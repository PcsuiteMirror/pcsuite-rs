//! mdfs: the in-app file browser plane.
//!
//! The phone's connection server (10380, same port as the control WS) also serves
//! a small HTTP/JSON file-manager API — this is what the desktop app's "files" tab
//! uses, *not* the vdfs (5678) plane. Reversed from the desktop `app.asar`
//! (`adapter.ts`): the renderer POSTs JSON to `/pc_file_manager/*` with two routing
//! headers — `newToken` (the session connect-token) and `deviceId` (the phone's
//! `mobileDeviceId`). The server sniffs the first byte, so plain HTTP works (no TLS
//! needed, same as the USB `/version` handshake) and is robust over adb-forwarded
//! ports.
//!
//! There is **no separate access-authorization gate** here: a connected session can
//! list immediately (unlike the vdfs plane). [`list`] enumerates a media/file
//! category; the resulting `savePath`s feed [`crate::vdfs::fetch`] for download.

use std::io::Write;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};

/// The phone connection server port (control WS + this HTTP API).
pub const CONTROL_PORT: u16 = 10380;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const IO_TIMEOUT: Duration = Duration::from_secs(20);
/// File downloads stream a whole tar; allow much longer than a list request.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// A category of file listing, mapped to the phone's `REQUEST_POSTS_*` type and the
/// `groupBy`/`sortCondition` the desktop app pairs with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    /// Recent files grouped by day (`REQUEST_POSTS_ONE_MOTH_LIST`).
    Recent,
    Image,
    Video,
    Audio,
    /// Generic files (`REQUEST_POSTS_FILELIST`).
    File,
    /// Documents (`REQUEST_POSTS_DOCSLIST`).
    Doc,
    /// Home overview (`REQUEST_POSTS_HOMEDATA`).
    Home,
}

impl ListKind {
    /// Parse the CLI `--type` value.
    pub fn parse(s: &str) -> Option<ListKind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "recent" | "month" | "" => ListKind::Recent,
            "image" | "images" | "photo" | "pic" => ListKind::Image,
            "video" | "videos" => ListKind::Video,
            "audio" | "audios" => ListKind::Audio,
            "file" | "files" => ListKind::File,
            "doc" | "docs" | "document" => ListKind::Doc,
            "home" => ListKind::Home,
            _ => return None,
        })
    }

    /// `(type, groupBy, sortCondition)` — mirrors the desktop request bodies.
    fn params(self) -> (&'static str, u8, u8) {
        match self {
            ListKind::Recent => ("REQUEST_POSTS_ONE_MOTH_LIST", 1, 5),
            ListKind::Image => ("REQUEST_POSTS_IMAGELIST", 0, 5),
            ListKind::Video => ("REQUEST_POSTS_VIDEOLIST", 0, 5),
            ListKind::Audio => ("REQUEST_POSTS_AUDIOLIST", 0, 5),
            ListKind::File => ("REQUEST_POSTS_FILELIST", 0, 5),
            ListKind::Doc => ("REQUEST_POSTS_DOCSLIST", 0, 5),
            ListKind::Home => ("REQUEST_POSTS_HOMEDATA", 0, 0),
        }
    }
}

/// One file/dir entry flattened from a channel response (group markers are dropped).
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// Phone-side absolute path (`savePath`); feed to [`crate::vdfs::fetch`].
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub mime: String,
    /// Modification time in epoch milliseconds (0 if absent).
    pub date_ms: i64,
    /// Media duration in ms (audio/video; 0 otherwise).
    pub duration_ms: i64,
    /// Source bucket/album name (`dirName`), when grouped.
    pub dir_name: String,
}

/// List a phone file/media category. `host` is the control host (USB: `127.0.0.1`
/// with 10380 adb-forwarded; LAN: the phone IP). `token` is the session connect
/// token; `device_id` is the phone's `mobileDeviceId`.
pub async fn list(
    host: &str,
    token: &str,
    device_id: &str,
    kind: ListKind,
    page_number: u32,
) -> Result<Vec<Entry>> {
    let (req_type, group_by, sort) = kind.params();
    let body = json!({
        "type": req_type,
        "pageIndex": 0,
        "pageNumber": page_number,
        "sortCondition": sort,
        "groupBy": group_by,
        "category": "",
        "data": "",
        "fileCount": 0,
    });
    let v = post_json(host, token, device_id, "/pc_file_manager/channel", &body)
        .await
        .with_context(|| format!("mdfs list {req_type}"))?;
    Ok(flatten(&v))
}

/// Resolve a virtual file-manager id (from `HOMEDATA`/tab entries) to a phone path.
pub async fn get_path(host: &str, token: &str, device_id: &str, id: &str) -> Result<String> {
    let v = post_json(
        host,
        token,
        device_id,
        "/pc_file_manager/get_path",
        &json!({"id": id, "path_type": ""}),
    )
    .await
    .context("mdfs get_path")?;
    Ok(v.get("result_path")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string())
}

/// `POST /version` on the control HTTP plane, returning the raw body (the phone
/// answers `0000` when its side is ready).
///
/// This is the official desktop's `checkVersion` — the *first* thing it does with a
/// new session, before `/base-info` and before opening the control WS (three retries,
/// the third through its own service proxy). The USB path has always done it; the LAN
/// path skipped it, which leaves the phone's own app short of the handshake it expects
/// after it asked this PC to connect.
pub async fn post_version(host: &str, token: &str, body: &Value) -> Result<String> {
    let payload = serde_json::to_vec(body)?;
    let (status, resp) = http_request(
        host,
        CONTROL_PORT,
        "POST",
        "/version",
        token,
        "",
        Some(&payload),
        IO_TIMEOUT,
    )
    .await?;
    let text = String::from_utf8_lossy(&resp).trim().to_string();
    if status != 200 {
        bail!("/version -> HTTP {status}: {}", text.chars().take(160).collect::<String>());
    }
    Ok(text)
}

/// POST a JSON body to a `/pc_file_manager/*` route and parse the JSON reply.
pub async fn post_json(
    host: &str,
    token: &str,
    device_id: &str,
    route: &str,
    body: &Value,
) -> Result<Value> {
    let payload = serde_json::to_vec(body)?;
    let (status, resp) =
        http_request(host, CONTROL_PORT, "POST", route, token, device_id, Some(&payload), IO_TIMEOUT)
            .await?;
    if status != 200 {
        let snippet: String = String::from_utf8_lossy(&resp).trim().chars().take(160).collect();
        bail!("mdfs {route} -> HTTP {status}: {snippet}");
    }
    serde_json::from_slice(&resp)
        .with_context(|| format!("mdfs {route}: reply was not JSON ({} bytes)", resp.len()))
}

/// Download one phone file by path over the mdfs HTTP plane — the path the desktop
/// app uses, which works without the vdfs (5678) serving gate. Two steps:
///   1. `POST /pc_file_manager/download_info?id=<id>` registers the request
///      (`downloadList` = the phone paths, `total` = summed size).
///   2. `GET /pc_file_manager/download?id=<id>` streams a **tar** of those files.
/// Returns the bytes of the single requested file (the first tar entry).
pub async fn download(host: &str, token: &str, device_id: &str, save_path: &str) -> Result<Vec<u8>> {
    // A unique per-request transfer id correlates the two calls (the desktop app uses
    // an arbitrary id; the phone only needs the same value on both requests).
    let id = format!("pcsuite-{:08x}", fnv1a(save_path.as_bytes()));
    let tar = download_tar(host, token, device_id, &id, &[save_path], 0).await?;
    untar_first(&tar).with_context(|| format!("download tar had no file entry for {save_path}"))
}

/// Download a whole batch of phone files (phone→PC「快传」flow): registers under
/// the caller's transfer `id` with the declared summed size, streams the tar, and
/// extracts **every** entry as `(tar entry name, bytes)`. The official client
/// echoes the same `id` in the `TRANS_FILE_SUCCESS:`/`TRANS_FILE_CANCEL:` receipt,
/// so the caller mints it (see [`new_transfer_id`]) and keeps it.
pub async fn download_batch(
    host: &str,
    token: &str,
    device_id: &str,
    id: &str,
    paths: &[String],
    total: u64,
) -> Result<Vec<(String, Vec<u8>)>> {
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let tar = download_tar(host, token, device_id, id, &refs, total).await?;
    untar_all(&tar)
}

/// Mint a transfer id for one `download_info`/`download` pair. The official
/// client uses a cuid; the phone only needs it unique and identical on both
/// requests (and in the receipt), so a v4 UUID is fine.
pub fn new_transfer_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The two-step mdfs download (`download_info` + `download`), returning the raw
/// tar bytes. `id` correlates the two requests (any unique string); `total` is the
/// summed size the phone announced (0 is accepted by the `pull` path).
pub async fn download_tar(
    host: &str,
    token: &str,
    device_id: &str,
    id: &str,
    paths: &[&str],
    total: u64,
) -> Result<Vec<u8>> {
    // Body verbatim from the official Windows client (2026-09-12 log); the four
    // trailing keys are constant for a plain file batch.
    let info = json!({
        "downloadList": paths,
        "type": "FROM_PC_FILE_MANAGER",
        "total": total,
        "isDir": false,
        "isAlbum": false,
        "albumList": [],
        "dirList": [],
    });
    let (st, resp) = http_request(
        host,
        CONTROL_PORT,
        "POST",
        &format!("/pc_file_manager/download_info?id={id}"),
        token,
        device_id,
        Some(&serde_json::to_vec(&info)?),
        IO_TIMEOUT,
    )
    .await
    .context("mdfs download_info")?;
    if st != 200 {
        let snippet: String = String::from_utf8_lossy(&resp).trim().chars().take(160).collect();
        bail!("mdfs download_info -> HTTP {st}: {snippet}");
    }

    let (st2, body) = http_request(
        host,
        CONTROL_PORT,
        "GET",
        &format!("/pc_file_manager/download?id={id}"),
        token,
        device_id,
        None,
        DOWNLOAD_TIMEOUT,
    )
    .await
    .context("mdfs download")?;
    if st2 != 200 {
        let snippet: String = String::from_utf8_lossy(&body).trim().chars().take(160).collect();
        bail!("mdfs download -> HTTP {st2}: {snippet}");
    }
    Ok(body)
}

/// FNV-1a 32-bit — a tiny stable hash for the transfer id (no extra deps).
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ---------------------------------------------------------------------------
// Upload (PC → phone): drop_files_info + /upload/drop_file_to_phone (tar push)
// ---------------------------------------------------------------------------

/// Registration step timeout (matches the official app's upload-register budget).
const UPLOAD_REGISTER_TIMEOUT: Duration = Duration::from_secs(300);
/// Final server ack can lag while the phone finishes untarring/scanning media —
/// the official app allows an hour. No read-idle timeout applies mid-transfer
/// (the connection is legitimately silent while we stream).
const UPLOAD_ACK_TIMEOUT: Duration = Duration::from_secs(3600);
/// Flush the chunked body at roughly this granularity.
const CHUNK_TARGET: usize = 64 * 1024;

/// One local file queued for upload. Directories are not supported (v1).
#[derive(Debug, Clone)]
pub struct UploadItem {
    /// Local filesystem path of a regular file.
    pub local: std::path::PathBuf,
    /// Tar entry name = basename (flat, no directory hierarchy).
    pub name: String,
    pub size: u64,
    /// Modification time, epoch ms (0 if unknown).
    pub mtime_ms: i64,
}

/// Upload local files to the phone (desktop app's drag-and-drop path). Two steps
/// on the 10380 gateway:
///   1. `POST /pc_file_manager/drop_files_info` — registers the batch
///      (`TO_PC_FILE_MANAGER`, per-item metadata, `ifDuplicated` policy).
///   2. `POST /upload/drop_file_to_phone?id=<same>&type=tar` — streams a single
///      ustar tar (one entry per file, flat basenames) as HTTP chunked, *without*
///      pre-building it in memory.
/// `save_dir` is the phone-side target directory ("" = phone default; observed
/// `Download/vivo办公套件/` on a V2505A).
/// `overwrite` picks the `ifDuplicated` policy (`overwrite` vs. `rename`).
/// Success requires `HTTP 200` **and** a JSON ack with `status == 0`.
pub async fn upload_files(
    host: &str,
    token: &str,
    device_id: &str,
    save_dir: &str,
    overwrite: bool,
    items: &[UploadItem],
) -> Result<()> {
    if items.is_empty() {
        bail!("nothing to upload");
    }
    // The tar stream must not sit in memory: build it on the fly over a blocking
    // socket on a blocking thread (tokio's TcpStream has no sync Write impl).
    let host = host.to_string();
    let token = token.to_string();
    let device_id = device_id.to_string();
    let save_dir = save_dir.to_string();
    let items = items.to_vec();
    tokio::task::spawn_blocking(move || {
        upload_blocking(&host, &token, &device_id, &save_dir, overwrite, &items)
    })
    .await
    .context("upload task join")?
}

fn upload_blocking(
    host: &str,
    token: &str,
    device_id: &str,
    save_dir: &str,
    overwrite: bool,
    items: &[UploadItem],
) -> Result<()> {
    use std::io::Write;
    use std::net::TcpStream as StdTcpStream;

    let addr = (host, CONTROL_PORT);
    let connect = || -> Result<StdTcpStream> {
        let s = StdTcpStream::connect_timeout(
            &std::net::ToSocketAddrs::to_socket_addrs(&addr)?
                .next()
                .with_context(|| format!("resolve {host}:{CONTROL_PORT}"))?,
            CONNECT_TIMEOUT,
        )
        .with_context(|| format!("connect {host}:{CONTROL_PORT}"))?;
        s.set_nodelay(true).ok();
        Ok(s)
    };

    // --- Step 1: register the drop batch ---
    let id = uuid::Uuid::new_v4().to_string();
    let policy = if overwrite { "overwrite" } else { "rename" };
    let drop_items: Vec<Value> = items
        .iter()
        .map(|it| {
            json!({
                "index": -1,
                "isDirectory": false,
                "fileName": it.name,
                "fileSize": it.size,
                "savePath": "",
                "date": it.mtime_ms,
                "fileType": "UNKNOWN",
                "mimeType": "",
                "ifDuplicated": policy,
            })
        })
        .collect();
    let info = json!({
        "id": id,
        "type": "TO_PC_FILE_MANAGER",
        "savePath": save_dir,
        "totalSize": items.iter().map(|it| it.size).sum::<u64>(),
        "totalCount": items.len(),
        "screen_w": 0,
        "screen_h": 0,
        "x": 0,
        "y": 0,
        "dropFileItems": drop_items,
    });
    let payload = serde_json::to_vec(&info)?;

    let mut s = connect()?;
    let head = format!(
        "POST /pc_file_manager/drop_files_info HTTP/1.1\r\nHost: {host}:{CONTROL_PORT}\r\n\
         newToken: {token}\r\ndeviceId: {device_id}\r\nX-ES-HTTP-VERSION: 1\r\n\
         Accept: application/json\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(&payload)?;
    s.flush()?;
    let buf = read_http_response(&mut s, UPLOAD_REGISTER_TIMEOUT).context("drop_files_info reply")?;
    let (status, resp) = parse_http(&buf);
    if status != 200 {
        // Failure replies are plain text (e.g. "is not valid save directory.").
        let body = String::from_utf8_lossy(&resp).trim().to_string();
        bail!("drop_files_info -> HTTP {status}: {body}");
    }
    tracing::info!(id = %id, files = items.len(), "drop batch registered");

    // --- Step 2: stream the tar as HTTP chunked ---
    let mut s = connect()?;
    let head = format!(
        "POST /upload/drop_file_to_phone?id={id}&type=tar HTTP/1.1\r\n\
         Host: {host}:{CONTROL_PORT}\r\n\
         newToken: {token}\r\ndeviceId: {device_id}\r\nX-ES-HTTP-VERSION: 1\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    s.write_all(head.as_bytes())?;
    {
        let cw = ChunkedWriter::new(&mut s);
        let mut builder = tar::Builder::new(cw);
        for it in items {
            // Real stat for mode/mtime; the crate emits GNU longname/pax headers
            // itself for names over 100 bytes (e.g. CJK basenames).
            builder
                .append_path_with_name(&it.local, &it.name)
                .with_context(|| format!("tar append {}", it.local.display()))?;
            tracing::info!(name = %it.name, size = it.size, "…streaming");
        }
        let cw = builder.into_inner().context("tar finish")?;
        cw.finish().context("chunked terminator")?;
    }
    s.flush()?;

    // The phone untars on the fly, then acks; media scanning runs by itself.
    let buf = read_http_response(&mut s, UPLOAD_ACK_TIMEOUT).context("upload reply")?;
    let (status, resp) = parse_http(&buf);
    let ack: Option<Value> = serde_json::from_slice(&resp).ok();
    let ok = status == 200 && ack.as_ref().and_then(|v| v.get("status")).and_then(Value::as_i64) == Some(0);
    if !ok {
        let body = String::from_utf8_lossy(&resp).trim().chars().take(200).collect::<String>();
        bail!("drop_file_to_phone -> HTTP {status}: {body}");
    }
    Ok(())
}

/// Read one HTTP response from a blocking socket without assuming the peer closes
/// the connection: `Connection: close` is honored by the phone's *other* routes,
/// but the upload handlers answer and keep the socket open, so a bare
/// `read_to_end` would stall until the deadline and throw away a perfectly good
/// reply. Returns as soon as the buffered bytes form a complete response
/// (headers + Content-Length body / chunk terminator), on EOF, or when
/// `overall` elapses (error only if nothing arrived at all).
fn read_http_response(s: &mut std::net::TcpStream, overall: Duration) -> Result<Vec<u8>> {
    use std::io::Read;
    let deadline = std::time::Instant::now() + overall;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok(); // poll granularity, not an idle cap
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    loop {
        if http_response_complete(&buf) {
            return Ok(buf);
        }
        if std::time::Instant::now() >= deadline {
            if buf.is_empty() {
                bail!("no reply within {overall:?}");
            }
            return Ok(buf); // partial/bare reply — let parse_http judge
        }
        match s.read(&mut tmp) {
            Ok(0) => return Ok(buf), // EOF: peer closed
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e).context("read reply"),
        }
    }
}

/// Is `buf` a full HTTP response? Header block + body per `Content-Length` or the
/// chunked terminator. A bare non-HTTP reply (the router's plain `NotFound`
/// style) counts as complete on any data; a header block with no length hint is
/// treated as complete (acks are JSON with a length or chunked).
fn http_response_complete(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    let Some(sep) = find(buf, b"\r\n\r\n") else {
        return !buf.starts_with(b"HTTP/");
    };
    let head = String::from_utf8_lossy(&buf[..sep]).to_lowercase();
    let body = &buf[sep + 4..];
    if head
        .lines()
        .any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"))
    {
        return find(body, b"\r\n0\r\n\r\n").is_some() || body.starts_with(b"0\r\n\r\n");
    }
    if let Some(cl) = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
    {
        return body.len() >= cl;
    }
    true
}

/// `std::io::Write` adapter that frames each buffered write as an HTTP/1.1 chunk
/// (`%x\r\n<data>\r\n`) straight onto the underlying stream; [`ChunkedWriter::finish`]
/// emits the `0\r\n\r\n` terminator. Lets `tar::Builder` stream onto the socket
/// without materializing the archive.
struct ChunkedWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> ChunkedWriter<W> {
    fn new(inner: W) -> Self {
        ChunkedWriter {
            inner,
            buf: Vec::with_capacity(CHUNK_TARGET * 2),
        }
    }

    /// Flush any buffered data as one chunk, then write the terminal zero chunk.
    fn finish(mut self) -> std::io::Result<()> {
        self.flush_buf()?;
        self.inner.write_all(b"0\r\n\r\n")?;
        self.inner.flush()
    }

    fn flush_buf(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let head = format!("{:x}\r\n", self.buf.len());
        self.inner.write_all(head.as_bytes())?;
        self.inner.write_all(&self.buf)?;
        self.inner.write_all(b"\r\n")?;
        self.buf.clear();
        Ok(())
    }
}

impl<W: Write> Write for ChunkedWriter<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK_TARGET {
            self.flush_buf()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_buf()?;
        self.inner.flush()
    }
}

/// One plain-HTTP request to the connection server, with the `newToken`/`deviceId`
/// routing headers. `Connection: close` lets us read the whole body to EOF (mirrors
/// the USB `/version` handshake). Returns `(status, body)`; `status == 0` means the
/// reply was not HTTP (e.g. a bare `NotFound` from the router when a header is wrong).
async fn http_request(
    host: &str,
    port: u16,
    method: &str,
    route: &str,
    token: &str,
    device_id: &str,
    body: Option<&[u8]>,
    read_timeout: Duration,
) -> Result<(u16, Vec<u8>)> {
    let mut head = format!(
        "{method} {route} HTTP/1.1\r\nHost: {host}:{port}\r\n\
         newToken: {token}\r\ndeviceId: {device_id}\r\n\
         Accept: application/json\r\nConnection: close\r\n"
    );
    if let Some(b) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    head.push_str("\r\n");

    let mut s = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .with_context(|| format!("connect {host}:{port} timed out"))?
        .with_context(|| format!("connect {host}:{port}"))?;
    s.set_nodelay(true).ok();
    s.write_all(head.as_bytes()).await?;
    if let Some(b) = body {
        s.write_all(b).await?;
    }
    s.flush().await?;

    // Read until the reply is *complete* rather than until the socket closes: the
    // phone keeps these connections alive despite `Connection: close`, so waiting for
    // EOF burns the whole timeout on every call. That cost the phone-initiated connect
    // its session — `/version` answered in milliseconds but stalled the caller 20s, by
    // which time the phone had given up and closed 10380.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + read_timeout;
    while !http_reply_complete(&buf) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, s.read(&mut chunk)).await {
            Ok(Ok(0)) => break, // EOF: whatever arrived is all there is
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    Ok(parse_http(&buf))
}

/// Whether `buf` already holds a whole HTTP reply: headers, plus either the
/// `Content-Length` bytes or a chunked terminator. A reply with neither header (the
/// phone closes the socket to mark the end) never reads complete, so the caller falls
/// back to EOF/timeout.
fn http_reply_complete(buf: &[u8]) -> bool {
    let Some(sep) = find(buf, b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..sep]).to_ascii_lowercase();
    let body_len = buf.len() - (sep + 4);
    if let Some(len) = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        return body_len >= len;
    }
    if head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked")) {
        return find(&buf[sep + 4..], b"0\r\n\r\n").is_some();
    }
    false
}

/// Split an HTTP response into `(status, body)`, de-chunking a `Transfer-Encoding:
/// chunked` body (the file-download stream uses it). Non-HTTP bytes → `(0, whole)`.
fn parse_http(buf: &[u8]) -> (u16, Vec<u8>) {
    let Some(sep) = find(buf, b"\r\n\r\n") else {
        return (0, buf.to_vec());
    };
    let head = &buf[..sep];
    let raw_body = &buf[sep + 4..];
    let head_str = std::str::from_utf8(head).unwrap_or("");
    let status = head_str
        .lines()
        .next()
        .filter(|line| line.starts_with("HTTP/"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let chunked = head_str
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    let body = if chunked { dechunk(raw_body) } else { raw_body.to_vec() };
    (status, body)
}

/// Decode an HTTP/1.1 chunked body (`<hexlen>\r\n<data>\r\n…0\r\n\r\n`).
fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    loop {
        let Some(eol) = find(b, b"\r\n") else { break };
        let size_hex = std::str::from_utf8(&b[..eol]).unwrap_or("");
        let size = usize::from_str_radix(size_hex.split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        b = &b[eol + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(b.len());
        out.extend_from_slice(&b[..take]);
        b = &b[take..];
        if b.starts_with(b"\r\n") {
            b = &b[2..];
        }
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Extract the first regular-file entry's bytes from a POSIX/ustar tar archive
/// (the file-download stream returns a tar of the requested paths). Skips pax/global
/// extended headers (`x`/`g` type flags); octal size is correct for files < 8 GiB.
fn untar_first(data: &[u8]) -> Result<Vec<u8>> {
    let mut off = 0usize;
    while off + 512 <= data.len() {
        let hdr = &data[off..off + 512];
        if hdr.iter().all(|&b| b == 0) {
            break; // end-of-archive marker
        }
        let size = tar_octal(&hdr[124..136]);
        let typeflag = hdr[156];
        off += 512;
        let data_blocks = size.div_ceil(512) * 512;
        // Regular file: typeflag '0' or NUL. Skip extended headers ('x'/'g') and others.
        if (typeflag == b'0' || typeflag == 0) && size > 0 {
            let end = (off + size).min(data.len());
            return Ok(data[off..end].to_vec());
        }
        off += data_blocks;
    }
    bail!("no regular file entry in tar ({} bytes)", data.len())
}

/// Extract **every** regular-file entry from a tar archive as `(name, bytes)`
/// (the phone→PC「快传」batch ships one entry per requested path). Handles GNU
/// longname entries (`L`) so CJK/超长文件名 keep their real name; skips pax/global
/// extended headers (`x`/`g`) and directories. Zero-length files are kept.
pub fn untar_all(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    let mut longname: Option<String> = None;
    while off + 512 <= data.len() {
        let hdr = &data[off..off + 512];
        if hdr.iter().all(|&b| b == 0) {
            break; // end-of-archive marker
        }
        let size = tar_octal(&hdr[124..136]);
        let typeflag = hdr[156];
        off += 512;
        let data_blocks = size.div_ceil(512) * 512;
        match typeflag {
            // GNU longname: this entry's data is the next entry's real name.
            b'L' => {
                let end = (off + size).min(data.len());
                let raw = &data[off..end];
                let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                longname = Some(String::from_utf8_lossy(&raw[..nul]).into_owned());
            }
            b'0' | 0 => {
                let name = longname.take().unwrap_or_else(|| tar_name(hdr));
                let end = (off + size).min(data.len());
                out.push((name, data[off..end].to_vec()));
            }
            _ => {} // 'x'/'g' extended headers, dirs ('5'), links, … — skip
        }
        off += data_blocks;
    }
    if out.is_empty() {
        bail!("no regular file entry in tar ({} bytes)", data.len());
    }
    Ok(out)
}

/// Read the plain name field of a tar header (up to the first NUL).
fn tar_name(hdr: &[u8]) -> String {
    let raw = &hdr[..100];
    let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..nul]).into_owned()
}

/// Parse a tar header octal field (space/NUL terminated).
fn tar_octal(field: &[u8]) -> usize {
    let s: String = field
        .iter()
        .take_while(|&&c| c != 0 && c != b' ')
        .map(|&c| c as char)
        .collect();
    usize::from_str_radix(s.trim(), 8).unwrap_or(0)
}

/// Walk a channel reply and collect every file/dir entry, regardless of whether the
/// payload is flat (`{dataList:[{...}],totalCount}`) or grouped
/// (`{dataList:[{dataList:{"bucket | type":[{...}]},time,dateFormat}]}`). Group
/// markers (`isGroup:true`, or rows with neither a name nor a path) are skipped.
fn flatten(v: &Value) -> Vec<Entry> {
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

fn walk(v: &Value, out: &mut Vec<Entry>) {
    match v {
        Value::Array(a) => {
            for x in a {
                walk(x, out);
            }
        }
        Value::Object(o) => {
            let is_group = o.get("isGroup").and_then(Value::as_bool).unwrap_or(false);
            let name = o.get("fileName").and_then(Value::as_str).unwrap_or("");
            let path = o.get("savePath").and_then(Value::as_str).unwrap_or("");
            if !is_group && (!name.is_empty() || !path.is_empty()) {
                out.push(Entry {
                    name: name.to_string(),
                    path: path.to_string(),
                    size: o.get("fileSize").and_then(Value::as_u64).unwrap_or(0),
                    is_dir: o.get("isDirectory").and_then(Value::as_bool).unwrap_or(false),
                    mime: o.get("mimeType").and_then(Value::as_str).unwrap_or("").to_string(),
                    date_ms: o.get("date").and_then(Value::as_i64).unwrap_or(0),
                    duration_ms: o.get("duration").and_then(Value::as_i64).unwrap_or(0),
                    dir_name: o.get("dirName").and_then(Value::as_str).unwrap_or("").to_string(),
                });
                return; // a leaf entry has no nested entries
            }
            for val in o.values() {
                walk(val, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_complete_needs_the_whole_body() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n";
        assert!(!http_reply_complete(head));
        assert!(!http_reply_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":"));
        assert!(http_reply_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}"));
        // chunked: complete only at the terminator
        let c = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n";
        assert!(!http_reply_complete(c));
        assert!(http_reply_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"));
        // no length and not chunked → only EOF can end it
        assert!(!http_reply_complete(b"HTTP/1.1 200 OK\r\n\r\nbody"));
    }

    #[test]
    fn parse_http_status_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        let (status, body) = parse_http(raw);
        assert_eq!(status, 200);
        assert_eq!(body, b"{\"a\":1}");
    }

    #[test]
    fn parse_http_non_http_is_zero() {
        let (status, body) = parse_http(b"NotFound");
        assert_eq!(status, 0);
        assert_eq!(body, b"NotFound");
    }

    #[test]
    fn flatten_flat_list() {
        let v: Value = serde_json::from_str(
            r#"{"dataList":[{"duration":768,"date":1780543699000,"fileName":"v.mp4","fileSize":2367687,"isDirectory":false,"savePath":"/storage/emulated/0/DCIM/Camera/v.mp4"}],"totalCount":1}"#,
        )
        .unwrap();
        let e = flatten(&v);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "v.mp4");
        assert_eq!(e[0].path, "/storage/emulated/0/DCIM/Camera/v.mp4");
        assert_eq!(e[0].size, 2367687);
        assert_eq!(e[0].duration_ms, 768);
    }

    #[test]
    fn dechunk_basic() {
        // "Wiki" + "pedia" in two chunks, then terminator.
        let raw = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(dechunk(raw), b"Wikipedia");
    }

    #[test]
    fn untar_first_extracts_single_file() {
        // Build a minimal ustar archive: 512 header + one 5-byte file + padding + 2 zero blocks.
        let mut tar = vec![0u8; 512];
        let name = b"hello.txt";
        tar[..name.len()].copy_from_slice(name);
        tar[124..136].copy_from_slice(b"00000000005\0"); // size field = octal 5
        tar[156] = b'0'; // regular file
        tar[257..262].copy_from_slice(b"ustar");
        let mut block = vec![0u8; 512];
        block[..5].copy_from_slice(b"world");
        tar.extend_from_slice(&block);
        tar.extend_from_slice(&[0u8; 1024]); // end-of-archive
        assert_eq!(untar_first(&tar).unwrap(), b"world");
    }

    #[test]
    fn flatten_grouped_skips_markers() {
        let v: Value = serde_json::from_str(
            r#"{"dataList":[
              {"groupCount":1,"groupName":"今天","isGroup":true,"date":-1,"fileName":"","fileSize":0,"savePath":""},
              {"dataList":{"截屏 | 图片":[{"dirName":"截屏","mimeType":"image/jpeg","date":1781656159000,"fileName":"s.jpg","fileSize":100,"isDirectory":false,"savePath":"/storage/emulated/0/Pictures/Screenshots/s.jpg"}]},"time":"今天","dateFormat":"2026/06/17 13:10"}
            ]}"#,
        )
        .unwrap();
        let e = flatten(&v);
        assert_eq!(e.len(), 1, "group marker should be skipped, one real entry kept");
        assert_eq!(e[0].name, "s.jpg");
        assert_eq!(e[0].dir_name, "截屏");
        assert_eq!(e[0].mime, "image/jpeg");
    }

    #[test]
    fn chunked_writer_frames_chunks() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut cw = ChunkedWriter::new(&mut out);
            cw.write_all(b"Wiki").unwrap();
            cw.write_all(b"pedia").unwrap();
            cw.flush().unwrap(); // explicit flush frames "Wikipedia" as one chunk
            cw.write_all(b"!").unwrap();
            cw.finish().unwrap();
        }
        assert_eq!(out, b"9\r\nWikipedia\r\n1\r\n!\r\n0\r\n\r\n");
        // …and the framed body round-trips through dechunk.
        assert_eq!(dechunk(&out), b"Wikipedia!");
    }

    #[test]
    fn chunked_writer_splits_large_writes() {
        let mut out: Vec<u8> = Vec::new();
        let big = vec![7u8; CHUNK_TARGET + 10];
        {
            let mut cw = ChunkedWriter::new(&mut out);
            cw.write_all(&big).unwrap(); // crosses the flush threshold internally
            cw.finish().unwrap();
        }
        // One oversize chunk (the whole buffered write), then the terminator.
        assert!(out.starts_with(format!("{:x}\r\n", big.len()).as_bytes()));
        assert!(out.ends_with(b"\r\n0\r\n\r\n"));
        assert_eq!(dechunk(&out), big);
    }

    /// Build a minimal ustar archive from `(name, bytes)` pairs (+ optional GNU
    /// longname for the second entry).
    fn make_tar(entries: &[(&str, &[u8])], longname: Option<&str>) -> Vec<u8> {
        let mut tar = Vec::new();
        let mut push_entry = |name: &str, data: &[u8], typeflag: u8| {
            let mut hdr = vec![0u8; 512];
            hdr[..name.len()].copy_from_slice(name.as_bytes());
            let sz = format!("{:011o}\0", data.len());
            hdr[124..136].copy_from_slice(sz.as_bytes());
            hdr[156] = typeflag;
            hdr[257..262].copy_from_slice(b"ustar");
            tar.extend_from_slice(&hdr);
            let mut block = vec![0u8; data.len().div_ceil(512) * 512];
            block[..data.len()].copy_from_slice(data);
            tar.extend_from_slice(&block);
        };
        for (i, (name, data)) in entries.iter().enumerate() {
            if i == 1 {
                if let Some(ln) = longname {
                    push_entry("././@LongLink", ln.as_bytes(), b'L');
                }
            }
            push_entry(name, data, b'0');
        }
        tar.extend_from_slice(&[0u8; 1024]); // end-of-archive
        tar
    }

    #[test]
    fn http_response_complete_detection() {
        assert!(!http_response_complete(b""));
        assert!(!http_response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{\"a\":"));
        assert!(http_response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n{\"a\":1}"));
        assert!(http_response_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"));
        assert!(!http_response_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n"));
        // 裸非 HTTP 回复（路由层 NotFound 风格）一来数据就算完整。
        assert!(http_response_complete(b"NotFound"));
    }

    #[test]
    fn untar_all_extracts_every_entry() {
        let tar = make_tar(&[("a.txt", b"hello"), ("b.txt", b""), ("c.txt", b"world!")], None);
        let got = untar_all(&tar).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], ("a.txt".to_string(), b"hello".to_vec()));
        assert_eq!(got[1], ("b.txt".to_string(), Vec::new()), "zero-length file kept");
        assert_eq!(got[2], ("c.txt".to_string(), b"world!".to_vec()));
    }

    #[test]
    fn untar_all_honors_gnu_longname() {
        let tar = make_tar(&[("a.txt", b"x"), ("short", b"yy")], Some("很长的中文文件名.pdf"));
        let got = untar_all(&tar).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].0, "很长的中文文件名.pdf");
        assert_eq!(got[1].1, b"yy");
    }
}
