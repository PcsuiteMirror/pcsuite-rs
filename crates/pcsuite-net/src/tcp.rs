//! Small TCP helpers.

use std::io;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Default connect timeout. A host that is simply *not there* (a remembered
/// Wi-Fi device whose IP has since changed, phone asleep, Wi-Fi off) never
/// answers the SYN, and the OS would keep retrying for ~75 s on macOS before
/// giving up — long enough that the app looks hung. Fail fast instead: a LAN
/// handshake to a live phone completes in milliseconds.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Open a TCP connection with `TCP_NODELAY` set, bounded by [`CONNECT_TIMEOUT`].
pub async fn connect(ip: &str, port: u16) -> io::Result<TcpStream> {
    connect_timeout(ip, port, CONNECT_TIMEOUT).await
}

/// Open a TCP connection with `TCP_NODELAY` set, giving up after `dur`.
pub async fn connect_timeout(ip: &str, port: u16, dur: Duration) -> io::Result<TcpStream> {
    match timeout(dur, TcpStream::connect((ip, port))).await {
        Ok(r) => {
            let s = r?;
            s.set_nodelay(true).ok();
            Ok(s)
        }
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no answer from {ip}:{port} within {}s", dur.as_secs()),
        )),
    }
}

/// Probe whether `ip:port` accepts a connection within `dur`.
pub async fn port_open(ip: &str, port: u16, dur: Duration) -> bool {
    matches!(timeout(dur, TcpStream::connect((ip, port))).await, Ok(Ok(_)))
}
