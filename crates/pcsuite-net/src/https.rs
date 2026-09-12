//! A *certificate-verifying* HTTPS client, used only for vivo's public cloud API.
//!
//! Deliberately separate from [`crate::tls`]: that module skips validation
//! because the phone serves a self-signed certificate on 10380/10381 and the
//! official client accepts it. Cloud requests carry the account token, so here
//! the chain is checked against the Mozilla root store.
//!
//! Scope is only what the connection-center API needs: one request per
//! connection (`Connection: close`), read to EOF, decode a chunked body if the
//! server sends one. No keep-alive, no redirects, no compression.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Wall-clock budget for a whole request (TCP + TLS + round trip).
const TIMEOUT: Duration = Duration::from_secs(20);

/// How long a streaming download may stall before it is abandoned. Applies per
/// read, not to the transfer as a whole — a large file is allowed to take as
/// long as it takes provided bytes keep arriving.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// An HTTP response with its body already de-chunked.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    /// Body parsed as JSON.
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body).with_context(|| {
            format!(
                "response body is not JSON (status {}): {}",
                self.status,
                String::from_utf8_lossy(&self.body[..self.body.len().min(200)])
            )
        })
    }

    /// True for 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

fn client_config() -> Result<Arc<ClientConfig>> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(c) = CFG.get() {
        return Ok(c.clone());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls protocol versions")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);
    Ok(CFG.get_or_init(|| config).clone())
}

/// Send one HTTPS request to `host:443` and read the whole response.
///
/// `headers` are extra request headers as `(name, value)`; `Host`, `Connection`,
/// `Accept` and the body's `Content-Type`/`Content-Length` are added here. A
/// `Some(body)` is sent as `application/json`.
pub async fn request(
    host: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Response> {
    tokio::time::timeout(TIMEOUT, request_inner(host, method, path, headers, body))
        .await
        .with_context(|| format!("{method} https://{host}{path} timed out"))?
}

async fn request_inner(
    host: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Response> {
    let mut tls = connect_and_send(host, method, path, headers, body, "application/json").await?;

    // `Connection: close` → EOF marks the end; no Content-Length bookkeeping.
    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).await.context("read response")?;
    parse_response(&raw)
}

/// Open the connection and write the request head (plus body); leave the stream
/// positioned at the first response byte. `accept` fills the `Accept` header.
async fn connect_and_send(
    host: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    accept: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let connector = TlsConnector::from(client_config()?);
    let tcp = TcpStream::connect((host, 443))
        .await
        .with_context(|| format!("tcp connect {host}:443"))?;
    tcp.set_nodelay(true).ok();
    let name = ServerName::try_from(host.to_string()).context("invalid host name")?;
    let mut tls = connector
        .connect(name, tcp)
        .await
        .with_context(|| format!("tls handshake with {host}"))?;

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: {accept}\r\n\
         User-Agent: pcsuite\r\nConnection: close\r\n"
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    head.push_str("\r\n");

    tls.write_all(head.as_bytes()).await.context("write head")?;
    if let Some(b) = body {
        tls.write_all(b).await.context("write body")?;
    }
    tls.flush().await.ok();
    Ok(tls)
}

/// Outcome of a streaming request: how it ended, and what came back.
pub struct StreamOutcome {
    pub status: u16,
    /// Body bytes written to the sink (0 for a non-2xx status).
    pub written: u64,
    /// For a non-2xx status, the start of the body — the server's error JSON.
    /// Empty on success, because a successful body went to the sink instead.
    pub error_body: Vec<u8>,
}

/// Send one HTTPS request and stream the response body into `sink` instead of
/// buffering it, so a multi-gigabyte download costs a fixed amount of memory.
///
/// `Accept: */*` is sent, since the response is not expected to be JSON. A
/// non-2xx status is *not* streamed: the body is read into
/// [`StreamOutcome::error_body`] (capped) so the caller can report the server's
/// complaint. The idle budget is [`STREAM_IDLE_TIMEOUT`] per read, with no
/// overall deadline.
pub async fn request_streaming<W>(
    host: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    sink: &mut W,
) -> Result<StreamOutcome>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut tls = tokio::time::timeout(
        TIMEOUT,
        connect_and_send(host, method, path, headers, body, "*/*"),
    )
    .await
    .with_context(|| format!("{method} https://{host}{path} timed out"))??;

    // Read just far enough to have the whole header block; whatever of the body
    // arrived in the same read is kept and replayed into the decoder below.
    let mut buf = Vec::with_capacity(8 * 1024);
    let split = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if read_more(&mut tls, &mut buf).await? == 0 {
            bail!("connection closed before the response headers were complete");
        }
        if buf.len() > 64 * 1024 {
            bail!("response headers exceed 64 KiB");
        }
    };

    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .context("no HTTP status line")?;
    let mut chunked = false;
    let mut content_length: Option<u64> = None;
    for l in lines {
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        } else if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse::<u64>().ok();
        }
    }

    let mut rest = buf.split_off(split + 4);

    if !(200..300).contains(&status) {
        // Keep enough of the error body to be diagnosable, then give up on it.
        while rest.len() < 4096 {
            let mut more = Vec::new();
            if read_more(&mut tls, &mut more).await? == 0 {
                break;
            }
            rest.extend_from_slice(&more);
        }
        rest.truncate(4096);
        return Ok(StreamOutcome { status, written: 0, error_body: rest });
    }

    let written = if chunked {
        stream_chunked(&mut tls, rest, sink).await?
    } else {
        stream_identity(&mut tls, rest, content_length, sink).await?
    };
    sink.flush().await.context("flush download sink")?;
    Ok(StreamOutcome { status, written, error_body: Vec::new() })
}

/// Append one read's worth of bytes to `buf`; returns how many arrived (0 = EOF).
async fn read_more<R>(src: &mut R, buf: &mut Vec<u8>) -> Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut tmp = [0u8; 16 * 1024];
    let n = tokio::time::timeout(STREAM_IDLE_TIMEOUT, src.read(&mut tmp))
        .await
        .context("download stalled")?
        .context("read from the download stream")?;
    buf.extend_from_slice(&tmp[..n]);
    Ok(n)
}

/// Copy a non-chunked body to the sink: to `content_length` if the server gave
/// one, else to EOF (which `Connection: close` guarantees is the body's end).
async fn stream_identity<R, W>(
    src: &mut R,
    prefix: Vec<u8>,
    content_length: Option<u64>,
    sink: &mut W,
) -> Result<u64>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut written = 0u64;
    let mut pending = prefix;
    loop {
        if !pending.is_empty() {
            let take = match content_length {
                Some(total) => pending.len().min((total - written) as usize),
                None => pending.len(),
            };
            sink.write_all(&pending[..take]).await.context("write download")?;
            written += take as u64;
            pending.clear();
        }
        if content_length == Some(written) {
            break;
        }
        if read_more(src, &mut pending).await? == 0 {
            if let Some(total) = content_length {
                if written < total {
                    bail!("download truncated: got {written} of {total} bytes");
                }
            }
            break;
        }
    }
    Ok(written)
}

/// Decode a chunked body incrementally into the sink.
async fn stream_chunked<R, W>(src: &mut R, prefix: Vec<u8>, sink: &mut W) -> Result<u64>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = prefix;
    let mut written = 0u64;
    // Bytes still owed by the chunk currently being copied.
    let mut remaining = 0usize;
    // Whether a chunk's trailing CRLF is still to be consumed.
    let mut expect_crlf = false;

    loop {
        let mut progressed = false;

        if remaining > 0 && !buf.is_empty() {
            let take = remaining.min(buf.len());
            sink.write_all(&buf[..take]).await.context("write download")?;
            written += take as u64;
            buf.drain(..take);
            remaining -= take;
            if remaining == 0 {
                expect_crlf = true;
            }
            progressed = true;
        }

        if remaining == 0 && expect_crlf && buf.len() >= 2 {
            buf.drain(..2);
            expect_crlf = false;
            progressed = true;
        }

        if remaining == 0 && !expect_crlf {
            if let Some(nl) = buf.windows(2).position(|w| w == b"\r\n") {
                let line = String::from_utf8_lossy(&buf[..nl]).to_string();
                let hex = line.split(';').next().unwrap_or("").trim().to_string();
                let Ok(size) = usize::from_str_radix(&hex, 16) else {
                    bail!("malformed chunk size {line:?}");
                };
                buf.drain(..nl + 2);
                if size == 0 {
                    return Ok(written);
                }
                remaining = size;
                progressed = true;
            }
        }

        if !progressed && read_more(src, &mut buf).await? == 0 {
            bail!("connection closed mid-chunk: {written} bytes written");
        }
    }
}

/// Split a raw HTTP/1.1 response into status + body, de-chunking when needed.
fn parse_response(raw: &[u8]) -> Result<Response> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("no HTTP header terminator in response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();

    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .context("no HTTP status line")?;

    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });

    let raw_body = &raw[split + 4..];
    let body = if chunked { dechunk(raw_body)? } else { raw_body.to_vec() };
    Ok(Response { status, body })
}

/// Decode an HTTP/1.1 chunked body (`<hexlen>\r\n<data>\r\n` … `0\r\n\r\n`).
fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    // A missing CRLF means the stream was cut short: keep what we decoded.
    while let Some(nl) = data.windows(2).position(|w| w == b"\r\n") {
        let size_line = String::from_utf8_lossy(&data[..nl]);
        // A chunk-extension (`;name=value`) may follow the length.
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_hex, 16) else {
            bail!("malformed chunk size {size_line:?}");
        };
        data = &data[nl + 2..];
        if size == 0 {
            break;
        }
        if data.len() < size {
            out.extend_from_slice(data);
            break;
        }
        out.extend_from_slice(&data[..size]);
        // Skip the chunk's trailing CRLF.
        data = &data[(size + 2).min(data.len())..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{\"a\":1}");
        assert!(r.ok());
    }

    #[test]
    fn parses_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.body, b"{\"a\":1}");
    }

    #[test]
    fn reports_error_status() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 401);
        assert!(!r.ok());
    }

    #[test]
    fn dechunk_tolerates_truncation() {
        // Stream cut mid-chunk: keep the bytes we did receive rather than erroring.
        assert_eq!(dechunk(b"5\r\nab").unwrap(), b"ab");
    }

    /// Feed the streaming decoders from memory, with `prefix` standing in for
    /// the body bytes that arrived alongside the headers.
    async fn drain_chunked(prefix: &[u8], rest: &[u8]) -> Result<Vec<u8>> {
        let mut src = std::io::Cursor::new(rest.to_vec());
        let mut out = Vec::new();
        stream_chunked(&mut src, prefix.to_vec(), &mut out).await?;
        Ok(out)
    }

    #[tokio::test]
    async fn streams_a_chunked_body_split_across_reads() {
        // Whole body already buffered.
        assert_eq!(
            drain_chunked(b"4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n", b"").await.unwrap(),
            b"abcdefg"
        );
        // Chunk header split from its data, and a chunk split mid-payload.
        assert_eq!(
            drain_chunked(b"4\r\nab", b"cd\r\n3\r\nefg\r\n0\r\n\r\n").await.unwrap(),
            b"abcdefg"
        );
        // Nothing buffered up front at all.
        assert_eq!(
            drain_chunked(b"", b"7\r\nabcdefg\r\n0\r\n\r\n").await.unwrap(),
            b"abcdefg"
        );
    }

    #[tokio::test]
    async fn a_chunked_body_cut_short_is_an_error_not_a_silent_truncation() {
        // Unlike the buffered path, a streamed download must not pass a partial
        // file off as complete — the caller writes it to disk.
        assert!(drain_chunked(b"9\r\nabc", b"").await.is_err());
    }

    #[tokio::test]
    async fn streams_an_identity_body_to_content_length_and_to_eof() {
        let mut src = std::io::Cursor::new(b"cdefg".to_vec());
        let mut out = Vec::new();
        let n = stream_identity(&mut src, b"ab".to_vec(), Some(7), &mut out)
            .await
            .unwrap();
        assert_eq!((n, out.as_slice()), (7, b"abcdefg".as_slice()));

        // No Content-Length: `Connection: close` makes EOF the terminator.
        let mut src = std::io::Cursor::new(b"cdefg".to_vec());
        let mut out = Vec::new();
        let n = stream_identity(&mut src, b"ab".to_vec(), None, &mut out)
            .await
            .unwrap();
        assert_eq!((n, out.as_slice()), (7, b"abcdefg".as_slice()));

        // Short read against a declared length must fail loudly.
        let mut src = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        assert!(stream_identity(&mut src, b"ab".to_vec(), Some(7), &mut out)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn identity_body_stops_at_content_length_ignoring_trailing_bytes() {
        // A keep-alive-style overshoot must not bleed into the saved file.
        let mut src = std::io::Cursor::new(b"cdeXXXX".to_vec());
        let mut out = Vec::new();
        let n = stream_identity(&mut src, b"ab".to_vec(), Some(5), &mut out)
            .await
            .unwrap();
        assert_eq!((n, out.as_slice()), (5, b"abcde".as_slice()));
    }
}
