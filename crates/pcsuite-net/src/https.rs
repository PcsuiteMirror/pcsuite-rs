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
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\n\
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

    // `Connection: close` → EOF marks the end; no Content-Length bookkeeping.
    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).await.context("read response")?;
    parse_response(&raw)
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
}
