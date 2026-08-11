//! SSDP-style presence/discovery on UDP 2200.
//!
//! Announcements are HTTP-ish datagrams carrying a base64 `compatGsonStr` JSON
//! blob describing the peer. We send our presence (multicast for discovery, or
//! unicast to a known phone for the direct/Tailscale path) and read the phone's
//! announcements back.

use std::net::SocketAddr;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::time::timeout;

pub const SSDP_PORT: u16 = 2200;
pub const MCAST_ADDR: &str = "239.255.255.250";

/// Build a presence announcement datagram from a base64 `compatGsonStr`.
///
/// `DATE` carries the sender's **local** wall clock as `YYYY-MM-DD HH:MM:SS` —
/// the format every other peer on the group uses. It is not decorative: a beacon
/// stamped with a long-past time is ignored, so this must be the current time on
/// every send (build the datagram per beacon, not once).
pub fn build_announce(compat_gson_b64: &str) -> Vec<u8> {
    format!(
        "RESPONSE * HTTP/1.1\r\nDATE:{}\r\ncompatGsonStr:{compat_gson_b64}\r\n\r\n",
        now_local()
    )
    .into_bytes()
}

/// Local wall clock as `YYYY-MM-DD HH:MM:SS`.
///
/// Hand-rolled to keep the crate dependency-free: the UTC offset is read once
/// from the system (`date +%z`) and the calendar conversion is arithmetic.
pub fn now_local() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + utc_offset_secs();

    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Seconds east of UTC, resolved once per process.
fn utc_offset_secs() -> i64 {
    static OFF: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        // `date +%z` → "+0800" / "-0500". Anything unexpected means UTC.
        let out = std::process::Command::new("date").arg("+%z").output();
        let raw = match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            Err(_) => return 0,
        };
        let bytes = raw.as_bytes();
        if bytes.len() < 5 {
            return 0;
        }
        let sign = if bytes[0] == b'-' { -1 } else { 1 };
        let hh: i64 = raw[1..3].parse().unwrap_or(0);
        let mm: i64 = raw[3..5].parse().unwrap_or(0);
        sign * (hh * 3600 + mm * 60)
    })
}

/// Days since the Unix epoch → (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Extract and base64-decode the `compatGsonStr` line into a JSON value.
fn parse_compat_gson(datagram: &[u8]) -> Option<serde_json::Value> {
    let text = std::str::from_utf8(datagram).ok()?;
    for line in text.lines() {
        if let Some(b64) = line.strip_prefix("compatGsonStr:") {
            let raw = STANDARD.decode(b64.trim()).ok()?;
            return serde_json::from_slice(&raw).ok();
        }
    }
    None
}

/// Continuously unicast `announce` to `phone_ip:2200` every `interval`.
///
/// Runs forever; spawn it with `tokio::spawn` and abort the handle when done.
/// This is the direct/Tailscale presence path (no multicast).
pub async fn presence_loop(
    phone_ip: String,
    compat_gson_b64: String,
    interval: Duration,
) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    let target = format!("{phone_ip}:{SSDP_PORT}");
    loop {
        // Rebuilt each time so the DATE header stays current.
        let _ = sock.send_to(&build_announce(&compat_gson_b64), &target).await;
        tokio::time::sleep(interval).await;
    }
}

/// Continuously **multicast** `announce` so nearby phones list this PC.
///
/// This is the "am I here" beacon the official desktop client runs the whole
/// time it is up (observed: one datagram every ~5s, sent *from* port 2200). The
/// phone discovers PCs by listening for it — it does not poll with `M-SEARCH` —
/// so without this its search reports "device not found" even when the account
/// already lists the PC.
///
/// Binds 2200 with address/port reuse so it coexists with anything else on the
/// port, and sends from it to match the official client's source port.
/// Runs forever; spawn it and abort the handle to stop.
pub async fn announce_loop(compat_gson_b64: String, interval: Duration) -> std::io::Result<()> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // Share the port so this coexists with anything else beaconing on it
    // (notably the official client, if the user still runs it).
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    // Prefer 2200 as the source port (what the official client sends from); if
    // something else holds it, beacon from an ephemeral port rather than not at
    // all — the phone matches on the payload, not the source port.
    let bind: SocketAddr = format!("0.0.0.0:{SSDP_PORT}").parse().unwrap();
    if socket.bind(&bind.into()).is_err() {
        tracing::debug!("udp/{SSDP_PORT} busy; beaconing from an ephemeral port");
        socket.bind(&"0.0.0.0:0".parse::<SocketAddr>().unwrap().into())?;
    }
    let udp = UdpSocket::from_std(socket.into())?;
    udp.set_multicast_ttl_v4(2).ok();

    let mcast: SocketAddr = format!("{MCAST_ADDR}:{SSDP_PORT}").parse().unwrap();
    loop {
        // Rebuilt each time so the DATE header stays current.
        if let Err(e) = udp.send_to(&build_announce(&compat_gson_b64), mcast).await {
            tracing::debug!(%e, "presence multicast failed (network down?)");
        }
        tokio::time::sleep(interval).await;
    }
}

/// Multicast discovery: announce ourselves and wait for a phone (`deviceType ==
/// "mobile"`) to reply. Returns `(phone_ip, its compatGson JSON)`.
pub async fn discover(announce: Vec<u8>, overall_timeout: Duration) -> std::io::Result<Option<(String, serde_json::Value)>> {
    // Bind 0.0.0.0:2200 with SO_REUSEADDR so we coexist with anything else on the port.
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    let bind: SocketAddr = "0.0.0.0:2200".parse().unwrap();
    socket.bind(&bind.into())?;
    let udp = UdpSocket::from_std(socket.into())?;
    udp.set_multicast_ttl_v4(2).ok();

    let mcast: SocketAddr = format!("{MCAST_ADDR}:{SSDP_PORT}").parse().unwrap();

    let deadline = tokio::time::Instant::now() + overall_timeout;
    let mut buf = vec![0u8; 65535];
    while tokio::time::Instant::now() < deadline {
        let _ = udp.send_to(&announce, mcast).await;
        // listen for ~1.4s between re-announcements
        let listen_until = tokio::time::Instant::now() + Duration::from_millis(1400);
        while tokio::time::Instant::now() < listen_until {
            let remaining = listen_until - tokio::time::Instant::now();
            match timeout(remaining, udp.recv_from(&mut buf)).await {
                Ok(Ok((n, addr))) => {
                    if let Some(json) = parse_compat_gson(&buf[..n]) {
                        if json.get("deviceType").and_then(|v| v.as_str()) == Some("mobile") {
                            return Ok(Some((addr.ip().to_string(), json)));
                        }
                    }
                }
                _ => break,
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_contains_compat_gson() {
        let a = build_announce("BASE64HERE");
        let s = String::from_utf8(a).unwrap();
        assert!(s.starts_with("RESPONSE * HTTP/1.1\r\n"));
        assert!(s.contains("compatGsonStr:BASE64HERE\r\n"));
    }

    #[test]
    fn date_header_is_current_local_time_in_the_peers_format() {
        // A stale DATE gets the beacon ignored, so this is load-bearing: it must
        // be `YYYY-MM-DD HH:MM:SS` (what phones and the official PC client send),
        // not an HTTP-style date, and it must be *now*.
        let s = String::from_utf8(build_announce("X")).unwrap();
        let date = s
            .lines()
            .find_map(|l| l.strip_prefix("DATE:"))
            .expect("DATE header");
        let b = date.as_bytes();
        assert_eq!(date.len(), 19, "got {date:?}");
        assert!(b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[7] == b'-');
        assert!(b[10] == b' ' && b[13] == b':' && b[16] == b':');

        // Same second as an independent conversion of the clock.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + utc_offset_secs();
        let (y, m, d) = civil_from_days(now.div_euclid(86_400));
        assert!(date.starts_with(&format!("{y:04}-{m:02}-{d:02}")), "got {date:?}");
    }

    #[test]
    fn civil_from_days_handles_epoch_and_leap_years() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(19_416), (2023, 2, 28));
        assert_eq!(civil_from_days(19_417), (2023, 3, 1)); // non-leap year rollover
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // leap day
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn parse_roundtrip() {
        let json = serde_json::json!({"deviceType":"mobile","deviceName":"iqoo"});
        let b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json).unwrap());
        let datagram = build_announce(&b64);
        let parsed = parse_compat_gson(&datagram).unwrap();
        assert_eq!(parsed["deviceType"], "mobile");
        assert_eq!(parsed["deviceName"], "iqoo");
    }
}
