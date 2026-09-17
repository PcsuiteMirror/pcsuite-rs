//! `pcsuite-ffi` — swift-bridge bindings over `pcsuite-core` for a SwiftUI macOS app.
//!
//! Design: **Swift only ever calls Rust** (no Rust→Swift delegates), which keeps
//! the boundary free of `Send`/threading hazards.
//! - `pcsuite_connect_usb` / `pcsuite_connect_lan` — blocking; return a `PcSession`.
//! - `start_screen()` → a `PcScreen`; poll `next_frame()` (blocking) on a background
//!   thread to pull raw HEVC frames (empty `Vec` = stream ended). No decode here —
//!   decode with VideoToolbox on the Swift side.
//! - `mouse/scroll/tap` — blocking input injection (need `start_screen()` first).
//! - `enable_verify()` then poll `next_verify_code()` (blocking; `"code\tsign"`,
//!   empty = session ended).
//! - `enable_clipboard()` — full text+image sync via a built-in macOS backend.
//!
//! A single global multi-threaded tokio runtime drives everything; the blocking
//! calls park the *calling* thread while the runtime's workers keep the session's
//! background tasks (control WS, video loop, clipboard relay) alive. Call the
//! blocking pollers (`next_frame`, `next_verify_code`) and the seconds-long
//! `connect_*` calls **off the main thread**.

// swift-bridge's generated glue (the `#[swift_bridge::bridge]` expansion) trips a
// couple of cosmetic lints; they're not in our hand-written code.
#![allow(clippy::unnecessary_cast, clippy::while_let_loop)]

use std::sync::OnceLock;

use tokio::runtime::Runtime;

use pcsuite_core::{
    cloud, config, pair, presence_once, register, usb, ClipboardConfig, ConnectAnswer, DeadReason,
    InputHandle,
    MouseAction, MouseButton, PhoneNotify, PresenceConfig, RegisterConfig, Registration,
    ScreenParams, ScreenStream, Session, UsbConfig,
};

mod clipboard_mac;
use clipboard_mac::MacClipboard;

// NOTE: keep this module free of `///` doc comments — the swift-bridge *build*
// parser (separate from the macro) rejects them. Use plain `//` here; the real
// docs live on the Rust impls below and in SWIFT_INTEGRATION.md.
#[swift_bridge::bridge]
mod ffi {
    extern "Rust" {
        type PcScreen;
        // Block until the next raw HEVC frame; empty Vec = stream ended OR stop()
        // was called. Loop this on a background thread.
        fn next_frame(&self) -> Vec<u8>;
        // Ask the frame pump to stop: the next (or in-flight) next_frame() returns
        // an empty Vec within ~300ms so the polling thread can break and release
        // this PcScreen. Lets Swift tear mirroring down without dropping the handle
        // out from under a parked next_frame() call.
        fn stop(&self);
        // Block for the next privacy / secure-screen event. Returns one of
        // "clear" (back to a normal screen), "password", "safety" (FLAG_SECURE,
        // e.g. fingerprint), or "lockScreen". Returns "" when the stream ends or
        // stop() is called. Loop this on a background thread: when the phone hits
        // a secure surface it stops the video, so show a "handle on phone" hint.
        fn next_privacy_event(&self) -> String;
        // Block for the next audio packet: one ADTS-framed AAC access unit exactly
        // as the phone sent it (7-byte ADTS header carrying sample rate + channel
        // count, then the AU). Only flows when start_screen was called with
        // audio=true. Empty Vec = stream ended OR stop() was called. Loop this on a
        // background thread and feed a decoder.
        fn next_audio_frame(&self) -> Vec<u8>;
        // Block for the next IME caret report from the phone, as "x,y" (caret
        // position in the mirror's pixel space) or "off" (focused field went
        // away). Returns "" when the stream ends / stop() is called. Use it to
        // place the PC's IME candidate window at the on-device caret.
        fn next_input_cursor(&self) -> String;
    }

    extern "Rust" {
        type PcSession;

        // Start screen mirroring; returns a PcScreen to poll. Also arms input.
        // `max_size` caps the longer screen edge (px): lower = lower resolution =
        // less encode/decode latency. Pass 0 for the full-resolution default.
        // `bit_rate` (bps) and `frame_rate` (fps) override the encoder; pass 0 to
        // let the phone pick its own default. `audio` requests the phone's audio
        // stream (no_audio=false) — the bytes are demuxed off the video path but
        // not yet decoded/played (see AudioProbe).
        // Drop the PcScreen (or call stop()) to stop mirroring.
        fn start_screen(
            &self,
            max_size: i64,
            bit_rate: i64,
            frame_rate: i64,
            audio: bool,
        ) -> Result<PcScreen, String>;
        // Enable clipboard sync (text + images), built-in macOS backend. Direction:
        // recv = apply the phone's clipboard to this Mac (phone→PC); send = push this
        // Mac's clipboard to the phone (PC→phone). Pass both true for bidirectional.
        fn enable_clipboard(&self, recv: bool, send: bool) -> Result<(), String>;
        // Gracefully disconnect before teardown: send the literal "close" on the
        // control WS, exactly as the official app does (ws.send("close")). The phone
        // runs its full PC-disconnect cleanup on it. REQUIRED for clean reconnects:
        // without it a bare socket close leaves stale phone-side state and phone→PC
        // clipboard sync silently dies on the next connect. Call on user-initiated
        // disconnect, before releasing the session. Blocking but fast (~200ms flush);
        // no-op if the link is already dead.
        fn stop_clipboard(&self);
        // Arm SMS verify-code relay; then poll next_verify_code().
        fn enable_verify(&self);
        // Block for the next SMS code as "code\tsign"; empty = session ended OR
        // stop_verify() was called.
        fn next_verify_code(&self) -> String;
        // Ask the verify poller to stop: the next (or in-flight) next_verify_code()
        // returns an empty String within ~300ms so its thread can break and release
        // this PcSession (mirror of PcScreen::stop for the verify-code loop).
        fn stop_verify(&self);

        // Arm phone notification relay; then poll next_notification().
        fn enable_notify(&self);
        // Block for the next phone notification, tab-separated as
        // "appName\ttitle\tcontent\tpackageName\tpendingIntentId"; empty = session
        // ended OR stop_notify() was called.
        fn next_notification(&self) -> String;
        // Ask the notify poller to stop (mirror of stop_verify for notifications).
        fn stop_notify(&self);
        // Block until the phone's connection center asks this PC to do something with
        // one of its function buttons, returning "<name>\t<msgId>" ("" = stopped /
        // session over). Names: openVivoScreen (start mirroring — the phone's 「投屏」),
        // closeVivoScreen. ALWAYS answer with reply_connect_center(name, msgId, …):
        // the phone's button waits on that reply.
        fn next_connect_center_request(&self) -> String;
        fn stop_connect_center(&self);
        // Answer a connection-center request; code 0 = done, anything else is a failure
        // whose reason the phone shows.
        fn reply_connect_center(&self, name: String, msg_id: String, code: i64, reason: String);

        // Upload local files to the phone (the desktop app's drag-and-drop path):
        // drop_files_info + a streamed tar over the 10380 HTTP gateway. `save_dir`
        // is the phone-side target directory ("" = the phone's default, observed
        // "Download/vivo办公套件/"); duplicates are renamed, never overwritten.
        // Directories are rejected (v1: regular files only). Blocks for the whole
        // transfer — call off the main thread. Returns the phone-side directory
        // the files landed in.
        fn push_files(&self, paths: Vec<String>, save_dir: String) -> Result<String, String>;
        // Arm the phone→PC receivers: (a) 快传 — the phone announces FILE_TRANS_TAG
        // on the shared control WS, we pull the batch over the mdfs HTTP plane and
        // ack on the same WS; (b) 互传 (EasyShare) — the phone connects to our
        // 10191 listener and we pull a zip off its HTTP server (no FILE_TRANS_TAG;
        // a bind failure on 10191 only disables this entry). Both feed the same
        // event stream. Received files are written into `save_dir` (created
        // if missing). Then poll next_file_transfer_event().
        fn enable_file_transfer(&self, save_dir: String) -> Result<(), String>;
        // Block for the next file-transfer event as a JSON object:
        //   {"type":"started"|"done"|"failed"|"cancelled", "files":[names],
        //    "dir": save_dir, "error": "…"}
        // Empty = session ended OR stop_file_transfer() was called.
        fn next_file_transfer_event(&self) -> String;
        // Ask the file-transfer poller to stop (mirror of stop_notify).
        fn stop_file_transfer(&self);

        // Fetch phone device facts (storage capacity, model, OS) via the 10380
        // /base-info gateway. Returns tab-separated fields, in order:
        //   name, brand, product, androidVersion, osVersion, widthPx, heightPx,
        //   fold(0/1), totalStorageGb, availableStorageGb, availableBytes, account,
        //   openId, mobileDeviceId
        // The trailing mobileDeviceId is the phone's stable unique device id (the
        // value /base-info routes on), so the app can key a per-device roster on it.
        // Blocking (one HTTP round-trip) — call off the main thread.
        fn device_info(&self) -> Result<String, String>;

        // Block until the connection is gone (the shared 10380 control WS — used by
        // both USB and LAN — closed or errored), returning why: "closed by phone"
        // when the phone ended the session on purpose (WS close frame / "close"
        // text — the user chose that, don't dial back), "connection lost" when the
        // link dropped (worth reconnecting). Returns "" if stop_watch() was called
        // first (intentional teardown). Run on a background thread; covers idle and
        // mid-mirror drops alike.
        fn wait_disconnect(&self) -> String;
        // Ask wait_disconnect() to return "" within ~300ms so its watcher thread can
        // break and release this PcSession before the handle is dropped.
        fn stop_watch(&self);

        // Inject a mouse event. action: 0=down,1=up,2=move. button: 1=left,2=right.
        // Coordinates are in your reference frame (w, h); the phone scales.
        fn mouse(&self, action: u8, button: u8, x: i64, y: i64, w: i64, h: i64) -> bool;
        // Inject a scroll. vscroll > 0 up, < 0 down.
        fn scroll(&self, vscroll: i64, x: i64, y: i64, w: i64, h: i64) -> bool;
        // Commit text into the phone's focused input field (IME commitText).
        // Carries full Unicode (Chinese/emoji) — the path for keyboard typing.
        // Needs start_screen() first (shares its /mirror/control channel).
        fn text(&self, s: String) -> bool;
        // Delete `before` chars before and `after` chars after the cursor
        // (IME deleteSurroundingText) — used for Backspace.
        fn delete_surrounding(&self, before: i64, after: i64) -> bool;
        // Tap (down+up, left button) at (x, y) in the reference frame (w, h).
        fn tap(&self, x: i64, y: i64, w: i64, h: i64) -> bool;
        // Press an Android key (down+up). keycode is a KEYCODE_* value, e.g.
        // BACK=4, HOME=3, APP_SWITCH=187. Drives the on-screen navigation keys.
        fn key(&self, keycode: i64) -> bool;
        // Move the phone's audio between the phone's own speaker and this PC while
        // mirroring — no stream restart. to_pc=true: the phone mutes itself and
        // starts sending AAC (poll next_audio_frame); false: it stops sending and
        // its speaker comes back. Needs start_screen() first (rides /mirror/control).
        fn set_audio_to_pc(&self, to_pc: bool) -> bool;
    }

    extern "Rust" {
        // Initialize tracing/logging (honours RUST_LOG). Safe to call once.
        fn pcsuite_log_init();
        // ABI version of this library.
        fn pcsuite_abi_version() -> u32;
        // Set the LAN pairing identity at runtime (empty field = leave default).
        // Call before pcsuite_connect_lan. USB ignores these.
        fn pcsuite_set_identity(open_id: String, pc_mac: String, account: String, device_name: String);
        // Set (empty seed = clear) the stored pairing seed for one phone IP, used by
        // the LAN connectType=2 path. Call before pcsuite_connect_lan.
        fn pcsuite_set_seed(phone_ip: String, seed: String);
        // Set (empty = default) the super-clipboard PC device id. Must match the id
        // the phone registered for this PC at pairing, or phone→PC clipboard won't
        // push. Used by both USB and LAN. Call before connecting.
        fn pcsuite_set_clip_id(clip_id: String);
        // Connect over USB (adb). Blocks a few seconds — call off the main thread.
        fn pcsuite_connect_usb() -> Result<PcSession, String>;
        // Is a phone on the USB cable? A bare `adb devices` that touches nothing
        // on the phone, for callers waiting on a cable rather than connecting.
        // Returns "ready" (connect can proceed), "no-device" (nothing attached —
        // keep waiting), "unauthorized" (attached, USB debugging not allowed yet)
        // or "no-adb" (adb missing/broken — waiting is hopeless). Blocks up to a
        // few seconds (adb may have to start its server): call off the main thread.
        fn pcsuite_usb_probe() -> String;
        // Connect over LAN/Tailscale. remote=true uses connectType=1 (no seed).
        fn pcsuite_connect_lan(phone_ip: String, remote: bool) -> Result<PcSession, String>;
        // Open the session with a token presence already registered (from
        // PcCloudPresence::take_connect_request) — no ConnectFlow of our own, so the
        // connection presence is holding stays up. Use this for a phone-initiated
        // connect; pcsuite_connect_lan is for one we start ourselves.
        fn pcsuite_connect_lan_token(phone_ip: String, token: String) -> Result<PcSession, String>;
        // Abort an in-flight pcsuite_connect_usb / pcsuite_connect_lan / PcPaired
        // connect() from another thread: it gives up its sockets and returns the
        // error "connect cancelled" right away instead of waiting out the network
        // timeouts (a remembered Wi-Fi IP that no longer answers takes ~30s).
        // Safe to call when nothing is connecting.
        fn pcsuite_cancel_connect();
        // Begin QR pairing (local ls=true variant — pure LAN, no cloud/seed/10191).
        // `lip` = this machine's LAN IP to advertise in the QR; pass "" to auto-detect.
        // Render qr_url() as a QR for the phone to scan via PCSuite 扫码连接电脑.
        fn pcsuite_pair_begin(lip: String) -> PcPairing;

        // Start/stop the LAN presence beacon: without it the phone's "find a
        // computer" search reports not-found, because it discovers PCs by
        // listening for this. Purely local — fine in either mode. Call after the
        // identity is set; calling start twice restarts with the current identity.
        fn pcsuite_presence_start() -> Result<(), String>;
        fn pcsuite_presence_stop();

        // App-lifetime 互传 (EasyShare) receiver on :10191, independent of any
        // session. The phone's 互传「我的设备」entry connects to *this PC's* 10191
        // when there is no pcsuite session (with one it hands the file to pcsuite,
        // which arrives as FILE_TRANS_TAG on that session instead) — so, like the
        // official VivoConnService, the listener must be up from launch, not only
        // while connected. Received files land in `save_dir`. Events (same JSON
        // shape as next_file_transfer_event) come from pcsuite_share_recv_next_event();
        // it returns "" once stopped. While this is running, enable_file_transfer()
        // does not arm its own per-session listener.
        fn pcsuite_share_recv_start(save_dir: String) -> Result<(), String>;
        fn pcsuite_share_recv_next_event() -> String;
        fn pcsuite_share_recv_stop();

        // ── vivo-account mode (opt-in; serverless never calls any of these) ──
        //
        // Select the identity mode: "serverless" (default — no server is ever
        // contacted) or "vivo_account". Anything unrecognised means serverless.
        // Call at startup, before connecting.
        fn pcsuite_set_mode(mode: String);
        // The active mode as a string, for the settings UI to read back.
        fn pcsuite_mode() -> String;
        // Hand the core the account credentials the QR login produced. Held in
        // memory only — the app owns persistence (keychain). Empty values sign out.
        fn pcsuite_cloud_set_account(open_id: String, token: String, country_code: String);
        // This PC's cloud device id, SHA256(platform UUID)+serial. Same value the
        // official client derives, so registering doesn't create a duplicate entry.
        // Returns "" if it can't be read.
        fn pcsuite_cloud_device_id() -> String;
        // The super-clipboard PC id implied by that device id (its first 6 hex
        // digits) — what pcsuite_set_clip_id should be given in account mode.
        fn pcsuite_cloud_clip_pc_id() -> String;
        // Register this PC with the connection center so the phone lists it.
        // Blocks on the network — call off the main thread. Returns the deviceId.
        fn pcsuite_cloud_register() -> Result<String, String>;
        // The account's devices, one per line, tab-separated:
        //   deviceId \t name \t model \t type \t reportTime \t ip \t externalId
        // `type` 3 = PC, anything else = a phone/pad. The API has no online flag,
        // so reportTime ("2026-08-11 10:59:09.374", or empty) is the only liveness
        // hint. Blocks — call off the main thread. Empty = no devices.
        fn pcsuite_cloud_devices() -> Result<String, String>;
        // Remove this PC from the account (the phone stops listing it).
        fn pcsuite_cloud_unregister() -> Result<String, String>;

        // ── 云传输 (cloud transfer) receiver ──
        //
        // The third way the phone sends files here, and the only one that is not
        // peer-to-peer: the phone uploads to vivo's relay and this PC downloads
        // later, so it works with the two never on the same network. Needs
        // account mode and a signed-in account; in serverless mode the poll loop
        // runs but makes no request.
        //
        // Files land in `save_dir`. Events use the same JSON shape as
        // pcsuite_share_recv_next_event, so the app can feed both into one
        // handler. `interval_secs` is the background re-check period; the
        // official client does not poll at all, so pick something lazy and call
        // pcsuite_cloud_recv_poll_now() at the moments that matter (launch,
        // sign-in, waking, connecting a phone).
        fn pcsuite_cloud_recv_start(save_dir: String, interval_secs: f64) -> Result<(), String>;
        fn pcsuite_cloud_recv_next_event() -> String;
        // Check the relay immediately instead of waiting out the interval.
        fn pcsuite_cloud_recv_poll_now();
        fn pcsuite_cloud_recv_stop();
    }

    extern "Rust" {
        type PcCloudPresence;
        // Hold a 10191 ConnectFlow connection to `phone_ip` open so the phone's
        // connection center lists this PC as discoverable ("可连"), reconnecting
        // automatically if it drops. This is the mechanism the official desktop
        // service uses; vpush / the cloud getUserCookie heartbeat / SSDP are all
        // unnecessary (each was disproven — see docs/LAN_DISCOVERY_HANDOFF.md).
        //
        // Prerequisites: a REAL businessId must be configured (via
        // pcsuite_set_identity's pc_mac, or the persisted config from
        // pcsuite_cloud_register) and it must match what this PC is registered
        // under, or the phone accepts the connection but never links it to the
        // listed device (stays 「未发现」). For connectType=2 set the per-IP seed
        // (pcsuite_set_seed) first; remote=true uses connectType=1 (no seed).
        //
        // Non-blocking: spawns a background task on the shared runtime and returns
        // at once. Poll status() for the UI; call stop() (or drop the handle) to
        // end and let the phone fall back to 「未发现」.
        fn pcsuite_cloud_presence_start(phone_ip: String, remote: bool) -> PcCloudPresence;
        // Current state, for the UI to poll: "connecting", "holding" (the phone
        // shows 「可连」), "reconnecting", "error: <why>", or "stopped".
        fn status(&self) -> String;
        // Poll-and-clear: after the phone tapped 「连接」 (it pushed bytes:[24] on the
        // held connection) this returns the token to open the session with — already
        // registered on that connection, so open 10380 with it directly via
        // pcsuite_connect_lan_token and do NOT register again (a second 10191 makes the
        // phone close the held one). Connect only, no mirror, like the official
        // desktop. Returns "" when nothing is pending.
        fn take_connect_request(&self) -> String;
        // Report how that session went (ret_code 0 = up, anything else = failed with
        // ret_msg), so presence can answer the phone on the still-held connection
        // (bytes:[27] {retCode,retMsg}) — the official service's last step. Two rules:
        // keep this handle alive until you have called it (stopping presence first
        // closes the connection the answer rides on), and call it promptly — the phone
        // drops its own request after ~5s and goes back to 「未发现」.
        fn report_connect_result(&self, ret_code: i64, ret_msg: String);
        // Tell presence that session has ended, so this PC stops being presented as
        // connected (it drops the connection it upgraded and re-holds a plain
        // discoverable one). Without this the phone keeps showing 「已连接」 with
        // function buttons that do nothing. Idempotent.
        fn report_session_ended(&self);
        // Upgrade the held connection for a connect WE start, returning the token to
        // open 10380 with (pcsuite_connect_lan_token); "" = nothing to upgrade, use
        // pcsuite_connect_lan instead. Pair it with report_session_ended when done.
        fn upgrade_for_connect(&self) -> String;
        // The phone address this hold is aimed at right now — it re-resolves itself when
        // the phone comes back on another address, so read it instead of remembering what
        // was passed to pcsuite_cloud_presence_start.
        fn phone_ip(&self) -> String;
        // Stop holding the connection (idempotent). The task ends and status
        // becomes "stopped"; the phone reverts to 「未发现」 on its next refresh.
        fn stop(&self);
    }

    extern "Rust" {
        type PcPairing;
        // The QR payload to render (the phone scans it; it carries a self-made token
        // the phone stores, plus ls=true + our lip so it reports back over the LAN).
        fn qr_url(&self) -> String;
        // The LAN IP chosen for the QR (where the phone POSTs notifyConnection).
        fn lan_ip(&self) -> String;
        // Block up to timeout_ms for the phone to scan and report its IP to :9199.
        // Returns a PcPaired (call connect() on it) or an error on timeout. Call off
        // the main thread.
        fn wait_phone(&self, timeout_ms: u32) -> Result<PcPaired, String>;
        // Abort an in-flight wait_phone() from another thread: it returns an error
        // within ~300ms and frees the :9199 listener so a re-pair can rebind.
        fn cancel(&self);
    }

    extern "Rust" {
        type PcPaired;
        // The phone's LAN IP (the data-plane host).
        fn phone_ip(&self) -> String;
        // The phone's device id (bleId, e.g. "52467a") — the clipboard routing id.
        fn ble_id(&self) -> String;
        // The phone's display name (e.g. "iQOO 15").
        fn device_name(&self) -> String;
        // The phone's logged-in account (masked, e.g. "173****991").
        fn vivo_account(&self) -> String;
        // "phone" or "pad".
        fn device_type(&self) -> String;
        // Open the data plane over the paired phone — the QR token is already trusted,
        // so this skips the 10191 sign. Blocks ~1s; returns a PcSession. Off-main.
        fn connect(&self) -> Result<PcSession, String>;
    }
}

// ───────────────────────────── runtime ─────────────────────────────

static RT: OnceLock<Runtime> = OnceLock::new();

fn rt() -> &'static Runtime {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    })
}

// ─────────────────────── connect cancellation ───────────────────────

/// Cancel signal for the blocking `pcsuite_connect_*` calls, bumped by
/// [`pcsuite_cancel_connect`].
///
/// Connecting is one long `block_on`, so without this the calling thread is
/// parked until the network gives up — up to ~30 s against a phone that no
/// longer owns the remembered IP. The app's "Cancel" would then do nothing
/// visible, and every queued call behind it (disconnect, a connect to another
/// device) would wait too. Selecting on this signal drops the connect future
/// instead, which aborts the in-flight sockets and returns at once.
static CONNECT_CANCEL: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();

fn connect_cancel() -> &'static tokio::sync::watch::Sender<u64> {
    CONNECT_CANCEL.get_or_init(|| tokio::sync::watch::channel(0u64).0)
}

/// Run a connect future on the shared runtime, aborting it as soon as
/// [`pcsuite_cancel_connect`] is called. Only cancels sent *after* this call
/// starts count, so a stale cancel can't kill the next connect.
fn block_on_cancellable<T>(
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> Result<T, String> {
    let mut cancel = connect_cancel().subscribe();
    cancel.borrow_and_update(); // ignore cancels that happened before now
    rt().block_on(async move {
        tokio::select! {
            r = fut => r.map_err(|e| format!("{e:#}")),
            _ = cancel.changed() => Err("connect cancelled".to_string()),
        }
    })
}

fn pcsuite_cancel_connect() {
    connect_cancel().send_modify(|gen| *gen += 1);
}

/// Local interface IP that routes toward `peer` (UDP connect picks the route).
fn local_ip_toward(peer: &str) -> Option<String> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect((peer, 9)).ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

// ───────────────────────────── handles ─────────────────────────────

/// A connected control session. All methods take `&self` (state is behind locks)
/// so the polling loops and one-off control calls can run on different threads.
pub struct PcSession {
    session: tokio::sync::Mutex<Session>,
    input: std::sync::Mutex<Option<InputHandle>>,
    verify_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    // Set by stop_verify(); the verify poller checks it between short recv timeouts.
    verify_stop: std::sync::atomic::AtomicBool,
    // Notification relay — mirror of the verify channel/stop pair.
    notify_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    notify_stop: std::sync::atomic::AtomicBool,
    // File-transfer receiver — mirror of the notify channel/stop pair.
    filetrans_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    filetrans_stop: std::sync::atomic::AtomicBool,
    // 互传 (EasyShare) 10191 listener, armed by enable_file_transfer() alongside the
    // 快传 receiver — both feed the same filetrans event channel. Dropped on stop /
    // session teardown (its Drop aborts the accept loop).
    share_recv: std::sync::Mutex<Option<pcsuite_core::ShareReceiver>>,
    // Cached phone mobileDeviceId for /base-info; resolved lazily without re-sending
    // a SHADOW startup when one is already on the wire (see resolve_device_id).
    device_id_cache: std::sync::Mutex<String>,
    // True once clipboard is enabled. While set, resolve_device_id() must NOT send its
    // own SHADOW startup (it would rotate the clipboard keys mid-handshake and break
    // phone→PC sync) — it waits for the clipboard handshake's retained reply instead.
    clipboard_active: std::sync::atomic::AtomicBool,
    // Phone connection-center requests (its function buttons: 「投屏」 …) arrive as text
    // on the shared control WS. Subscribed lazily on first poll; a broadcast receiver
    // only sees messages sent after it subscribes, which is fine — the phone sends these
    // when the user taps, long after connect.
    center_rx: tokio::sync::Mutex<Option<tokio::sync::broadcast::Receiver<String>>>,
    center_stop: std::sync::atomic::AtomicBool,
    // Liveness of the shared control WS; reads `Some(why)` once the connection is gone.
    dead_rx: tokio::sync::Mutex<tokio::sync::watch::Receiver<Option<DeadReason>>>,
    // Set by stop_watch(); the disconnect watcher checks it between short timeouts.
    watch_stop: std::sync::atomic::AtomicBool,
    token: String,
    data_ip: String,
    pc_ip: String,
    bind_addr: String,
    connect_type: String,
    vdfs_fetch_host: String,
    vdfs_fetch_port: u16,
    open_id: String,
    device_name: String,
    usb_adb: Option<String>,
    _reg: Option<Registration>,
}

/// A live screen-mirror stream. Poll [`PcScreen::next_frame`]; call [`PcScreen::stop`]
/// (or drop it) to stop.
pub struct PcScreen {
    frames: tokio::sync::Mutex<ScreenStream>,
    // Privacy/secure-screen events, polled on a thread separate from frames so
    // the two blocking pollers never contend for one lock.
    events: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<String>>,
    // IME caret reports, polled on its own thread for the same reason.
    cursor: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<String>>,
    // ADTS-framed AAC packets, polled on its own thread for the same reason.
    audio: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    // Set by stop(); the next_*() pollers observe it between short recv timeouts
    // and return empty so the polling threads break and drop this.
    stop: std::sync::atomic::AtomicBool,
}

impl PcScreen {
    fn next_frame(&self) -> Vec<u8> {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        rt().block_on(async {
            let mut frames = self.frames.lock().await;
            loop {
                if self.stop.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                // Short timeout so a stop() (or natural end) is noticed promptly even
                // when the phone screen is static and no frames are arriving.
                match tokio::time::timeout(Duration::from_millis(300), frames.next_frame()).await {
                    Ok(opt) => return opt.unwrap_or_default(),
                    Err(_) => continue,
                }
            }
        })
    }

    fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn next_privacy_event(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        rt().block_on(async {
            let mut events = self.events.lock().await;
            loop {
                if self.stop.load(Ordering::Relaxed) {
                    return String::new();
                }
                match tokio::time::timeout(Duration::from_millis(300), events.recv()).await {
                    Ok(Some(tok)) => return tok,    // a privacy state token
                    Ok(None) => return String::new(), // channel closed (stream ended)
                    Err(_) => continue,             // timeout -> re-check stop
                }
            }
        })
    }

    fn next_audio_frame(&self) -> Vec<u8> {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        rt().block_on(async {
            let mut audio = self.audio.lock().await;
            loop {
                if self.stop.load(Ordering::Relaxed) {
                    return Vec::new();
                }
                match tokio::time::timeout(Duration::from_millis(300), audio.recv()).await {
                    Ok(opt) => return opt.unwrap_or_default(),
                    Err(_) => continue, // silence (or audio off) -> re-check stop
                }
            }
        })
    }

    fn next_input_cursor(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        rt().block_on(async {
            let mut cursor = self.cursor.lock().await;
            loop {
                if self.stop.load(Ordering::Relaxed) {
                    return String::new();
                }
                match tokio::time::timeout(Duration::from_millis(300), cursor.recv()).await {
                    Ok(Some(s)) => return s,        // "x,y" or "off"
                    Ok(None) => return String::new(),
                    Err(_) => continue,
                }
            }
        })
    }
}

impl PcSession {
    fn start_screen(
        &self,
        max_size: i64,
        bit_rate: i64,
        frame_rate: i64,
        audio: bool,
    ) -> Result<PcScreen, String> {
        let mut params = ScreenParams::default();
        if max_size > 0 {
            params.max_size = max_size;
        }
        if bit_rate > 0 {
            params.bit_rate = bit_rate;
        }
        if frame_rate > 0 {
            params.frame_rate = frame_rate;
        }
        params.no_audio = !audio;
        let mut stream = rt()
            .block_on(async {
                let mut s = self.session.lock().await;
                s.enable_screen(params).await
            })
            .map_err(|e| format!("{e:#}"))?;
        *self.input.lock().unwrap() = stream.input();
        let events = stream.take_events();
        let cursor = stream.take_cursor();
        let audio_rx = stream.take_audio();
        Ok(PcScreen {
            frames: tokio::sync::Mutex::new(stream),
            events: tokio::sync::Mutex::new(events),
            cursor: tokio::sync::Mutex::new(cursor),
            audio: tokio::sync::Mutex::new(audio_rx),
            stop: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn enable_clipboard(&self, recv: bool, send: bool) -> Result<(), String> {
        if let Some(adb) = self.usb_adb.clone() {
            // USB: the phone reaches our relay/vdfs over adb reverse, and we reach
            // the phone's vdfs over adb forward.
            rt().block_on(async move {
                pcsuite_core::adb::run_ok(&adb, &["reverse", "tcp:8904", "tcp:8904"]).await;
                pcsuite_core::adb::run_ok(&adb, &["reverse", "tcp:5679", "tcp:5679"]).await;
                pcsuite_core::adb::run_ok(&adb, &["forward", "tcp:10382", "tcp:5678"]).await;
            });
        }
        let cfg = ClipboardConfig {
            data_ip: self.data_ip.clone(),
            pc_ip: self.pc_ip.clone(),
            bind_addr: self.bind_addr.clone(),
            token: self.token.clone(),
            open_id: self.resolve_clip_open_id(),
            device_name: self.device_name.clone(),
            connect_type: self.connect_type.clone(),
            vdfs_fetch_host: self.vdfs_fetch_host.clone(),
            vdfs_fetch_port: self.vdfs_fetch_port,
            recv_from_phone: recv,
            send_to_phone: send,
        };
        let r = rt().block_on(async {
            let mut s = self.session.lock().await;
            s.enable_clipboard(cfg, std::sync::Arc::new(MacClipboard)).await
        })
        .map_err(|e| format!("{e:#}"));
        if r.is_ok() {
            // From now on resolve_device_id() must reuse the clipboard handshake's
            // SHADOW reply, never send a competing startup (it rotates the clip keys).
            self.clipboard_active.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        r
    }

    fn stop_clipboard(&self) {
        rt().block_on(async {
            let s = self.session.lock().await;
            s.shutdown_clipboard().await;
        });
    }

    /// The account openId for the cowork clipboard `SHADOW_LIKE` handshake (the phone
    /// gates relay-clipboard on it matching its account). Prefer a configured value —
    /// including one learned from `/base-info` on an earlier connect and applied via
    /// `set_identity` — over the connect-time snapshot, so a re-pair after the openId
    /// was learned uses the real value. We deliberately do NOT fetch `/base-info` here:
    /// before the cowork handshake the phone answers it very slowly (~20s), which would
    /// stall `connect`. openId is learned post-connect instead (see `device_info` /
    /// the app's `autoFillOpenID`) and takes effect from the next connect.
    fn resolve_clip_open_id(&self) -> String {
        if config::has_open_id() {
            config::default_identity().open_id
        } else {
            self.open_id.clone()
        }
    }

    fn enable_verify(&self) {
        self.verify_stop.store(false, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *self.verify_rx.lock().unwrap() = Some(rx);
        rt().block_on(async {
            let mut s = self.session.lock().await;
            s.enable_verify(move |code, sign| {
                let _ = tx.send(format!("{code}\t{sign}"));
            });
        });
    }

    fn next_verify_code(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        // Take the receiver out so we don't hold the std lock across the blocking
        // recv, then put it back.
        let rx = self.verify_rx.lock().unwrap().take();
        let Some(mut rx) = rx else { return String::new() };
        let code = rt().block_on(async {
            loop {
                if self.verify_stop.load(Ordering::Relaxed) {
                    return None;
                }
                // Short timeout so stop_verify() (or session end) is noticed promptly.
                match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                    Ok(v) => return v, // Some(code) or None (channel closed)
                    Err(_) => continue,
                }
            }
        });
        *self.verify_rx.lock().unwrap() = Some(rx);
        code.unwrap_or_default()
    }

    fn stop_verify(&self) {
        self.verify_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn enable_notify(&self) {
        self.notify_stop.store(false, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *self.notify_rx.lock().unwrap() = Some(rx);
        rt().block_on(async {
            let mut s = self.session.lock().await;
            s.enable_notify(move |n| {
                let _ = tx.send(format!(
                    "{}\t{}\t{}\t{}\t{}",
                    n.app_name, n.title, n.content, n.package_name, n.pending_intent_id
                ));
            });
        });
    }

    fn next_notification(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        let rx = self.notify_rx.lock().unwrap().take();
        let Some(mut rx) = rx else { return String::new() };
        let msg = rt().block_on(async {
            loop {
                if self.notify_stop.load(Ordering::Relaxed) {
                    return None;
                }
                match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                    Ok(v) => return v, // Some(line) or None (channel closed)
                    Err(_) => continue,
                }
            }
        });
        *self.notify_rx.lock().unwrap() = Some(rx);
        msg.unwrap_or_default()
    }

    fn stop_notify(&self) {
        self.notify_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Block until the phone's connection center asks this PC to do something (its
    /// function buttons: 「投屏」 → `openVivoScreen`, closing it → `closeVivoScreen`),
    /// returning `"<name>\t<msgId>"`. Empty string once stopped or the session ended —
    /// same shape as `next_notification`, so the app pumps it the same way.
    ///
    /// Answer every one with `reply_connect_center` (same name + msgId): the phone's
    /// button waits on that reply.
    fn next_connect_center_request(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        let msg = rt().block_on(async {
            let mut guard = self.center_rx.lock().await;
            if guard.is_none() {
                *guard = Some(self.session.lock().await.control().subscribe());
            }
            let rx = guard.as_mut().expect("subscribed above");
            loop {
                if self.center_stop.load(Ordering::Relaxed) {
                    return None;
                }
                match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                    Ok(Ok(text)) => {
                        if let Some(req) = pcsuite_core::parse_connect_center(&text) {
                            return Some(format!("{}\t{}", req.name, req.msg_id));
                        }
                    }
                    // Lagged: the phone out-ran the buffer; keep listening.
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(_)) => return None, // control WS gone
                    Err(_) => continue,        // poll timeout
                }
            }
        });
        msg.unwrap_or_default()
    }

    fn stop_connect_center(&self) {
        self.center_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Answer a connection-center request (`code` 0 = done).
    fn reply_connect_center(&self, name: String, msg_id: String, code: i64, reason: String) {
        let _ = rt().block_on(async {
            self.session
                .lock()
                .await
                .reply_connect_center(&name, &msg_id, code, &reason)
                .await
        });
    }

    fn push_files(&self, paths: Vec<String>, save_dir: String) -> Result<String, String> {
        use anyhow::Context;
        if paths.is_empty() {
            return Err("nothing to upload".to_string());
        }
        let mut items = Vec::with_capacity(paths.len());
        for f in &paths {
            let p = std::path::Path::new(f);
            let md = std::fs::metadata(p).map_err(|e| format!("stat {f}: {e}"))?;
            if !md.is_file() {
                return Err(format!("{f}: 目录上传暂不支持，请只传普通文件"));
            }
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .with_context(|| format!("{f}: 无法取文件名"))
                .map_err(|e| format!("{e:#}"))?;
            let mtime_ms = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            items.push(pcsuite_core::mdfs::UploadItem {
                local: p.to_path_buf(),
                name,
                size: md.len(),
                mtime_ms,
            });
        }
        let device_id = self.resolve_device_id()?;
        rt().block_on(pcsuite_core::mdfs::upload_files(
            &self.data_ip,
            &self.token,
            &device_id,
            &save_dir,
            false, // ifDuplicated = rename
            &items,
        ))
        .map_err(|e| format!("{e:#}"))?;
        // upload_files doesn't report the resolved dir; give back what we asked
        // for, or the phone's observed default when it chose.
        Ok(if save_dir.is_empty() {
            "Download/vivo办公套件/".to_string()
        } else {
            save_dir
        })
    }

    fn enable_file_transfer(&self, save_dir: String) -> Result<(), String> {
        let device_id = self.resolve_device_id()?;
        self.filetrans_stop
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *self.filetrans_rx.lock().unwrap() = Some(rx);
        let cfg = pcsuite_core::FileTransConfig {
            data_ip: self.data_ip.clone(),
            token: self.token.clone(),
            device_id,
            save_dir: save_dir.clone(),
        };
        let tx2 = tx.clone();
        rt().block_on(async {
            let mut s = self.session.lock().await;
            s.enable_file_trans(cfg, move |ev| {
                let _ = tx.send(ev.to_json());
            });
            // 互传 (EasyShare): the phone connects to *our* 10191 and we pull a zip
            // over its HTTP server — no FILE_TRANS_TAG involved. When the app runs
            // the app-lifetime listener (pcsuite_share_recv_start) that one already
            // owns the port and its own event queue, so leave it alone; otherwise
            // (CLI-style use) arm a per-session one here so one switch covers both
            // receive entries. A bind failure (port taken by the official service /
            // a probe) only disables 互传: it is logged and 快传 keeps working.
            if share_recv_global_active() {
                tracing::info!("互传 listener: app-lifetime instance active, not arming per-session one");
            } else {
                let share_cfg = pcsuite_core::ShareConfig { save_dir };
                match pcsuite_core::ShareReceiver::start(share_cfg, move |ev| {
                    let _ = tx2.send(ev.to_json());
                })
                .await
                {
                    Ok(recv) => *self.share_recv.lock().unwrap() = Some(recv),
                    Err(e) => tracing::warn!(err = %format!("{e:#}"), "互传 receiver unavailable"),
                }
            }
        });
        Ok(())
    }

    fn next_file_transfer_event(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        let rx = self.filetrans_rx.lock().unwrap().take();
        let Some(mut rx) = rx else { return String::new() };
        let msg = rt().block_on(async {
            loop {
                if self.filetrans_stop.load(Ordering::Relaxed) {
                    return None;
                }
                match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                    Ok(v) => return v, // Some(json) or None (channel closed)
                    Err(_) => continue,
                }
            }
        });
        *self.filetrans_rx.lock().unwrap() = Some(rx);
        msg.unwrap_or_default()
    }

    fn stop_file_transfer(&self) {
        self.filetrans_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 互传 listener 一并停掉（Drop aborts the 10191 accept loop）。
        *self.share_recv.lock().unwrap() = None;
    }

    fn device_info(&self) -> Result<String, String> {
        let device_id = self.resolve_device_id()?;
        let info = rt()
            .block_on(pcsuite_core::device::fetch(&self.data_ip, &self.token, &device_id))
            .map_err(|e| format!("{e:#}"))?;
        Ok(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            info.mobile_device_name,
            info.mobile_brand,
            info.product,
            info.android_version,
            info.os_version,
            info.width_pixels,
            info.height_pixels,
            info.fold_screen as i32,
            info.total_storage_gb,
            info.available_storage_gb,
            info.available_bytes,
            info.vivo_account,
            info.open_id,
            device_id,
        ))
    }

    /// Resolve (and cache) the phone's `mobileDeviceId` for `/base-info`. Prefers a
    /// SHADOW reply already on the wire (reading it sends nothing, so it can't disturb
    /// an active clipboard session); only asks — a harmless `startup`, no `ready` — if
    /// nothing has announced yet.
    fn resolve_device_id(&self) -> Result<String, String> {
        {
            let c = self.device_id_cache.lock().unwrap();
            if !c.is_empty() {
                return Ok(c.clone());
            }
        }
        let clip_active = self.clipboard_active.load(std::sync::atomic::Ordering::Relaxed);
        let id = rt()
            .block_on(async {
                // Fast path: the (clipboard) handshake already retained the phone's
                // SHADOW reply.
                {
                    let s = self.session.lock().await;
                    if let Some(id) = s.known_device_id() {
                        return Ok::<String, anyhow::Error>(id);
                    }
                }
                // With clipboard active a startup is in flight; wait for its reply
                // rather than sending our own (a second startup rotates the clipboard
                // keys mid-handshake → phone→PC sync dies). Poll the retained reply
                // for as long as the handshake keeps re-sending its startup (plus a
                // second for the last answer to land), so a phone that only answers
                // the second or third startup still yields the device id.
                if clip_active {
                    let window = pcsuite_core::clipboard::SHADOW_REPLY_WAIT
                        * (pcsuite_core::clipboard::SHADOW_STARTUP_RESENDS + 1)
                        + std::time::Duration::from_secs(1);
                    let polls = window.as_millis() / 100;
                    for _ in 0..polls {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let s = self.session.lock().await;
                        if let Some(id) = s.known_device_id() {
                            return Ok(id);
                        }
                    }
                    anyhow::bail!(
                        "device id unavailable: clipboard handshake produced no SHADOW reply within {}s, startup re-sent {} times (not sending a competing startup, which would rotate the clipboard keys)",
                        window.as_secs(),
                        pcsuite_core::clipboard::SHADOW_STARTUP_RESENDS
                    );
                }
                // No clipboard in flight (e.g. browse-only) → safe to ask ourselves.
                let s = self.session.lock().await;
                let identity = config::default_identity();
                let p = s.phone_info(&self.pc_ip, &self.connect_type, &identity).await?;
                Ok(p.mobile_device_id)
            })
            .map_err(|e| format!("{e:#}"))?;
        *self.device_id_cache.lock().unwrap() = id.clone();
        Ok(id)
    }

    fn wait_disconnect(&self) -> String {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        fn describe(reason: DeadReason) -> String {
            match reason {
                DeadReason::Closed => "closed by phone",
                DeadReason::Lost => "connection lost",
            }
            .to_string()
        }
        rt().block_on(async {
            let mut rx = self.dead_rx.lock().await;
            loop {
                if self.watch_stop.load(Ordering::Relaxed) {
                    return String::new(); // intentional teardown
                }
                if let Some(reason) = *rx.borrow() {
                    return describe(reason);
                }
                // Short timeout so stop_watch() is noticed promptly even while the
                // connection is still healthy and the signal hasn't changed.
                match tokio::time::timeout(Duration::from_millis(300), rx.changed()).await {
                    Ok(Ok(())) => {
                        if let Some(reason) = *rx.borrow() {
                            return describe(reason);
                        }
                    }
                    // Sender dropped without setting a reason — the session was
                    // dropped out from under us; treat as a (benign) link loss.
                    Ok(Err(_)) => return describe(DeadReason::Lost),
                    Err(_) => continue, // timeout -> re-check the stop flag
                }
            }
        })
    }

    fn stop_watch(&self) {
        self.watch_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn mouse(&self, action: u8, button: u8, x: i64, y: i64, w: i64, h: i64) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        let a = match action {
            0 => MouseAction::Down,
            1 => MouseAction::Up,
            _ => MouseAction::Move,
        };
        let b = match button {
            2 => MouseButton::Right,
            _ => MouseButton::Left,
        };
        rt().block_on(input.mouse(a, b, x, y, w, h)).is_ok()
    }

    fn scroll(&self, vscroll: i64, x: i64, y: i64, w: i64, h: i64) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        rt().block_on(input.scroll(vscroll, x, y, w, h)).is_ok()
    }

    fn text(&self, s: String) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        rt().block_on(input.text(&s)).is_ok()
    }

    fn delete_surrounding(&self, before: i64, after: i64) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        rt().block_on(input.delete_surrounding(before, after)).is_ok()
    }

    fn tap(&self, x: i64, y: i64, w: i64, h: i64) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        rt().block_on(input.tap(x, y, w, h)).is_ok()
    }

    fn key(&self, keycode: i64) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        rt().block_on(input.key(keycode)).is_ok()
    }

    fn set_audio_to_pc(&self, to_pc: bool) -> bool {
        let Some(input) = self.input.lock().unwrap().clone() else {
            return false;
        };
        rt().block_on(input.set_audio_to_pc(to_pc)).is_ok()
    }
}

impl Drop for PcSession {
    fn drop(&mut self) {
        // Remove the USB forwards we added; Session/Registration drop afterwards,
        // aborting all background tasks and stopping SSDP presence.
        if let Some(adb) = self.usb_adb.clone() {
            rt().block_on(async move {
                pcsuite_core::adb::run_ok(&adb, &["reverse", "--remove", "tcp:8904"]).await;
                pcsuite_core::adb::run_ok(&adb, &["reverse", "--remove", "tcp:5679"]).await;
                pcsuite_core::adb::run_ok(&adb, &["forward", "--remove", "tcp:10382"]).await;
                usb::cleanup(&adb).await;
            });
        }
    }
}

// ───────────────────────────── free functions ─────────────────────────────

fn pcsuite_log_init() {
    use std::io::IsTerminal;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        // stderr, like the app's own log lines (the fmt default is stdout), so
        // one redirect of fd 2 captures both. Colour only for a terminal: the
        // app may have pointed stderr at a log file, and escape codes make
        // that unreadable.
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

fn pcsuite_abi_version() -> u32 {
    1
}

fn pcsuite_set_identity(open_id: String, pc_mac: String, account: String, device_name: String) {
    config::set_identity(open_id, pc_mac, account, device_name);
}

fn pcsuite_set_seed(phone_ip: String, seed: String) {
    config::set_seed(phone_ip, seed);
}

fn pcsuite_set_clip_id(clip_id: String) {
    config::set_clip_pc_id(clip_id);
}

fn presence_slot() -> &'static std::sync::Mutex<Option<pcsuite_core::PresenceBeacon>> {
    static P: OnceLock<std::sync::Mutex<Option<pcsuite_core::PresenceBeacon>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn pcsuite_presence_start() -> Result<(), String> {
    let _guard = rt().enter(); // presence::start spawns onto the shared runtime
    let beacon = pcsuite_core::presence::start().map_err(|e| format!("{e:#}"))?;
    *presence_slot().lock().unwrap() = Some(beacon); // dropping any previous one stops it
    Ok(())
}

fn pcsuite_presence_stop() {
    presence_slot().lock().unwrap().take();
}

// ── App-lifetime 互传 (EasyShare) listener on :10191 ──
//
// Same three-piece shape as the per-session file-transfer receiver (listener,
// event queue, stop flag), just global: the phone's 互传「我的设备」entry connects
// to our 10191 whenever there is *no* pcsuite session, so the listener has to
// outlive sessions.

fn share_recv_slot() -> &'static std::sync::Mutex<Option<pcsuite_core::ShareReceiver>> {
    static S: OnceLock<std::sync::Mutex<Option<pcsuite_core::ShareReceiver>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn share_recv_rx() -> &'static std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>> {
    static R: OnceLock<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>> =
        OnceLock::new();
    R.get_or_init(Default::default)
}

fn share_recv_stop_flag() -> &'static std::sync::atomic::AtomicBool {
    static F: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &F
}

/// True while the app-lifetime listener owns :10191 — enable_file_transfer()
/// then leaves 互传 to it instead of failing a second bind.
fn share_recv_global_active() -> bool {
    share_recv_slot().lock().unwrap().is_some()
}

fn pcsuite_share_recv_start(save_dir: String) -> Result<(), String> {
    // Drop any previous instance first so the port is free for the new bind.
    share_recv_slot().lock().unwrap().take();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let cfg = pcsuite_core::ShareConfig { save_dir };
    let recv = rt()
        .block_on(pcsuite_core::ShareReceiver::start(cfg, move |ev| {
            let _ = tx.send(ev.to_json());
        }))
        .map_err(|e| format!("{e:#}"))?;
    share_recv_stop_flag().store(false, std::sync::atomic::Ordering::Relaxed);
    *share_recv_rx().lock().unwrap() = Some(rx);
    *share_recv_slot().lock().unwrap() = Some(recv);
    Ok(())
}

/// Block for the next 互传 event as JSON; "" once stopped (or never started).
/// Mirrors PcSession::next_file_transfer_event: the receiver is taken out for
/// the wait and put back afterwards, polling in short slices so stop() is
/// noticed promptly.
fn pcsuite_share_recv_next_event() -> String {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let rx = share_recv_rx().lock().unwrap().take();
    let Some(mut rx) = rx else { return String::new() };
    let msg = rt().block_on(async {
        loop {
            if share_recv_stop_flag().load(Ordering::Relaxed) {
                return None;
            }
            match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                Ok(v) => return v, // Some(json) or None (listener dropped → sender gone)
                Err(_) => continue,
            }
        }
    });
    *share_recv_rx().lock().unwrap() = Some(rx);
    msg.unwrap_or_default()
}

fn pcsuite_share_recv_stop() {
    share_recv_stop_flag().store(true, std::sync::atomic::Ordering::Relaxed);
    share_recv_slot().lock().unwrap().take(); // Drop aborts the accept loop
}

// ── 10191 hold-presence (makes the phone list this PC as 「可连」) ──

type AnswerSlot = std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<ConnectAnswer>>>>;

pub struct PcCloudPresence {
    status: std::sync::Arc<std::sync::Mutex<String>>,
    /// Set to the session token when the phone asks this PC to connect; the app takes
    /// it and opens 10380 with it.
    connect_requested: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Set while the phone's ask-connect is awaiting its outcome; taking it sends the
    /// `bytes:[27]` `{retCode,retMsg}` answer on the still-held connection.
    answer: AnswerSlot,
    /// Fired by `report_session_ended`: tells presence that the session it upgraded this
    /// connection for is over, so it drops it and goes back to a plain discoverable hold.
    session_ended: std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// Our *own* connects go through here: presence upgrades the connection it is
    /// holding and hands back the token (see `upgrade_for_connect`).
    upgrade_tx: tokio::sync::mpsc::Sender<pcsuite_core::UpgradeRequest>,
    /// The address this hold is currently aimed at. It changes on its own when the phone
    /// comes back on a different one, so the app reads it rather than remembering what it
    /// passed in (it decides whether a connect can ride the hold by comparing addresses).
    current_ip: std::sync::Arc<std::sync::Mutex<String>>,
    stop_tx: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl PcCloudPresence {
    fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
    /// Poll-and-clear: the token to open the session with, or "" when nothing is
    /// pending. The token is already registered on the held connection, so the app must
    /// open 10380 with it directly (`pcsuite_connect_lan_token`) and NOT register again.
    fn take_connect_request(&self) -> String {
        self.connect_requested.lock().unwrap().take().unwrap_or_default()
    }
    /// Report how the session the phone asked for went, so presence can answer it on
    /// the held connection (`retCode` 0 = up). The phone gives up in ~5s, so call this
    /// as soon as the connect resolves — and keep this handle alive until then, or the
    /// answer is never sent and the phone falls back to 「未发现」.
    fn report_connect_result(&self, ret_code: i64, ret_msg: String) {
        match self.answer.lock().unwrap().take() {
            Some(tx) => {
                // On success hand presence a channel for "that session is over", so it
                // can stop presenting this PC as connected once the session goes away.
                let ended = if ret_code == 0 {
                    let (etx, erx) = tokio::sync::oneshot::channel();
                    *self.session_ended.lock().unwrap() = Some(etx);
                    Some(erx)
                } else {
                    None
                };
                let _ = tx.send(ConnectAnswer { ret_code, ret_msg, ended });
            }
            None => tracing::warn!("presence: connect result reported with none pending"),
        }
    }

    /// The phone address this hold is aimed at right now (it re-resolves itself when the
    /// phone moves).
    fn phone_ip(&self) -> String {
        self.current_ip.lock().unwrap().clone()
    }

    /// Ask presence to turn the connection it is holding into a formal connect for a
    /// connect **we** initiated, returning the token to open 10380 with
    /// (`pcsuite_connect_lan_token`). Empty string = no hold to upgrade (or it failed);
    /// fall back to `pcsuite_connect_lan`, which registers on its own connection.
    ///
    /// Going through the hold is what keeps the phone showing this PC as connected: a
    /// second 10191 makes it close the held one.
    fn upgrade_for_connect(&self) -> String {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let (ended_tx, ended_rx) = tokio::sync::oneshot::channel();
        let req = pcsuite_core::UpgradeRequest { reply: reply_tx, ended: ended_rx };
        let token = rt().block_on(async {
            if self.upgrade_tx.send(req).await.is_err() {
                return None;   // presence task is gone
            }
            match tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx).await {
                Ok(Ok(Ok(t))) => Some(t),
                Ok(Ok(Err(e))) => {
                    tracing::info!(err = %e, "presence: in-place upgrade failed");
                    None
                }
                _ => None,
            }
        });
        match token {
            Some(t) => {
                // The session rides the hold now: remember how to end it.
                *self.session_ended.lock().unwrap() = Some(ended_tx);
                t
            }
            None => String::new(),
        }
    }

    /// Tell presence the session the phone asked for has ended (disconnected, dropped, or
    /// the user ended it here). Presence then drops the connection it upgraded and
    /// re-holds a fresh pre-connect, so the phone goes back to 「可连」 instead of showing
    /// this PC as connected with dead function buttons. Idempotent.
    fn report_session_ended(&self) {
        if let Some(tx) = self.session_ended.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
    fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}

impl Drop for PcCloudPresence {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(true);
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

fn pcsuite_cloud_presence_start(phone_ip: String, remote: bool) -> PcCloudPresence {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let status = Arc::new(Mutex::new("connecting".to_string()));
    let connect_requested: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let answer: AnswerSlot = Arc::new(Mutex::new(None));
    let session_ended = Arc::new(Mutex::new(None));
    let current_ip = Arc::new(Mutex::new(phone_ip.clone()));
    let (upgrade_tx, mut upgrade_rx) = tokio::sync::mpsc::channel::<pcsuite_core::UpgradeRequest>(1);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    // The LAN sign must carry the account's openId (the phone rejects a mismatch
    // with bytes:[28]). If only the cloud account has it (identity still on the
    // placeholder), feed it into the identity so the sign verifies.
    if config::default_identity().open_id == config::OPEN_ID_PLACEHOLDER {
        let oid = cloud_account().read().unwrap().open_id.clone();
        if !oid.is_empty() {
            config::set_open_id(oid);
        }
    }
    let identity = config::default_identity();
    // Guard the placeholder businessId here too — holding a connection under it
    // just wastes effort (the phone can't link it to the device → 「未发现」).
    if config::is_pc_mac_placeholder(&identity.pc_mac) {
        *status.lock().unwrap() =
            "error: no businessId (call pcsuite_set_identity / pcsuite_cloud_register first)".into();
        return PcCloudPresence {
            status,
            connect_requested,
            answer,
            session_ended,
            upgrade_tx,
            current_ip,
            stop_tx,
            task: None,
        };
    }
    let st = status.clone();
    let req_flag = connect_requested.clone();
    let answer_slot = answer.clone();
    let ip_slot = current_ip.clone();
    let _guard = rt().enter();
    let task = rt().spawn(async move {
        let mut stop_rx = stop_rx;
        let stored_seed = resolve_stored_seed(&phone_ip, remote).await;
        // Remember which phone this hold is for, so a retry can ask the connection center
        // where it is *now* (see `refresh_target`).
        let mut phone_name: Option<String> = None;
        let mut cfg = PresenceConfig {
            phone_ip,
            identity,
            stored_seed,
            remote,
            // Hold exactly like the official pre-connect: device_info exchange only.
            // Announcing a connect here leaves the phone with a 「正在连接 …」
            // notification it can never finish.
            connect_frame_on_hold: false,
            // Turn the held connection into a formal connect in place when the phone
            // asks: never a second 10191, which the phone answers by closing this one.
            reregister_on_ask: true,
        };

        let mut backoff = Duration::from_secs(1);
        let mut refresh = false;   // the first attempt uses the address we were handed
        loop {
            if *stop_rx.borrow() {
                break;
            }
            *st.lock().unwrap() = "connecting".into();
            // A phone that was pocketed / left the Wi-Fi usually comes back on a different
            // address — and its connectType=2 seed is per-address — so re-ask the
            // connection center before each retry instead of dialing the address this hold
            // started with forever. Best-effort: on any error keep the current target.
            if refresh {
                match cloud_center() {
                    Ok(cc) => match cc.phone_lan_target(phone_name.as_deref()).await {
                        Ok(t) => {
                            if t.ip != cfg.phone_ip {
                                tracing::info!(
                                    from = %cfg.phone_ip,
                                    to = %t.ip,
                                    "presence: phone moved → new address"
                                );
                            }
                            phone_name = Some(t.name);
                            cfg.phone_ip = t.ip;
                            *ip_slot.lock().unwrap() = cfg.phone_ip.clone();
                            if !cfg.remote {
                                // A configured seed still wins (explicit override).
                                cfg.stored_seed =
                                    config::default_stored_seed(&cfg.phone_ip).or(t.seed);
                            }
                        }
                        Err(e) => tracing::info!(err = %format!("{e:#}"), "presence: target refresh"),
                    },
                    Err(e) => tracing::info!(err = %format!("{e:#}"), "presence: no connect center"),
                }
            }
            refresh = true;   // every attempt after the first re-checks the address
            let st_ready = st.clone();
            let req = req_flag.clone();
            let ans = answer_slot.clone();
            let once = presence_once(
                &cfg,
                move || {
                    *st_ready.lock().unwrap() = "holding".into();
                },
                move |token: &str| {
                    // Park the token and the answer channel for the app: it opens the
                    // session with this token (no registration of its own — that would
                    // open a second 10191 and the phone would close the held one), then
                    // calls report_connect_result, which presence reports to the phone
                    // as bytes:[27]. Without that answer the phone gives up on its own
                    // request after ~5s and shows 「未发现」 again.
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    *ans.lock().unwrap() = Some(tx);
                    *req.lock().unwrap() = Some(token.to_string());
                    rx
                },
                Some(&mut upgrade_rx),
            );
            tokio::select! {
                // The hold only ends when the phone closes it (or on error) — an
                // ask-connect is handled inside, connection kept.
                r = once => match r {
                    Ok(()) => {
                        *st.lock().unwrap() = "reconnecting".into();
                        backoff = Duration::from_secs(1);
                    }
                    // Not reachable (pocketed, Wi-Fi off, moved): keep retrying. Say
                    // "waiting" rather than "error" — the hold is not broken, the phone
                    // is simply away, and the next attempt re-checks its address.
                    Err(e) => {
                        tracing::info!(err = %format!("{e:#}"), "presence: not reachable, will retry");
                        *st.lock().unwrap() = "waiting".into();
                    }
                },
                _ = stop_rx.changed() => break,
            }
            if *stop_rx.borrow() {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = stop_rx.changed() => break,
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
        *st.lock().unwrap() = "stopped".into();
    });

    PcCloudPresence {
        status,
        connect_requested,
        answer,
        session_ended,
        upgrade_tx,
        current_ip,
        stop_tx,
        task: Some(task),
    }
}

// ───────────────────── vivo-account mode (opt-in) ─────────────────────
//
// The account lives here rather than in `config` because the app owns
// persistence (keychain) and only hands the credentials over per launch.

fn cloud_account() -> &'static std::sync::RwLock<cloud::Account> {
    static A: OnceLock<std::sync::RwLock<cloud::Account>> = OnceLock::new();
    A.get_or_init(Default::default)
}

fn pcsuite_set_mode(mode: String) {
    config::set_mode(config::Mode::parse(&mode));
}

fn pcsuite_mode() -> String {
    config::mode().as_str().to_string()
}

fn pcsuite_cloud_set_account(open_id: String, token: String, country_code: String) {
    let mut a = cloud_account().write().unwrap();
    a.open_id = open_id;
    a.token = token;
    a.country_code = if country_code.is_empty() { "cn".into() } else { country_code };
}

fn pcsuite_cloud_device_id() -> String {
    cloud::pc_device_id().unwrap_or_default()
}

fn pcsuite_cloud_clip_pc_id() -> String {
    cloud::derived_clip_pc_id().unwrap_or_default()
}

/// Refuse cloud calls unless the user actually chose account mode — a stale
/// account in memory must never cause a request in serverless mode.
fn cloud_center() -> Result<cloud::ConnectCenter, String> {
    if !config::mode().uses_cloud() {
        return Err("not in vivo-account mode".into());
    }
    let account = cloud_account().read().unwrap().clone();
    cloud::ConnectCenter::new(account).map_err(|e| e.to_string())
}

fn pcsuite_cloud_register() -> Result<String, String> {
    let cc = cloud_center()?;
    rt().block_on(cc.register_self()).map_err(|e| format!("{e:#}"))?;
    Ok(cc.device_id().to_string())
}

fn pcsuite_cloud_devices() -> Result<String, String> {
    let cc = cloud_center()?;
    let list = rt().block_on(cc.device_list()).map_err(|e| format!("{e:#}"))?;
    Ok(list
        .iter()
        .map(|d| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                d.device_id,
                d.name,
                d.model,
                d.device_type,
                d.report_time,
                d.ip().unwrap_or(""),
                d.external_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn pcsuite_cloud_unregister() -> Result<String, String> {
    let cc = cloud_center()?;
    rt().block_on(cc.unbind()).map_err(|e| format!("{e:#}"))?;
    Ok(cc.device_id().to_string())
}

// ── 云传输 (cloud transfer) receiver ──
//
// Unlike the 互传 listener above there is no socket to own: receiving is a poll
// of the vendor's relay. So this is a background task rather than a listener,
// with the same three-piece shape (task slot, event queue, stop flag) plus a
// notify the app can ring to make the next check happen now.

fn cloud_recv_slot() -> &'static std::sync::Mutex<Option<tokio::task::JoinHandle<()>>> {
    static S: OnceLock<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn cloud_recv_rx() -> &'static std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>
{
    static R: OnceLock<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>> =
        OnceLock::new();
    R.get_or_init(Default::default)
}

fn cloud_recv_stop_flag() -> &'static std::sync::atomic::AtomicBool {
    static F: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &F
}

/// Rung by [`pcsuite_cloud_recv_poll_now`] to cut the wait short.
fn cloud_recv_wake() -> &'static tokio::sync::Notify {
    static N: OnceLock<tokio::sync::Notify> = OnceLock::new();
    N.get_or_init(Default::default)
}

/// One pass over the relay. Silent when there is nothing to do — including when
/// the user is in serverless mode or not signed in, which is the normal state
/// for most users and must not produce an error event on every tick.
///
/// Returns false when the account was not configured *yet*, which at launch is a
/// race rather than a decision: the poller is armed from `AppModel.init()` but
/// the credentials arrive from the app delegate, so the first pass can easily
/// run against an empty account. The caller retries soon instead of sleeping out
/// the full interval, otherwise a transfer that is already waiting sits there
/// until the next tick.
async fn cloud_recv_once(save_dir: &str, tx: &tokio::sync::mpsc::UnboundedSender<String>) -> bool {
    if !config::mode().uses_cloud() {
        // A deliberate choice, not a race — nothing to wait for.
        return true;
    }
    let account = cloud_account().read().unwrap().clone();
    if !account.is_complete() {
        return false;
    }
    let client = match pcsuite_core::cloudshare::CloudShare::new(account) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "云传输: client unavailable");
            return true;
        }
    };

    let tx = tx.clone();
    let forward = move |ev: pcsuite_core::cloudshare::CloudEvent| {
        if let Some(json) = ev.to_file_trans_json() {
            let _ = tx.send(json);
        }
    };
    if let Err(e) = client.receive_pending(save_dir, &forward).await {
        // A failed *query* (offline, expired token) is not a transfer failure —
        // log it rather than telling the user a file could not be received.
        tracing::debug!(error = %format!("{e:#}"), "云传输: poll failed");
    }
    true
}

fn pcsuite_cloud_recv_start(save_dir: String, interval_secs: f64) -> Result<(), String> {
    // Drop any previous poller before starting a new one.
    if let Some(h) = cloud_recv_slot().lock().unwrap().take() {
        h.abort();
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    cloud_recv_stop_flag().store(false, std::sync::atomic::Ordering::Relaxed);
    *cloud_recv_rx().lock().unwrap() = Some(rx);

    // Floor is a sanity guard only — the caller picks the real cadence.
    let interval = std::time::Duration::from_secs_f64(interval_secs.max(5.0));
    // While the app is still installing the account, come back quickly.
    let not_ready_retry = std::time::Duration::from_secs(2);
    let handle = rt().spawn(async move {
        loop {
            let ready = cloud_recv_once(&save_dir, &tx).await;
            let wait = if ready { interval } else { not_ready_retry };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = cloud_recv_wake().notified() => {}
            }
        }
    });
    *cloud_recv_slot().lock().unwrap() = Some(handle);
    Ok(())
}

/// Block for the next 云传输 event as JSON; "" once stopped (or never started).
/// Same take-out/put-back dance as [`pcsuite_share_recv_next_event`].
fn pcsuite_cloud_recv_next_event() -> String {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let rx = cloud_recv_rx().lock().unwrap().take();
    let Some(mut rx) = rx else { return String::new() };
    let msg = rt().block_on(async {
        loop {
            if cloud_recv_stop_flag().load(Ordering::Relaxed) {
                return None;
            }
            match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
                Ok(v) => return v,
                Err(_) => continue,
            }
        }
    });
    *cloud_recv_rx().lock().unwrap() = Some(rx);
    msg.unwrap_or_default()
}

fn pcsuite_cloud_recv_poll_now() {
    cloud_recv_wake().notify_one();
}

fn pcsuite_cloud_recv_stop() {
    cloud_recv_stop_flag().store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(h) = cloud_recv_slot().lock().unwrap().take() {
        h.abort();
    }
}

/// Cheap USB cable check — see [`pcsuite_core::usb::probe`]. Not cancellable: it
/// runs one short `adb devices` and never opens a socket.
fn pcsuite_usb_probe() -> String {
    rt().block_on(usb::probe(None)).as_str().to_string()
}

fn pcsuite_connect_usb() -> Result<PcSession, String> {
    let id = config::default_identity();
    let (u, session) = block_on_cancellable(async {
        let u = usb::prepare(UsbConfig {
            pc_name: Some(id.device_name.clone()),
            ..UsbConfig::default()
        })
        .await?;
        // Best-effort, bounded: the official pre-WS `/version` + full `/base-info`
        // (the same pair the LAN path sends). It must land *before* the WS comes
        // up: it carries our display name, which the phone freezes into its
        // "已连接" notification then, and `isLogin`/`openid`/`pcSystemType="2"`,
        // without which the phone's connection center doesn't count this as a
        // connection — it keeps offering 「连接」 and routes phone→PC file shares
        // through 云传输 instead of this session. Time-boxed so it can never
        // stall the connect.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pcsuite_core::pre_ws_probe("127.0.0.1", &u.token, &u.conn_id),
        )
        .await;
        let session = Session::connect("127.0.0.1", &u.token).await?;
        Ok::<_, anyhow::Error>((u, session))
    })?;
    let dead_rx = session.dead_signal();
    Ok(PcSession {
        session: tokio::sync::Mutex::new(session),
        input: std::sync::Mutex::new(None),
        verify_rx: std::sync::Mutex::new(None),
        verify_stop: std::sync::atomic::AtomicBool::new(false),
        notify_rx: std::sync::Mutex::new(None),
        notify_stop: std::sync::atomic::AtomicBool::new(false),
        filetrans_rx: std::sync::Mutex::new(None),
        filetrans_stop: std::sync::atomic::AtomicBool::new(false),
        share_recv: std::sync::Mutex::new(None),
        device_id_cache: std::sync::Mutex::new(String::new()),
        clipboard_active: std::sync::atomic::AtomicBool::new(false),
        center_rx: tokio::sync::Mutex::new(None),
        center_stop: std::sync::atomic::AtomicBool::new(false),
        dead_rx: tokio::sync::Mutex::new(dead_rx),
        watch_stop: std::sync::atomic::AtomicBool::new(false),
        token: u.token,
        data_ip: "127.0.0.1".into(),
        pc_ip: "127.0.0.1".into(),
        bind_addr: "127.0.0.1".into(),
        connect_type: "USB".into(),
        vdfs_fetch_host: "127.0.0.1".into(),
        vdfs_fetch_port: 10382,
        open_id: id.open_id,
        device_name: id.device_name,
        usb_adb: Some(u.adb),
        _reg: None,
    })
}

/// Resolve the `connectType=2` (WLAN) stored seed for one phone IP: a configured
/// per-IP seed wins, else the phone's own published `ext.seeds` from the connection
/// center — so the app never has to plumb a seed through itself.
///
/// Shared by the presence hold and the LAN connect on purpose: the phone does not
/// treat the two ConnectFlow types as interchangeable. `connectType=2` is a nearby
/// WLAN connect; `connectType=1` is the seedless FARAWAYWLAN (remote) path. Falling
/// back to `remote` just because no seed was configured would register this PC as a
/// far-away device on its own LAN. `remote = true` (the caller asked for it) needs
/// no seed.
async fn resolve_stored_seed(phone_ip: &str, remote: bool) -> Option<String> {
    if remote {
        return None;
    }
    if let Some(s) = config::default_stored_seed(phone_ip) {
        return Some(s);
    }
    let cc = cloud_center().ok()?;
    let list = cc.device_list().await.ok()?;
    list.iter().find(|d| d.is_phone()).and_then(|d| {
        d.seeds
            .get(phone_ip)
            .or_else(|| d.seeds.values().next())
            .cloned()
    })
}

/// Open the 10380 session with a token that is **already registered** — the one
/// `PcCloudPresence::take_connect_request` handed over after the phone asked this PC to
/// connect. No ConnectFlow of our own: presence registered that token on the connection
/// it is still holding, and opening a second 10191 would make the phone close that one
/// (measured: within ~12ms), which also drops the phone's view of this PC to 「未发现」.
fn pcsuite_connect_lan_token(phone_ip: String, token: String) -> Result<PcSession, String> {
    let session = block_on_cancellable(async {
        // Official parity: /version + /base-info before the WS.
        pcsuite_core::pre_ws_probe(&phone_ip, &token, "pcsuite_ask_connect").await;
        Ok::<_, anyhow::Error>(Session::connect(&phone_ip, &token).await?)
    })?;
    Ok(build_wlan_session(session, token, phone_ip, None))
}

fn pcsuite_connect_lan(phone_ip: String, remote: bool) -> Result<PcSession, String> {
    let (reg, token, session) = block_on_cancellable(async {
        // `remote = false` means "prefer the nearby WLAN connect": resolve a seed
        // (config, else the account's device list) and only fall back to the seedless
        // connectType=1 when there genuinely is none — registering with no seed would
        // otherwise just fail. The caller no longer has to know whether a seed exists.
        let stored_seed = resolve_stored_seed(&phone_ip, remote).await;
        let remote = remote || stored_seed.is_none();
        tracing::info!(
            phone = %phone_ip,
            connect_type = if remote { 1 } else { 2 },
            "LAN connect"
        );
        let reg = register(RegisterConfig {
            reg_ip: phone_ip.clone(),
            identity: config::default_identity(),
            stored_seed,
            remote,
            token: None,
            conn_id: None,
            presence: true,
        })
        .await?;
        let token = reg.token.clone();
        let session = Session::connect(&phone_ip, &token).await?;
        Ok::<_, anyhow::Error>((reg, token, session))
    })?;
    Ok(build_wlan_session(session, token, phone_ip, Some(reg)))
}

/// Build a WLAN-flavoured [`PcSession`] from an already-connected control session.
/// Shared by `pcsuite_connect_lan` (with its SSDP-presence `Registration`) and the
/// QR-pairing `connect()` (no registration — the QR delivered the token).
fn build_wlan_session(
    session: Session,
    token: String,
    phone_ip: String,
    reg: Option<Registration>,
) -> PcSession {
    let id = config::default_identity();
    let pc_ip = local_ip_toward(&phone_ip).unwrap_or_else(|| "0.0.0.0".into());
    let dead_rx = session.dead_signal();
    PcSession {
        session: tokio::sync::Mutex::new(session),
        input: std::sync::Mutex::new(None),
        verify_rx: std::sync::Mutex::new(None),
        verify_stop: std::sync::atomic::AtomicBool::new(false),
        notify_rx: std::sync::Mutex::new(None),
        notify_stop: std::sync::atomic::AtomicBool::new(false),
        filetrans_rx: std::sync::Mutex::new(None),
        filetrans_stop: std::sync::atomic::AtomicBool::new(false),
        share_recv: std::sync::Mutex::new(None),
        device_id_cache: std::sync::Mutex::new(String::new()),
        clipboard_active: std::sync::atomic::AtomicBool::new(false),
        center_rx: tokio::sync::Mutex::new(None),
        center_stop: std::sync::atomic::AtomicBool::new(false),
        dead_rx: tokio::sync::Mutex::new(dead_rx),
        watch_stop: std::sync::atomic::AtomicBool::new(false),
        token,
        data_ip: phone_ip.clone(),
        pc_ip,
        bind_addr: "0.0.0.0".into(),
        connect_type: "WLAN".into(),
        vdfs_fetch_host: phone_ip,
        vdfs_fetch_port: 5678,
        open_id: id.open_id,
        device_name: id.device_name,
        usb_adb: None,
        _reg: reg,
    }
}

// ───────────────────────────── QR pairing ─────────────────────────────

/// This machine's LAN IPv4 (en0/en1/en2), skipping Tailscale (100.x). macOS path via
/// `ipconfig getifaddr`; falls back to the route toward a private LAN address.
fn local_lan_ip() -> Option<String> {
    for iface in ["en0", "en1", "en2"] {
        if let Ok(out) = std::process::Command::new("ipconfig")
            .args(["getifaddr", iface])
            .output()
        {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ip.is_empty() && !ip.starts_with("100.") {
                return Some(ip);
            }
        }
    }
    local_ip_toward("192.168.0.1").filter(|ip| !ip.starts_with("100."))
}

/// In-progress QR pairing: the self-made token + the QR payload to show. Holds no
/// network state — the listener only runs while [`PcPairing::wait_phone`] is awaited.
pub struct PcPairing {
    token: String,
    url: String,
    lip: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn pcsuite_pair_begin(lip: String) -> PcPairing {
    let id = config::default_identity();
    let lip = if lip.trim().is_empty() {
        local_lan_ip().unwrap_or_else(|| "0.0.0.0".into())
    } else {
        lip
    };
    let token = pair::random_token();
    let url = pair::qr_url(&token, &lip, &id);
    PcPairing {
        token,
        url,
        lip,
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

impl PcPairing {
    fn qr_url(&self) -> String {
        self.url.clone()
    }

    fn lan_ip(&self) -> String {
        self.lip.clone()
    }

    fn wait_phone(&self, timeout_ms: u32) -> Result<PcPaired, String> {
        let dur = std::time::Duration::from_millis(timeout_ms as u64);
        let notify = rt()
            .block_on(pair::wait_for_phone_until(pair::NOTIFY_PORT, dur, &self.stop))
            .map_err(|e| format!("{e:#}"))?;
        Ok(PcPaired {
            token: self.token.clone(),
            notify,
        })
    }

    fn cancel(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A phone that scanned the QR and reported its IP. Call [`PcPaired::connect`] to
/// open the data plane.
pub struct PcPaired {
    token: String,
    notify: PhoneNotify,
}

impl PcPaired {
    fn phone_ip(&self) -> String {
        self.notify.phone_ip.clone()
    }
    fn ble_id(&self) -> String {
        self.notify.ble_id.clone()
    }
    fn device_name(&self) -> String {
        self.notify.device_name.clone()
    }
    fn vivo_account(&self) -> String {
        self.notify.vivo_account.clone()
    }
    fn device_type(&self) -> String {
        self.notify.device_type.clone()
    }

    fn connect(&self) -> Result<PcSession, String> {
        let phone_ip = self.notify.phone_ip.clone();
        let token = self.token.clone();
        let session = block_on_cancellable(Session::connect(&phone_ip, &token))?;
        Ok(build_wlan_session(session, token, phone_ip, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The cancel signal is process-global (only one connect runs at a time in the
    /// app), so these tests must not overlap.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A cancel unblocks the calling thread immediately, instead of leaving it
    /// parked for the whole network timeout — the app's "Cancel" button.
    #[test]
    fn cancel_aborts_a_blocking_connect() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = started.clone();
        let t = std::thread::spawn(move || {
            let began = Instant::now();
            let r: Result<(), String> = block_on_cancellable(async move {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(60)).await; // stands in for a dead IP
                Ok(())
            });
            (r, began.elapsed())
        });
        // Wait for the connect to actually be in flight, then cancel it.
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        pcsuite_cancel_connect();
        let (r, elapsed) = t.join().expect("connect thread panicked");
        assert_eq!(r.unwrap_err(), "connect cancelled");
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }

    /// A cancel that arrives before a connect starts must not kill it.
    #[test]
    fn stale_cancel_does_not_affect_the_next_connect() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        pcsuite_cancel_connect();
        let r: Result<u8, String> = block_on_cancellable(async { Ok(7) });
        assert_eq!(r.unwrap(), 7);
    }
}
