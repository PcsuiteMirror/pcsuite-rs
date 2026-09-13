//! vivo 互传 (EasyShare) phone→PC receiver on TCP 10191.
//!
//! Distinct from the「快传」receiver in [`crate::filetrans`] (which rides the
//! 10380 control WS + mdfs tar): here the **phone connects to us**. Our SSDP
//! presence already advertises `com.vivo.share.CONNECT_PC`
//! ([`crate::config::SERVICE_RECORD`]), so when the user picks this Mac in the
//! phone's 互传「我的设备」list, the phone opens a TCP connection to :10191 and
//! speaks the ConnectFlow framing (`PAY_LOAD_1` + BE32 len + JSON, see
//! [`pcsuite_proto::payload1`]) — but with **no seed/sign check**:
//!
//!   1. phone → PC: connect frame `{"type":1,"service_id":"com.vivo.share.CONNECT_PC",
//!      "deviceName":"…","extra_info":"{\"port\":\"8080\",…}",…}`. `extra_info`
//!      is stringified JSON whose `ipArr` member is NOT valid JSON (unquoted
//!      IPs) — so only `port` is scraped out textually; the phone IP comes from
//!      the TCP peer address instead.
//!   2. PC → phone: same framing, `bytes:[1]` (= accept), shell fields mirroring
//!      the request, identity from [`crate::config::default_identity`].
//!   3. **PC opens `ws://<phone>:<port>/websocket`** (subprotocol
//!      `v1.vs.vivo.com.cn`) and runs the v1 exchange — this is the whole ball
//!      game, see below.
//!   4. The phone then closes the 10191 connection; the session is over.
//!
//! # Why the WebSocket is mandatory
//!
//! An earlier version of this module skipped the WS and just pulled
//! `GET /status?status=0` + `GET /file_download?name=pcsuit` over plain HTTP.
//! That is protocol **version 0**, and on a current phone it silently does
//! nothing: `AsyncService.A0` (postTaskToClient) reads
//!
//! ```text
//! if (K.f() != -17) { if (K.f() != 1 || ws == null) return; ws.sendAction("sendRequest", task); }
//! ```
//!
//! `K.f()` is the negotiated version and only becomes `1` after a successful WS
//! `versionNegotiation`. Without it the phone never announces the task, waits,
//! and gives up with「发送失败」. (It could still be coaxed into streaming via the
//! legacy path, which is why it appeared to work once — not a stable entry.)
//!
//! The exchange, byte-for-byte as the official desktop client does it (found in
//! its Electron bundle, `analysis/asar-src/dist/electron/main.js`, `[vivoshare]`):
//!
//! | phone → PC | PC → phone |
//! |---|---|
//! | `action:0:versionNegotiation?{"versions":[1]}` | `ack:0:versionNegotiation?{"version":1,"threadLimit":5,"device_avatar_uri":""}` |
//! | `action:1:sendRequest?{…,"id":"<taskId>",…}` | `ack:1:sendRequest?{"versions":[1]}` |
//! | (streams the zip) | `GET /download?taskId=<id>&name=pcsuit&vtime=true` |
//! | `ack:2:status` | `action:2:status?{"taskId":"<id>","type":"1"}` then close |
//!
//! Traps worth keeping in mind:
//! - JSON keys come from Gson `@SerializedName` and do **not** match the field
//!   names: `supportedVersions` → `versions`, `selectedVersion` → `version`.
//! - The `sendRequest` ack **must carry a body**; a bare `ack:1:sendRequest`
//!   makes the phone drop the WS on the spot.
//! - Every action must be answered within 5s (`BaseWebSocket` TIMEOUT).
//! - `FileController` demands `taskId` once the version is 1, and only treats
//!   the batch as a PC transfer when `name=pcsuit` — both live in the same
//!   `K.f() == 1` branch.
//! - The phone's share server only exists for the duration of a task and flaps:
//!   observed accepting at t+0, gone at t+0.4s, genuinely up at t+13s (once
//!   t+35s). [`grab_ws`] polls instead of assuming.
//! - The official client uses `https://` for the file routes; plain HTTP works
//!   too (`Server: VS-HTTP`). **Do not route these through an HTTP proxy** — a
//!   `502` with an empty body is a proxy talking, not the phone.
//!
//! # ★ Timing is the whole game (2026-09-12)
//!
//! The phone brings its share server up **before** it dials our 10191, and the
//! moment a WebSocket attaches it pushes `versionNegotiation` and expects an ack
//! almost immediately. Answer the connect frame first and connect afterwards —
//! what this module used to do — and the ~200ms spent reading and replying to
//! the 10191 frame is enough for the phone to give up: the WS is closed
//! (`early eof`), the share server is torn down within about a second, and every
//! later poll gets `Connection refused`.
//!
//! So [`handle_conn`] races: the instant the TCP connection is accepted it spawns
//! the **entire** WS session (hunt → negotiate → pull the zip) and only then
//! reads and answers the 10191 frame. Both halves run concurrently and are
//! independent — on a successful run the file is on disk *before* `bytes:[1]`
//! goes out:
//!
//! ```text
//! t+0.000  phone connected
//! t+0.017  WS up (attempts=1)
//! t+0.041  task announced  count=1 bytes=469542
//! t+0.112  saved Screenshot_….jpg      ← 469542 bytes, Exif intact
//! t+0.206  connect frame accepted      ← bytes:[1] only now
//! ```
//!
//! This was mis-diagnosed several times before the raw-socket probe settled it,
//! so for the record, all three of these are **false**: the phone does open 8080
//! (it just lives ~1s); the old code was not too slow to find it (attempt #1
//! connected every time); and the WS upgrade is not rejected (a clean
//! `101 Switching Protocols` with `Sec-WebSocket-Protocol: v1.vs.vivo.com.cn`
//! comes back, followed by the negotiation frame).
//!
//! # Which entry the phone picks
//!
//! 互传「我的设备」→ this Mac reaches us two different ways, and both now work:
//!
//! - **This module (8080)** — when the phone classifies the Mac as ShareDevice
//!   type 4, i.e. discovered over the LAN by our SSDP beacon
//!   ([`crate::presence`]). Verified on-device with and without a live session.
//! - **快传 ([`crate::filetrans`])** — when the entry came from the phone's own
//!   pcsuite (ShareDevice type 6, the「云传输」badge): `VivoShareServicePool`
//!   runnable `t` calls `sendFilesByPCSuite()`, so the batch arrives as
//!   `FILE_TRANS_TAG` on the 10380 control WS instead.
//!
//! The numeric types name a discovery channel, not an OS — the phone's own
//! `DeviceCache` merge logs them as `PC_BLE` (3) and `PC_SUITE` (6). Registering
//! the PC in the cloud as Windows rather than Mac changes nothing (tested).
//! Note the merge in `DeviceCache.u()` keeps a cached type-6 entry and discards
//! an incoming type-4 one, so a stale pcsuite entry can pin the phone to the
//! 快传 route until VivoShare's process restarts.
//!
//! Verified on-device (iQOO 15 → Mac, `re_vcs/share_probe4.py` and the CLI):
//! screenshots and an 8.8MB photo arrive intact and the phone reports「发送成功」.
//!
//! Lifecycle: one global listener, owned by the returned [`ShareReceiver`];
//! dropping it stops the accept loop. If :10191 is already taken (official
//! VivoConnService, a stale probe, …) [`ShareReceiver::start`] fails with a
//! clear log instead of panicking — the rest of the session keeps working.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use pcsuite_net::ws::{WsClient, WsFrame};
use pcsuite_proto::payload1;

use crate::config;
use crate::filetrans::FileTransEvent;

/// The 互传 listen port (same port the phone uses for pcsuite ConnectFlow when
/// *we* are the client — that direction is outbound-only, so no conflict).
pub const SHARE_PORT: u16 = 10191;

/// Service id of the 互传 connect frame (protocol-mandated wire constant).
pub const SHARE_SERVICE_ID: &str = "com.vivo.share.CONNECT_PC";

/// The phone's share HTTP/WS port. Every observed connect frame names 8080; the
/// frame's own value still wins if it ever differs.
const DEFAULT_SHARE_PORT: u16 = 8080;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Idle cap on any single socket read (a stalled phone shouldn't hang forever).
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// WS route/subprotocol the phone's share server exposes (`HttpConst.Router.WS`
/// + `HttpConst.WSSubProtocol.V1`).
const WS_PATH: &str = "/websocket";
const WS_SUBPROTOCOL: &str = "v1.vs.vivo.com.cn";
/// How long to keep hunting for the phone's (flapping) share server.
const WS_WINDOW: Duration = Duration::from_secs(90);
/// Poll fast: on a phone running the 2026-08-28 image the share server can live
/// for barely a second (it is started *before* the phone re-checks whether a PC
/// is already connected, and torn down again when that check rejects the
/// transfer), so a lazy poll misses the window entirely.
const WS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
/// Time allowed for the phone's connect frame after TCP accept.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// The connect JSON is small; refuse to buffer more than this while framing.
const MAX_FRAME_BUF: usize = 1 << 20;

/// Where received files go.
pub struct ShareConfig {
    /// Local directory received files are written into (created if missing).
    pub save_dir: String,
}

/// Owns the 10191 accept loop. Drop to stop (the task is aborted).
pub struct ShareReceiver {
    task: JoinHandle<()>,
}

impl Drop for ShareReceiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ShareReceiver {
    /// Bind `0.0.0.0:10191` and spawn the accept loop. `on_event` reports each
    /// batch step with the same [`FileTransEvent`] stream the 快传 receiver
    /// uses, so frontends need no second channel.
    pub async fn start<F>(cfg: ShareConfig, on_event: F) -> Result<ShareReceiver>
    where
        F: Fn(FileTransEvent) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("0.0.0.0", SHARE_PORT)).await.with_context(|| {
            format!(
                "bind 0.0.0.0:{SHARE_PORT} failed — 互传接收不可用（端口被占用？官方 \
                 VivoConnService / 探针脚本 / 上一个实例还在跑？）"
            )
        })?;
        tracing::info!(port = SHARE_PORT, dir = %cfg.save_dir, "互传 (EasyShare) receiver armed");
        sweep_partials(&cfg.save_dir);
        let task = tokio::spawn(accept_loop(listener, cfg, on_event));
        Ok(ShareReceiver { task })
    }
}

/// Drop `.vivoshare-*.zip.part` leftovers in `dir`. A superseded session is
/// aborted mid-download, so its own cleanup never runs — without this the
/// user's download folder slowly fills with half-written archives.
fn sweep_partials(dir: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for name in entries.flatten().map(|e| e.file_name()) {
        let name = name.to_string_lossy();
        if name.starts_with(".vivoshare-") && name.ends_with(".zip.part") {
            let path = format!("{dir}/{name}");
            if std::fs::remove_file(&path).is_ok() {
                tracing::debug!(path = %path, "互传: swept a stale partial");
            }
        }
    }
}

async fn accept_loop<F>(listener: TcpListener, cfg: ShareConfig, on_event: F)
where
    F: Fn(FileTransEvent) + Send + Sync + 'static,
{
    let cfg = std::sync::Arc::new(cfg);
    let on_event = std::sync::Arc::new(on_event);
    // Exactly one session may be live. The phone opens a *fresh* 10191 connection
    // per tap, while a failed session's [`grab_ws`] keeps hunting for up to
    // `WS_WINDOW`. Left alone those stale pollers steal the next task's
    // WebSocket — observed on-device as two `WS up` in the same millisecond
    // followed by "early eof" / "EOF inside chunked stream" as they fight over
    // it, with the phone reporting「发送失败」even though a file had landed.
    // A new connection means the user tapped again, so: newest tap wins.
    let mut current: Option<(JoinHandle<()>, std::sync::Arc<AtomicBool>)> = None;
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                tracing::info!(%peer, "互传: phone connected");
                // Supersede a previous session unless it is already downloading —
                // an impatient extra tap must not kill a transfer in flight.
                let busy = match current.take() {
                    Some((handle, committed)) if !handle.is_finished() => {
                        if committed.load(Ordering::SeqCst) {
                            tracing::info!(
                                "互传: a download is already in flight — accepting this \
                                 connection but not starting a second session"
                            );
                            current = Some((handle, committed));
                            true
                        } else {
                            tracing::info!("互传: superseding the previous session");
                            handle.abort();
                            false
                        }
                    }
                    _ => false,
                };
                let cfg = cfg.clone();
                let on_event = on_event.clone();
                let committed = std::sync::Arc::new(AtomicBool::new(false));
                let flag = committed.clone();
                let handle = tokio::spawn(async move {
                    if let Err(e) =
                        handle_conn(sock, &peer.ip().to_string(), &cfg, on_event.clone(), flag, !busy)
                            .await
                    {
                        tracing::warn!(%peer, err = %format!("{e:#}"), "互传: session failed");
                    }
                });
                if !busy {
                    current = Some((handle, committed));
                }
            }
            Err(e) => {
                tracing::warn!(err = %e, "互传: accept failed");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// A spawned task that is aborted when the guard drops, unless [`take`n](Self::take)
/// out first. Dropping a bare `JoinHandle` only detaches the task; this makes an
/// early return actually cancel it.
struct AbortOnDrop<T>(Option<JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    /// Remove the handle so it survives the guard (the caller now owns its
    /// lifetime — e.g. to `.await` it).
    fn take(&mut self) -> Option<JoinHandle<T>> {
        self.0.take()
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(h) = &self.0 {
            h.abort();
        }
    }
}

/// One 互传 session: connect frame → reply → WS v1 exchange → zip → done.
/// `committed` is flipped once the phone has announced a task and the download
/// is underway, so [`accept_loop`] knows this session must not be superseded.
/// `run_session` is false when another session already owns a live download: we
/// still accept the connection (leaving the phone hanging is worse) but do not
/// start a second, competing WebSocket hunt.
async fn handle_conn<F>(
    mut sock: TcpStream,
    phone_ip: &str,
    cfg: &ShareConfig,
    on_event: std::sync::Arc<F>,
    committed: std::sync::Arc<AtomicBool>,
    run_session: bool,
) -> Result<()>
where
    F: Fn(FileTransEvent) + Send + Sync + 'static,
{
    // ★ Start hunting the phone's share server *now*, before we even read the
    // connect frame. On the phone's 2026-08-28 image the server is already up by
    // the time it dials our 10191, and it pushes `versionNegotiation` the moment
    // a WebSocket attaches — then tears the whole task down a fraction of a
    // second after it processes our `bytes:[1]`. Answering first and connecting
    // afterwards (what this module used to do) loses the race by ~200ms and only
    // ever sees `Connection refused`; racing in at accept time gets a clean 101.
    // The port is 8080 in every observed frame; a frame naming a different one
    // falls back to a fresh hunt below.
    // The task must run the *whole* WS session, not just the connect: the phone
    // pushes `versionNegotiation` within ~2ms of the socket attaching and closes
    // it again if that goes unanswered while we are still busy replying on 10191.
    // Abort-on-drop: if this fn returns before taking the handle back out — the
    // connect frame never arrives (a port probe, a stray local connection that
    // closes at once, as `peer closed before connect frame`), or the frame's
    // service_id is wrong — the hunt must stop with it. A bare JoinHandle only
    // *detaches* on drop, so without this the poller would keep hammering the
    // phone's 8080 every WS_POLL_INTERVAL for the whole 90s WS_WINDOW (~900
    // `share server not reachable yet` lines) for a connection that was never a
    // transfer.
    let mut session = AbortOnDrop(run_session.then(|| {
        let ip = phone_ip.to_string();
        let save_dir = cfg.save_dir.clone();
        let ev = on_event.clone();
        let committed = committed.clone();
        tokio::spawn(async move {
            let ws = grab_ws(&ip, DEFAULT_SHARE_PORT).await?;
            pull_batch(
                ws,
                &ip,
                DEFAULT_SHARE_PORT,
                &save_dir,
                &committed,
                ev.as_ref(),
            )
            .await
        })
    }));

    let body = read_first_frame(&mut sock).await?;
    let v: Value = serde_json::from_slice(&body).context("互传 connect frame was not JSON")?;
    let service_id = v.get("service_id").and_then(Value::as_str).unwrap_or("");
    if service_id != SHARE_SERVICE_ID {
        // Defensive: pcsuite.SERVICE frames belong to the client-side ConnectFlow;
        // a phone never sends them here. Close instead of answering.
        bail!("unexpected service_id {service_id:?} (not {SHARE_SERVICE_ID}) — closing");
    }
    let device_name = v
        .get("deviceName")
        .and_then(Value::as_str)
        .unwrap_or("手机")
        .to_string();
    let port = v
        .get("extra_info")
        .and_then(Value::as_str)
        .map(parse_extra_port)
        .unwrap_or(8080);
    let frame_id = v.get("id").and_then(Value::as_i64).unwrap_or(config::FRAME_ID);
    let extra_raw = v.get("extra_info").and_then(Value::as_str).unwrap_or("");
    tracing::info!(device = %device_name, http_port = port, extra_info = %extra_raw, "互传: connect frame accepted");
    // NOTE: no `Started` event here. Answering this frame does **not** mean a
    // transfer is coming: since the phone's 2026-08-28 system update the same
    // handshake is emitted by ConnBase merely to set up the link, and on a Mac
    // target the files then travel over 快传 instead (see the module docs). We
    // only announce a batch once the phone's WS actually hands us a task.

    // Reply `bytes:[1]` (= accept), shell mirroring the request, identity ours.
    let id = config::default_identity();
    let reply = json!({
        "channel": 0,
        "id": frame_id,
        "open_Id": id.open_id,
        "target_id": id.pc_mac,
        "account": id.account,
        "type": 1,
        "service_id": SHARE_SERVICE_ID,
        "bytes": [1],
        "deviceName": id.device_name,
    });
    let accept_frame = payload1::encode_json(&reply)?;

    if !run_session {
        sock.write_all(&accept_frame).await.context("send accept frame")?;
        sock.flush().await.ok();
        tracing::info!("互传: connection accepted but left idle (another transfer owns the phone)");
        return Ok(());
    }

    if port != DEFAULT_SHARE_PORT {
        tracing::warn!(
            port,
            "互传: frame named a non-default share port; the session raced {DEFAULT_SHARE_PORT}"
        );
    }

    // ★ Send `bytes:[1]` only once the transfer is done. On the phone's current
    // build this answer *ends* the share session: the WS is closed within tens of
    // milliseconds of it arriving (observed 9ms with a raw probe, 59ms here).
    // Small files never noticed because the zip had already landed by then —
    // in a successful run `saved` is logged *before* this frame goes out — but a
    // 157MB pull was still streaming and died with `early eof` / `EOF inside
    // chunked stream`, and the phone's device row showed「发送失败」.
    let outcome = match session.take() {
        Some(handle) => handle.await.context("互传: session task did not finish")?,
        None => {
            sock.write_all(&accept_frame).await.context("send accept frame")?;
            sock.flush().await.ok();
            return Ok(());
        }
    };
    sock.write_all(&accept_frame).await.context("send accept frame")?;
    sock.flush().await.ok();
    tracing::info!("互传: connect frame answered (bytes:[1]) after the transfer");
    match outcome {
        Ok(saved) => {
            on_event(FileTransEvent::Done {
                files: saved,
                dir: cfg.save_dir.clone(),
            });
        }
        Err(e) => {
            let error = format!("{e:#}");
            tracing::warn!(err = %error, "互传: batch failed");
            on_event(FileTransEvent::Failed {
                files: vec![device_name],
                error,
            });
        }
    }
    // The phone closes the 10191 connection itself once done; just drop ours.
    Ok(())
}

/// Buffer reads until the first complete `PAY_LOAD_1` frame arrives (handles
/// half frames and extra trailing bytes; only the first frame is used).
async fn read_first_frame(sock: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 8192];
    loop {
        match payload1::decode_strict(&buf) {
            Ok(Some((body, _))) => return Ok(body),
            Ok(None) => {}
            Err(e) => return Err(e).context("互传 frame magic mismatch"),
        }
        if buf.len() > MAX_FRAME_BUF {
            bail!("connect frame exceeds {MAX_FRAME_BUF} bytes");
        }
        let n = tokio::time::timeout(FRAME_TIMEOUT, sock.read(&mut tmp))
            .await
            .context("timeout waiting for 互传 connect frame")?
            .context("read connect frame")?;
        if n == 0 {
            bail!("peer closed before connect frame");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

/// Scrape `"port":"…"` (or `"port":N`) out of the `extra_info` string. The
/// string is JSON-*like* but its `ipArr` member holds unquoted IPs
/// (`"ipArr":"[192.168.1.203]"` …), so a full JSON parse is not reliable.
/// Falls back to 8080 when absent/unparseable.
fn parse_extra_port(extra: &str) -> u16 {
    let Some(kpos) = extra.find("\"port\"") else { return 8080 };
    let rest = &extra[kpos + 6..];
    let Some(cpos) = rest.find(':') else { return 8080 };
    let val = rest[cpos + 1..].trim_start();
    let digits: String = val
        .trim_start_matches('"')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap_or(8080)
}

/// Grab the phone's `/websocket` and run the v1 exchange to completion.
/// Returns the basenames actually written.
async fn pull_batch<F>(
    mut ws: WsClient<TcpStream>,
    phone_ip: &str,
    port: u16,
    save_dir: &str,
    committed: &AtomicBool,
    on_event: &F,
) -> Result<Vec<String>>
where
    F: Fn(FileTransEvent) + Send + Sync + 'static,
{
    std::fs::create_dir_all(save_dir).with_context(|| format!("mkdir {save_dir}"))?;
    let mut saved: Option<Result<Vec<String>>> = None;
    // The zip pull runs as its own task so the loop below keeps answering the
    // phone while it streams; `pending_task` is the taskId to report against.
    let mut download: Option<JoinHandle<Result<Vec<String>>>> = None;
    let mut pending_task: Option<String> = None;

    loop {
        let frame = tokio::select! {
            // Finished pulling: report the outcome the way the official client
            // does, then stop. Checked first so a completed download is not made
            // to wait on the next WS frame.
            res = async { download.as_mut().expect("guarded").await }, if download.is_some() => {
                let task_id = pending_task.take().unwrap_or_default();
                let result = match res {
                    Ok(r) => r,
                    Err(e) => Err(anyhow::anyhow!("互传: download task failed: {e}")),
                };
                let ty = if result.is_ok() { "1" } else { "2" };
                let end = format!("action:2:status?{{\"taskId\":\"{task_id}\",\"type\":\"{ty}\"}}");
                if let Err(e) = ws.send_text(&end).await {
                    tracing::warn!(err = %e, "互传: could not report final status");
                }
                saved = Some(result);
                break;
            }
            frame = tokio::time::timeout(READ_TIMEOUT, ws.recv()) => {
                match frame.context("互传: timed out waiting for a WS frame")? {
                    Ok(f) => f,
                    // The socket died under us. If a pull is still running it is
                    // on its own HTTP connection and may well finish, so keep it
                    // rather than throwing away a transfer that is nearly done —
                    // we just cannot report status back on a dead WS.
                    Err(e) => {
                        if let Some(handle) = download.take() {
                            tracing::warn!(err = %e, "互传: WS died mid-download; awaiting the pull anyway");
                            saved = Some(match handle.await {
                                Ok(r) => r,
                                Err(je) => Err(anyhow::anyhow!("互传: download task failed: {je}")),
                            });
                            break;
                        }
                        return Err(anyhow::Error::new(e).context("互传: WS read failed"));
                    }
                }
            }
        };
        let msg = match frame {
            WsFrame::Text(t) => t,
            WsFrame::Ping(p) => {
                ws.send_pong(&p).await.ok();
                continue;
            }
            // The phone sometimes closes as soon as it has streamed the zip. If
            // a pull is still running, let it finish (we simply cannot report
            // status on a closed socket) instead of throwing the transfer away.
            WsFrame::Close => {
                if let Some(handle) = download.take() {
                    saved = Some(match handle.await {
                        Ok(r) => r,
                        Err(e) => Err(anyhow::anyhow!("互传: download task failed: {e}")),
                    });
                    tracing::info!("互传: phone closed the WS while downloading; kept the result");
                }
                break;
            }
            _ => continue,
        };
        let Some((mid, action, extra)) = parse_action(&msg) else {
            continue; // acks from the phone (`ack:2:status`) land here
        };

        match action {
            // Must be answered within 5s (BaseWebSocket TIMEOUT) or the phone
            // aborts with「传输失败」. Byte-for-byte what the official desktop
            // client sends.
            "versionNegotiation" => {
                let ack = format!(
                    "ack:{mid}:versionNegotiation?\
                     {{\"version\":1,\"threadLimit\":5,\"device_avatar_uri\":\"\"}}"
                );
                ws.send_text(&ack).await.context("send versionNegotiation ack")?;
            }

            // The phone now trusts us with the task; `id` is the taskId the
            // download route demands.
            "sendRequest" => {
                let info: Value = serde_json::from_str(extra)
                    .context("互传: sendRequest payload was not JSON")?;
                let task_id = info
                    .get("id")
                    .and_then(Value::as_str)
                    .context("互传: sendRequest carried no task id")?
                    .to_string();
                let count = info.get("fileCount").and_then(Value::as_i64).unwrap_or(-1);
                let bytes = info.get("totalSize").and_then(Value::as_i64).unwrap_or(-1);
                tracing::info!(task = %task_id, count, bytes, "互传: task announced");
                // Now a transfer really is coming. File names only arrive with the
                // zip, so `files` stays empty — that is what marks a batch as 互传
                // for the frontend (快传 batches always carry ≥1 name).
                on_event(FileTransEvent::Started { files: vec![] });
                // ★ This ack carries a body. A bare `ack:N:sendRequest` makes the
                //   phone drop the WS immediately (observed on-device).
                ws.send_text(&format!("ack:{mid}:sendRequest?{{\"versions\":[1]}}"))
                    .await
                    .context("send sendRequest ack")?;

                // From here on this session owns the phone: no further tap may abort it.
                committed.store(true, Ordering::SeqCst);
                // ★ Download off-loop. Pulling the zip inline blocks this read
                // loop for the whole transfer, and on anything big the phone
                // speaks again before it finishes — gets no ack inside its 5s
                // window — and kills the HTTP stream, which surfaces here as
                // `EOF inside chunked stream`. (Observed: 12MB fine, 157MB dead
                // after 0.6s.) It also delayed the final `action:2:status` past
                // the point the phone had given up, so a file that *did* land
                // still showed「发送失败」on the device row.
                download = Some(tokio::spawn({
                    let ip = phone_ip.to_string();
                    let dir = save_dir.to_string();
                    let task = task_id.clone();
                    async move { download_batch(&ip, port, &task, &dir).await }
                }));
                pending_task = Some(task_id);
            }

            // Unknown actions still need an ack or the phone stalls on its 5s timer.
            other => {
                tracing::debug!(action = other, "互传: unhandled WS action, acking blindly");
                ws.send_text(&format!("ack:{mid}:{other}")).await.ok();
            }
        }
    }

    saved.unwrap_or_else(|| bail!("互传: WS closed before the phone announced a task"))
}

/// Download the batch zip and extract it. The phone streams it chunked with the
/// CRC left in the data descriptor, so entry CRCs read as 0 — [`extract_zip`]
/// must not reject on that.
async fn download_batch(
    phone_ip: &str,
    port: u16,
    task_id: &str,
    save_dir: &str,
) -> Result<Vec<String>> {
    let route = format!("/download?taskId={task_id}&name=pcsuit&vtime=true");
    let tmp = format!("{save_dir}/.vivoshare-{}.zip.part", uuid::Uuid::new_v4());
    let saved = match http_download(phone_ip, port, &route, &tmp).await {
        Ok(bytes) => {
            tracing::info!(bytes, tmp = %tmp, "互传: zip downloaded, extracting");
            let tmp2 = tmp.clone();
            let dir = save_dir.to_string();
            tokio::task::spawn_blocking(move || extract_zip(&tmp2, &dir))
                .await
                .context("extract task join")?
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    saved
}

/// Open `ws://<phone>:<port>/websocket`, retrying until the window opens.
///
/// The phone's share HTTP server only exists for the duration of a task and its
/// availability is *not* synchronised with the 10191 accept: on-device it has
/// been seen accepting at t+0, gone by t+0.4s, and only genuinely up at t+13s
/// (and once t+35s). So poll rather than assume, and never give up early.
async fn grab_ws(phone_ip: &str, port: u16) -> Result<WsClient<TcpStream>> {
    let deadline = tokio::time::Instant::now() + WS_WINDOW;
    let mut attempts = 0u32;
    let mut last = "no attempt yet".to_string();
    while tokio::time::Instant::now() < deadline {
        attempts += 1;
        match open_ws(phone_ip, port).await {
            Ok(ws) => {
                tracing::info!(attempts, "互传: WS up");
                return Ok(ws);
            }
            Err(e) => {
                last = format!("{e:#}");
                // The very first failure is the diagnostic one: "connection
                // refused" means the phone never opened its server, while an
                // upgrade error means it did and rejected us — completely
                // different bugs. Later attempts are logged sparsely.
                if attempts == 1 || attempts.is_multiple_of(50) {
                    tracing::info!(attempts, err = %last, "互传: share server not reachable yet");
                }
            }
        }
        tokio::time::sleep(WS_POLL_INTERVAL).await;
    }
    bail!("ws://{phone_ip}:{port}/websocket never opened ({attempts} attempts, last: {last})")
}

async fn open_ws(phone_ip: &str, port: u16) -> Result<WsClient<TcpStream>> {
    let sock = tokio::time::timeout(WS_CONNECT_TIMEOUT, TcpStream::connect((phone_ip, port)))
        .await
        .context("tcp connect timed out")?
        .context("tcp connect failed")?;
    sock.set_nodelay(true).ok();
    let mut ws = WsClient::new(sock);
    // Empty token: this server has no token check (unlike the 10380 control WS);
    // the subprotocol is what it actually validates.
    let status = tokio::time::timeout(
        CONNECT_TIMEOUT,
        ws.upgrade(&format!("{phone_ip}:{port}"), WS_PATH, WS_SUBPROTOCOL, ""),
    )
    .await
    .context("ws upgrade timed out")?
    .context("ws upgrade failed")?;
    if !status.contains("101") {
        bail!("ws upgrade -> {status}");
    }
    Ok(ws)
}

/// Split `action:<id>:<name>[?<json>]` into its parts. Anything else (the
/// phone's own `ack:` replies) yields `None`.
fn parse_action(msg: &str) -> Option<(&str, &str, &str)> {
    let rest = msg.strip_prefix("action:")?;
    let (mid, rest) = rest.split_once(':')?;
    Some(match rest.split_once('?') {
        Some((action, extra)) => (mid, action, extra),
        None => (mid, rest, ""),
    })
}

/// `GET` a (chunked) file stream, writing the decoded body straight to `dest`.
/// Returns the decoded byte count. Runs the blocking file writes inline on the
/// async socket — writes are small flushes, fine on a worker thread.
async fn http_download(host: &str, port: u16, route: &str, dest: &str) -> Result<u64> {
    let s = connect_http(host, port, route).await?;
    let mut sock = BufSock {
        s,
        buf: Vec::new(),
    };
    // Read the header block.
    let head = loop {
        if let Some(pos) = find(&sock.buf, b"\r\n\r\n") {
            break sock.buf.drain(..pos + 4).collect::<Vec<u8>>();
        }
        if sock.buf.len() > 64 * 1024 {
            bail!("HTTP header block too large");
        }
        if sock.fill().await? == 0 {
            bail!("peer closed before HTTP headers");
        }
    };
    let (status, _, chunked) = split_head_full(&head)?;
    if status != 200 {
        bail!("{route} -> HTTP {status}");
    }
    let mut file = std::fs::File::create(dest).with_context(|| format!("create {dest}"))?;
    use std::io::Write;
    let mut total = 0u64;
    if chunked {
        loop {
            let line = sock.read_line().await?;
            let size_str = std::str::from_utf8(&line).unwrap_or("");
            let size =
                usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
                    .with_context(|| format!("bad chunk size {size_str:?}"))?;
            if size == 0 {
                break; // trailers (if any) are irrelevant for a zip stream
            }
            total += sock.copy_exact(size, &mut file).await?;
            sock.consume(2).await?; // trailing CRLF after each chunk
        }
    } else {
        // No chunked encoding (unexpected for this route): take whatever the
        // peer sends until EOF.
        if !sock.buf.is_empty() {
            file.write_all(&sock.buf)?;
            total += sock.buf.len() as u64;
            sock.buf.clear();
        }
        let mut tmp = [0u8; 64 * 1024];
        loop {
            let n = tokio::time::timeout(READ_TIMEOUT, sock.s.read(&mut tmp))
                .await
                .context("download read timeout")??;
            if n == 0 {
                break;
            }
            file.write_all(&tmp[..n])?;
            total += n as u64;
        }
    }
    file.flush()?;
    Ok(total)
}

/// Connect and send a bare HTTP/1.1 GET (no auth headers — 互传's WeiChuan-HTTP
/// server does not check them).
async fn connect_http(host: &str, port: u16, route: &str) -> Result<TcpStream> {
    let mut s = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .with_context(|| format!("connect {host}:{port} timed out"))?
        .with_context(|| format!("connect {host}:{port}"))?;
    s.set_nodelay(true).ok();
    let head = format!(
        "GET {route} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: vivoshare-pc\r\nConnection: close\r\n\r\n"
    );
    s.write_all(head.as_bytes()).await?;
    s.flush().await?;
    Ok(s)
}

/// A socket with a small read buffer, for chunked decoding without
/// over-consuming past chunk boundaries.
struct BufSock {
    s: TcpStream,
    buf: Vec<u8>,
}

impl BufSock {
    /// Read more into `buf`; returns bytes read (0 = EOF).
    async fn fill(&mut self) -> Result<usize> {
        let mut tmp = [0u8; 64 * 1024];
        let n = tokio::time::timeout(READ_TIMEOUT, self.s.read(&mut tmp))
            .await
            .context("download read timeout")??;
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(n)
    }

    /// Read one CRLF-terminated line (line content, without the CRLF).
    async fn read_line(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(pos) = find(&self.buf, b"\r\n") {
                return Ok(self.buf.drain(..pos + 2).take(pos).collect());
            }
            if self.fill().await? == 0 {
                bail!("EOF inside chunked stream (line)");
            }
        }
    }

    /// Drop exactly `n` bytes from the stream.
    async fn consume(&mut self, n: usize) -> Result<()> {
        while self.buf.len() < n {
            if self.fill().await? == 0 {
                bail!("EOF inside chunked stream (consume)");
            }
        }
        self.buf.drain(..n);
        Ok(())
    }

    /// Copy exactly `n` stream bytes into `w`; returns `n`.
    async fn copy_exact<W: std::io::Write>(&mut self, n: usize, w: &mut W) -> Result<u64> {
        let mut left = n;
        while left > 0 {
            if self.buf.is_empty() && self.fill().await? == 0 {
                bail!("EOF inside chunked stream (data)");
            }
            let take = left.min(self.buf.len());
            w.write_all(&self.buf[..take])?;
            self.buf.drain(..take);
            left -= take;
        }
        Ok(n as u64)
    }
}

/// Parse just a header block: `(status, (), chunked)`.
fn split_head_full(head: &[u8]) -> Result<(u16, (), bool)> {
    let text = std::str::from_utf8(head).unwrap_or("");
    let status = text
        .lines()
        .next()
        .filter(|l| l.starts_with("HTTP/"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let chunked = text.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    Ok((status, (), chunked))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Extract every regular file of the zip at `zip_path` into `dir`, basenames
/// only (entry names are phone-absolute paths), de-duplicating collisions with
/// a ` (1)` suffix. Directory entries are skipped. Returns saved basenames.
fn extract_zip(zip_path: &str, dir: &str) -> Result<Vec<String>> {
    let file = std::fs::File::open(zip_path).with_context(|| format!("open {zip_path}"))?;
    let mut zip = zip::ZipArchive::new(file).context("not a valid zip archive")?;
    let mut saved = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).with_context(|| format!("zip entry #{i}"))?;
        if entry.is_dir() {
            continue;
        }
        let raw = entry.name().to_string();
        let safe = safe_basename(&raw).unwrap_or_else(|| format!("recv-{i}.bin"));
        let dest = dedup_path(dir, &safe);
        let mut out = std::fs::File::create(&dest).with_context(|| format!("create {dest}"))?;
        std::io::copy(&mut entry, &mut out).with_context(|| format!("extract {safe}"))?;
        // 落盘名可能与 safe 不同（dedup 加了 " (N)" 后缀）—— 上报实际文件名。
        let final_name = std::path::Path::new(&dest)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(safe);
        tracing::info!(name = %final_name, from = %raw, "互传: saved");
        saved.push(final_name);
    }
    if saved.is_empty() {
        bail!("zip had no file entries");
    }
    Ok(saved)
}

/// Keep only the basename of a phone-side path; reject anything that would
/// escape the save dir or is otherwise unusable (`""`, `.`, `..`).
fn safe_basename(name: &str) -> Option<String> {
    let base = std::path::Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())?;
    if base.is_empty() || base == "." || base == ".." {
        None
    } else {
        Some(base)
    }
}

/// `dir/name`, or `dir/stem (N).ext` when already taken (N counting up).
pub(crate) fn dedup_path(dir: &str, name: &str) -> String {
    let first = format!("{dir}/{name}");
    if !std::path::Path::new(&first).exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(p) if p > 0 => (&name[..p], &name[p..]),
        _ => (name, ""),
    };
    for n in 1..1000 {
        let cand = format!("{dir}/{stem} ({n}){ext}");
        if !std::path::Path::new(&cand).exists() {
            return cand;
        }
    }
    first // give up: overwrite
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_split_half_and_double() {
        // 半帧 + 一次两帧都通过 decode_strict 循环正确拆分。
        let f1 = payload1::encode(br#"{"a":1}"#);
        let f2 = payload1::encode(br#"{"b":2}"#);
        let mut wire = f1.clone();
        wire.extend_from_slice(&f2);

        // half frame: incomplete -> None
        assert!(payload1::decode_strict(&wire[..10]).unwrap().is_none());
        assert!(payload1::decode_strict(&wire[..f1.len() - 1]).unwrap().is_none());

        // two frames in one buffer: first frame consumed exactly
        let (body, used) = payload1::decode_strict(&wire).unwrap().unwrap();
        assert_eq!(body, br#"{"a":1}"#);
        assert_eq!(used, f1.len());
        let (body2, used2) = payload1::decode_strict(&wire[used..]).unwrap().unwrap();
        assert_eq!(body2, br#"{"b":2}"#);
        assert_eq!(used + used2, wire.len());
    }

    #[test]
    fn extra_port_parsing() {
        // 真机格式：ipArr 不带引号，不是合法 JSON —— 只刮 port。
        let extra = r#"{"ipAddress":"192.168.1.10","port":"8080","deviceType":0,"ipArr":"[192.168.1.203]"}"#;
        assert_eq!(parse_extra_port(extra), 8080);
        let extra2 = r#"{"port":"9100","ipArr":"[10.0.0.2,10.0.0.3]"}"#;
        assert_eq!(parse_extra_port(extra2), 9100);
        // 无引号数字形式、缺失、乱码都回退 8080
        assert_eq!(parse_extra_port(r#"{"port":8081}"#), 8081);
        assert_eq!(parse_extra_port("{}"), 8080);
        assert_eq!(parse_extra_port("not json at all"), 8080);
        assert!(serde_json::from_str::<Value>(extra).is_ok(), "sample is valid JSON…");
        let _ = extra; // （port 解析不依赖整串可解析）
    }

    #[test]
    fn basename_sanitization() {
        assert_eq!(
            safe_basename("/storage/emulated/0/Pictures/截屏/a.jpg").as_deref(),
            Some("a.jpg")
        );
        assert_eq!(safe_basename("/storage/emulated/0/").as_deref(), Some("0"));
        assert_eq!(safe_basename("/"), None);
        assert_eq!(safe_basename(".."), None);
        assert_eq!(safe_basename(""), None);
    }

    #[test]
    fn dedup_renames() {
        let dir = std::env::temp_dir().join(format!("pcsuite-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert_eq!(dedup_path(&d, "a.jpg"), format!("{d}/a.jpg"));
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        assert_eq!(dedup_path(&d, "a.jpg"), format!("{d}/a (1).jpg"));
        std::fs::write(dir.join("a (1).jpg"), b"x").unwrap();
        assert_eq!(dedup_path(&d, "a.jpg"), format!("{d}/a (2).jpg"));
        // 无扩展名 / 点前缀
        assert_eq!(dedup_path(&d, "README"), format!("{d}/README"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_zip_roundtrip() {
        // 造一个 zip（store 模式即可，zip crate 都能读），entry 名是手机绝对路径。
        let dir = std::env::temp_dir().join(format!("pcsuite-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let zpath = dir.join("files.zip");
        {
            let f = std::fs::File::create(&zpath).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            use std::io::Write;
            zw.start_file("/storage/emulated/0/Pictures/a.jpg", opts).unwrap();
            zw.write_all(b"jpeg-bytes").unwrap();
            zw.start_file("/storage/emulated/0/DCIM/b.jpg", opts).unwrap();
            zw.write_all(b"more").unwrap();
            zw.add_directory("/storage/emulated/0/DCIM/", opts).unwrap();
            zw.finish().unwrap();
        }
        // 目标目录放一个重名文件验证 dedup。
        std::fs::write(dir.join("a.jpg"), b"old").unwrap();
        let saved = extract_zip(zpath.to_str().unwrap(), dir.to_str().unwrap()).unwrap();
        assert_eq!(saved, vec!["a (1).jpg".to_string(), "b.jpg".to_string()]);
        assert_eq!(std::fs::read(dir.join("a (1).jpg")).unwrap(), b"jpeg-bytes");
        assert_eq!(std::fs::read(dir.join("b.jpg")).unwrap(), b"more");
        assert_eq!(std::fs::read(dir.join("a.jpg")).unwrap(), b"old", "原文件不被覆盖");
        std::fs::remove_dir_all(&dir).ok();
    }
}
