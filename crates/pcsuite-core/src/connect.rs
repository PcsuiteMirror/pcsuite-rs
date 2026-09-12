//! ConnectFlow registration on 10191.
//!
//! Registers a self-made token with the phone so it opens the 10380 control
//! service. This is the pure-Rust replacement for the desktop connection-service
//! binary. Success is confirmed by polling 10380.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

use pcsuite_proto::connect::{self, Presence};
use pcsuite_proto::{payload1, PcIdentity};
use pcsuite_net::{ssdp, tcp};

use crate::config;

/// Inputs to [`register`].
pub struct RegisterConfig {
    /// IP to run the 10191 ConnectFlow against (LAN IP, or the phone's
    /// Tailscale IP for the remote path).
    pub reg_ip: String,
    /// PC identity advertised to the phone.
    pub identity: PcIdentity,
    /// Per-IP stored seed for the LAN `connectType=2` path. Ignored when `remote`.
    pub stored_seed: Option<String>,
    /// Use the `connectType=1` remote path (`key = SHA256(seed_b)`, no pre-shared
    /// seed) so registration works on any network.
    pub remote: bool,
    /// Token to register; a fresh 64-hex token is generated when `None`.
    pub token: Option<String>,
    /// Connection id; a timestamped id is generated when `None`.
    pub conn_id: Option<String>,
    /// Continuously unicast SSDP presence to `reg_ip` for the session.
    pub presence: bool,
}

/// A successful registration. Holds the presence task for the session's lifetime;
/// dropping it stops the presence announcements.
pub struct Registration {
    pub phone_ip: String,
    pub token: String,
    pub conn_id: String,
    presence_task: Option<JoinHandle<std::io::Result<()>>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(h) = self.presence_task.take() {
            h.abort();
        }
    }
}

/// Owns the presence task until [`register`] hands it to a [`Registration`].
/// Registration can fail (unreachable phone) or be cancelled mid-flight (the app
/// dropping the connect future), and a bare `JoinHandle` only *detaches* on drop
/// — the loop would keep unicasting presence to the phone forever, one leaked
/// task per failed attempt. Aborting on drop bounds it to the attempt.
struct PresenceGuard(Option<JoinHandle<std::io::Result<()>>>);

impl PresenceGuard {
    /// Hand the task over to the caller (it becomes the `Registration`'s job).
    fn release(mut self) -> Option<JoinHandle<std::io::Result<()>>> {
        self.0.take()
    }
}

impl Drop for PresenceGuard {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read from `sock` until a ConnectFlow reply JSON can be parsed (or timeout).
async fn read_reply(sock: &mut TcpStream, dur: Duration) -> Option<serde_json::Value> {
    let deadline = Instant::now() + dur;
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match timeout(remaining, sock.read(&mut tmp)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some((v, _off)) = payload1::parse_reply_lenient(&buf) {
                    return Some(v);
                }
            }
            _ => break,
        }
    }
    None
}

/// Run the full ConnectFlow and return once the phone opens 10380.
pub async fn register(cfg: RegisterConfig) -> Result<Registration> {
    let token = cfg.token.clone().unwrap_or_else(random_token);
    let conn_id = cfg
        .conn_id
        .clone()
        .unwrap_or_else(|| format!("pcsuite_{}", epoch_secs()));

    // Presence (direct/Tailscale path): keep announcing ourselves to the phone.
    let presence_task = if cfg.presence {
        let pres = Presence {
            device_id: &cfg.identity.pc_mac,
            open_id: &cfg.identity.open_id,
            account: &cfg.identity.account,
            device_name: &cfg.identity.device_name,
            service_record: config::SERVICE_RECORD,
            port: 10191,
            device_type: "pc",
            extra: "null",
        };
        let b64 = STANDARD.encode(serde_json::to_vec(&pres)?);
        let handle = PresenceGuard(Some(tokio::spawn(ssdp::presence_loop(
            cfg.reg_ip.clone(),
            b64,
            Duration::from_secs(2),
        ))));
        // give the phone a moment to register us as an active nearby peer
        tokio::time::sleep(Duration::from_secs(2)).await;
        Some(handle)
    } else {
        None
    };

    tracing::info!(reg_ip = %cfg.reg_ip, remote = cfg.remote, "ConnectFlow: connecting 10191");
    let mut sock = tcp::connect(&cfg.reg_ip, 10191)
        .await
        .context("connect 10191 (phone idle / WiFi off / IP changed?)")?;

    // [1] device-info exchange
    let dframe = payload1::encode_json(&connect::device_info_frame(&cfg.identity, 22))?;
    sock.write_all(&dframe).await?;
    sock.flush().await?;
    if let Some(v) = read_reply(&mut sock, Duration::from_secs(8)).await {
        tracing::info!(code = ?connect::reply_code(&v), "device_info reply");
    }

    // [2] compute sign
    let seed = if cfg.remote {
        String::new()
    } else {
        cfg.stored_seed
            .clone()
            .context("LAN mode needs a stored_seed for this IP (or pass remote = true)")?
    };
    let seed_b = uuid::Uuid::new_v4().to_string().to_uppercase();
    let sign = pcsuite_crypto::make_sign(&cfg.identity.open_id, &conn_id, &token, &seed, &seed_b);
    let connect_type = if cfg.remote { 1 } else { 2 };

    // [3] connect frame (seed + sign)
    let cframe = payload1::encode_json(&connect::connect_frame(
        &cfg.identity,
        &seed_b,
        &sign,
        connect_type,
        false,
        620,
    ))?;
    sock.write_all(&cframe).await?;
    sock.flush().await?;
    let reply_code = read_reply(&mut sock, Duration::from_secs(8))
        .await
        .and_then(|v| connect::reply_code(&v));
    tracing::info!(?reply_code, "connect reply");
    drop(sock);

    // [4] confirm: phone opens 10380
    let mut opened = false;
    for _ in 0..8 {
        if tcp::port_open(&cfg.reg_ip, 10380, Duration::from_secs(2)).await {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !opened {
        bail!(
            "10380 did not open (reply code {:?}) — token not accepted. \
             LAN needs the per-IP stored seed; otherwise pass remote = true (connectType=1).",
            reply_code
        );
    }

    tracing::info!(token = %token, "registration accepted; 10380 open");
    Ok(Registration {
        phone_ip: cfg.reg_ip,
        token,
        conn_id,
        presence_task: presence_task.and_then(PresenceGuard::release),
    })
}

/// Inputs to [`presence_once`].
pub struct PresenceConfig {
    /// Phone LAN IP to hold the 10191 ConnectFlow connection to.
    pub phone_ip: String,
    /// PC identity. `pc_mac` (the `target_id`) MUST match the businessId this PC
    /// is registered under in the connection center, or the phone accepts the
    /// connection but never associates it with the listed device (stays "未发现").
    pub identity: PcIdentity,
    /// Per-IP stored seed for `connectType=2`. Ignored when `remote`.
    pub stored_seed: Option<String>,
    /// Use the `connectType=1` remote path instead of a pre-shared seed.
    pub remote: bool,
    /// Announce a connect (`bytes:[0]` + sign, `isAutoConnect:"1"`) as part of the
    /// *hold*, which is what this code used to do.
    ///
    /// The official pre-connect does **not**:
    /// `DeviceWaitForPreConnect::startPreConnect` is ping → open socket → send
    /// device_info → recv device_info → parse, and then it just holds the socket
    /// (`recvMessageAndHeartbeatLoop`). No connect frame, no sign, no seed — the
    /// `bytes:[0]` frame belongs to `ConnectFlow`, i.e. the *formal* connect.
    ///
    /// Announcing a connect at hold time makes the phone believe one is starting: it
    /// raises a 「正在连接 …」 notification that never completes (observed on a real
    /// phone; the user has to cancel it before the connection center is usable).
    /// Kept switchable only to reproduce that.
    pub connect_frame_on_hold: bool,
    /// When the phone asks to connect (`bytes:[24]`), send a fresh connect frame
    /// (`isAutoConnect:"0"`, new token) **on this same held connection** and hand the
    /// new token to the caller, instead of handing over the token the presence
    /// handshake registered.
    ///
    /// Why in place and never on a second socket: the phone allows one 10191
    /// connection per PC — opening another closes this one within milliseconds
    /// (measured), and the `bytes:[25]`/`[27]` answers the phone is waiting for ride
    /// on *this* connection. The official service's `ConnectFlow` looks the device up
    /// with `findPreconnectDevice` for exactly that reason ("device is not
    /// preconnect!" is its complaint when there is nothing to reuse).
    pub reregister_on_ask: bool,
}

/// Do the 10191 ConnectFlow handshake, then **hold the connection open** — which
/// is exactly what keeps the phone listing this PC as discoverable ("可连").
///
/// Unlike [`register`], this does not escalate to 10380: `connect_status` stays
/// `false`, the socket is kept open, and the function only returns when the phone
/// closes it (EOF) or an error occurs — so the caller can reconnect. `on_ready`
/// fires once the handshake is accepted (auth_status true / reply code Success).
///
/// Falsified alternatives (do not reintroduce): the discoverable state is NOT
/// maintained by vpush(MQTT), the `getUserCookie` cloud heartbeat, or SSDP
/// beacons — only by this held LAN connection (see docs/LAN_DISCOVERY_HANDOFF.md).
/// What the caller made of the phone's connect request, reported back to the phone
/// on the held connection as `bytes:[27]` `{"retCode","retMsg"}`. `ret_code` 0 means
/// the session is up (the official desktop's `ConnectionErrorCodeFO.Success`); any
/// other value tells the phone the connect failed, and presence keeps holding.
#[derive(Debug, Clone)]
pub struct ConnectAnswer {
    pub ret_code: i64,
    pub ret_msg: String,
}

impl ConnectAnswer {
    pub fn ok() -> Self {
        ConnectAnswer { ret_code: 0, ret_msg: "success".into() }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        ConnectAnswer { ret_code: 1, ret_msg: msg.into() }
    }
}

/// How long to keep the held connection open waiting for the caller to bring the
/// session up and report back. The phone gives up in ~5s, so a caller that takes
/// longer has already lost the round — but keep reading until then rather than
/// tearing the link down under it.
const CONNECT_ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

/// A connection id shaped like the official desktop's — `<4 hex>_<epoch ms>`
/// (Electron `pre-connect-mode`: `${createRandomStr(4)}_${Date.now()}`).
fn official_conn_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:04x}_{}", now.subsec_nanos() as u16, now.as_millis())
}

pub async fn presence_once<F, G>(
    cfg: &PresenceConfig,
    on_ready: F,
    on_connect_request: G,
) -> Result<()>
where
    F: FnOnce(),
    // Kicks off the connect with the token this connection registered and hands back
    // a receiver for the outcome, which is reported to the phone as `bytes:[27]`.
    G: Fn(&str) -> oneshot::Receiver<ConnectAnswer>,
{
    tracing::info!(phone = %cfg.phone_ip, remote = cfg.remote, "presence: connecting 10191");
    let mut sock = tcp::connect(&cfg.phone_ip, 10191)
        .await
        .context("connect 10191 (phone idle / WiFi off / IP changed?)")?;

    // [1] device-info exchange — on its own this is the whole official pre-connect.
    let dframe = payload1::encode_json(&connect::device_info_frame(&cfg.identity, 22))?;
    sock.write_all(&dframe).await?;
    sock.flush().await?;
    let ack = read_reply(&mut sock, Duration::from_secs(8)).await;
    tracing::info!(
        code = ?ack.as_ref().and_then(connect::reply_code),
        auth = ?ack.as_ref().and_then(connect::auth_status),
        "device_info reply"
    );

    // The sign inputs for whichever connect frame we end up sending. A missing seed
    // is not fatal any more: it just means the seedless `connectType=1` path.
    let (connect_type, seed) = match (cfg.remote, cfg.stored_seed.clone()) {
        (false, Some(s)) => (2, s),
        (false, None) => {
            tracing::info!("presence: no stored seed for this phone IP → connectType=1");
            (1, String::new())
        }
        (true, _) => (1, String::new()),
    };

    // [2] Optionally announce a connect while merely holding. Off by default: the
    // official pre-connect stops after [1], and announcing here leaves the phone with
    // a 「正在连接 …」 notification it can never finish (see `connect_frame_on_hold`).
    let token = random_token();
    if cfg.connect_frame_on_hold {
        let conn_id = format!("pcsuite_presence_{}", epoch_secs());
        let seed_b = uuid::Uuid::new_v4().to_string().to_uppercase();
        let sign =
            pcsuite_crypto::make_sign(&cfg.identity.open_id, &conn_id, &token, &seed, &seed_b);
        let cframe = payload1::encode_json(&connect::connect_frame(
            &cfg.identity,
            &seed_b,
            &sign,
            connect_type,
            true,
            680,
        ))?;
        sock.write_all(&cframe).await?;
        sock.flush().await?;
        let reply = read_reply(&mut sock, Duration::from_secs(8)).await;
        // The reply code does not decide anything: the phone lists this PC as 「可连」
        // for as long as the connection lives, whatever it answered (an
        // OpenIdMismatch/Reject on the *connect* step does not un-list us). Only the
        // connection dropping does.
        tracing::info!(
            code = ?reply.as_ref().and_then(connect::reply_code),
            auth = ?reply.as_ref().and_then(connect::auth_status),
            "presence connect reply (hold-time announce)"
        );
    }
    tracing::info!(
        reregister_on_ask = cfg.reregister_on_ask,
        "presence: holding the connection open = 「可连」"
    );

    on_ready();

    // [3] Hold the connection open. The phone keeps this PC "可连" for as long as
    // the connection lives; it may also push frames here (e.g. when the user taps
    // "connect" on the phone), which we surface for the caller to act on later.
    let mut tmp = [0u8; 8192];
    'hold: loop {
        match sock.read(&mut tmp).await {
            Ok(0) => {
                tracing::info!("presence: phone closed the connection (EOF)");
                return Ok(());
            }
            Ok(n) => {
                if let Some((v, _)) = payload1::parse_reply_lenient(&tmp[..n]) {
                    tracing::info!(
                        code = ?connect::reply_code(&v),
                        bytes = n,
                        json = %v,
                        "presence: phone push"
                    );
                    // `bytes:[24]` = the phone tapped "连接" (wlan_mobile_ask_connect_pc).
                    // The phone then waits for US, on this same connection, to answer —
                    // `[25]` right away, then `[27]` with `{"retCode","retMsg"}` once the
                    // session is up (that is all `mobileAskConnectPC` does: ack, ask the
                    // desktop to connect, report the result). Opening 10380 without
                    // answering makes the phone give up after ~5s and go back to
                    // 「未发现」 — measured on a real phone, all three connect variants.
                    if connect::is_connect_request(&v) {
                        tracing::info!("presence: phone requested connect (bytes:[24]) → ack [25]");
                        let ack = payload1::encode_json(&connect::ask_connect_ack_frame(
                            &cfg.identity,
                        ))?;
                        sock.write_all(&ack).await?;
                        sock.flush().await?;

                        // Optionally turn this pre-connect link into a formal connect in
                        // place: same socket, fresh token/connId/seed_b, and
                        // `isAutoConnect:"0"` — what the official desktop asks its
                        // service for (`preconnect_connect`, autoConnect "0") without
                        // giving up the connection the phone is waiting on.
                        let session_token = if cfg.reregister_on_ask {
                            let t = random_token();
                            let cid = official_conn_id();
                            let sb = uuid::Uuid::new_v4().to_string().to_uppercase();
                            let sg = pcsuite_crypto::make_sign(
                                &cfg.identity.open_id,
                                &cid,
                                &t,
                                &seed,
                                &sb,
                            );
                            let f = payload1::encode_json(&connect::connect_frame(
                                &cfg.identity,
                                &sb,
                                &sg,
                                connect_type,
                                false,
                                620,
                            ))?;
                            sock.write_all(&f).await?;
                            sock.flush().await?;
                            let code = read_reply(&mut sock, Duration::from_secs(8))
                                .await
                                .and_then(|v| connect::reply_code(&v));
                            tracing::info!(?code, "presence: re-registered in place (bytes:[0])");
                            t
                        } else {
                            token.clone()
                        };

                        // Bring the session up while this connection stays open, then
                        // tell the phone how it went.
                        let mut rx = on_connect_request(&session_token);
                        let deadline = tokio::time::sleep(CONNECT_ANSWER_TIMEOUT);
                        tokio::pin!(deadline);
                        let answer = loop {
                            tokio::select! {
                                r = &mut rx => break r.ok(),
                                _ = &mut deadline => {
                                    tracing::warn!("presence: no connect answer in time");
                                    break None;
                                }
                                read = sock.read(&mut tmp) => match read {
                                    // The phone hung up before we could answer.
                                    Ok(0) => return Ok(()),
                                    Ok(_) => continue,   // ignore further pushes meanwhile
                                    Err(e) => return Err(e).context("presence read (connecting)"),
                                },
                            }
                        };
                        let Some(answer) = answer else {
                            continue 'hold;
                        };
                        tracing::info!(
                            ret_code = answer.ret_code,
                            ret_msg = %answer.ret_msg,
                            "presence: reporting connect result [27]"
                        );
                        let res = payload1::encode_json(&connect::ask_connect_result_frame(
                            &cfg.identity,
                            answer.ret_code,
                            &answer.ret_msg,
                        ))?;
                        sock.write_all(&res).await?;
                        sock.flush().await?;
                        // Keep holding either way — this connection *is* how the phone
                        // sees this PC. Dropping it after a successful connect puts the
                        // device straight back to 「未发现」 even with the 10380 session
                        // up (measured); the phone only ever un-lists us when the
                        // connection goes away.
                        continue 'hold;
                    }
                } else {
                    tracing::info!(
                        bytes = n,
                        hex = %hex::encode(&tmp[..n.min(400)]),
                        "presence: phone data (unparsed)"
                    );
                }
            }
            Err(e) => return Err(e).context("presence connection read"),
        }
    }
}
