//! Cloud transfer (the vendor calls it *freeTransfer*): receive files the phone
//! uploaded to vivo's relay service.
//!
//! This is the third and last way the phone can send files to this machine, and
//! the only one that is not peer-to-peer. The other two live in [`crate::share`]
//! and [`crate::mdfs`] and never touch a server; this one needs a signed-in vivo
//! account on both ends, costs the account's daily upload quota, and the files
//! expire after [`EXPIRY_HOURS`]. It exists because it works when the phone and
//! the PC are not on the same network — the phone uploads, we download later.
//!
//! Everything here is optional. A build with no account configured simply never
//! calls into this module, and the LAN paths are unaffected.
//!
//! # How a transfer is discovered
//!
//! The official client has two intakes and **neither is a background poll**:
//!
//! 1. Opening its transfer-history page queries [`CloudShare::pending_records`].
//! 2. Its push daemon posts `{"event":"freeTransfer","data":…}` into the app's
//!    own local HTTP bus, which hands the payload's `ext` object straight to the
//!    task manager.
//!
//! The push payload carries no file list — the client fetches that with
//! `initDownload` either way. So the push channel is a latency optimisation, not
//! a requirement: querying the record list is enough to receive everything.
//! That is what this module does, and why the MQTT push stack does not have to
//! be ported before cloud receive works.
//!
//! # Wire protocol
//!
//! Full transcription in `docs/CLOUD_TRANSFER_PROTOCOL.md`. In short:
//!
//! ```text
//! POST /api/v1/upload/record     {receiveDeviceType:3, receiveDeviceId, status:[3,8]}  → records
//! POST /api/v1/file/initDownload {taskId, receiveDeviceType:3, receiveDeviceId}        → chunks
//! POST /api/v1/file/download     {fileId, fileName, fileSize, taskId, …}               → raw bytes
//! POST /api/v1/download/complete {taskId}                                              → ack
//! ```
//!
//! `/api/v1/file/download` is a **POST whose response body is the file itself**,
//! not a JSON envelope and not a redirect — which is why it goes through
//! [`https::request_streaming`] rather than the JSON helper.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

use pcsuite_net::https;

use crate::cloud::{self, Account};

/// The fixed string the request signature is computed over. The phone signs its
/// own package name instead; this is the desktop client's.
const CONNECT_NAME: &str = "com.vivo.pcsuite.connect";

/// `source` header: this client is the macOS one. (Windows sends 3.)
const SOURCE_MAC: i64 = 4;

/// `model` header identifying the platform.
const MODEL_MAC: &str = "mac";

/// `receiveDeviceType` in every request body: 3 = PC, the same numbering the
/// connection center uses.
const RECEIVE_DEVICE_TYPE_PC: i64 = 3;

/// Record status: uploaded and waiting to be downloaded.
pub const STATUS_PENDING: i64 = 3;
/// Record status: already downloaded by the receiver.
pub const STATUS_DOWNLOADED: i64 = 7;
/// Record status: cancelled — still re-downloadable, so the history page asks
/// for these alongside [`STATUS_PENDING`].
pub const STATUS_CANCELLED: i64 = 8;

/// How long the relay keeps an uploaded file.
pub const EXPIRY_HOURS: i64 = 72;

/// How many files the official client downloads at once. Matched so that a slow
/// link behaves the way the vendor's server expects.
pub const CONCURRENCY: usize = 3;

/// Host used by the domestic (non-export) build, which does no region mapping.
pub const DOMESTIC_BRIDGE_HOST: &str = "cross-file-bridge.vivo.com";

/// The export build's server rooms, transcribed from `serverRoomMap` in the
/// official client: `(room label, country codes it serves, production host)`.
const ROOMS: &[(&str, &[&str], &str)] = &[
    (
        "asia",
        &[
            "sg", "np", "bd", "lk", "tw", "hk", "pk", "kh", "vn", "ph", "id", "my", "th", "sa",
            "ae", "eg", "ke", "tz", "uz", "ua", "ye", "ng", "jo", "gh", "ci", "iq", "ao", "bt",
            "tn", "pg", "dz", "ma", "af",
        ],
        "asia-cross-file-bridge.vivoglobal.com",
    ),
    ("in", &["in"], "in-cross-file-bridge.vivoglobal.com"),
    ("ru", &["ru", "by"], "ru-cross-file-bridge.vivoglobal.com"),
    (
        "eu1",
        &[
            "pl", "de", "cz", "fr", "es", "it", "rs", "uk", "gb", "ro", "at", "bg", "gr", "pt",
            "ie", "al", "ba", "cy", "hr", "me", "mk", "si", "sk", "hu", "nl",
        ],
        "eu-cross-file-bridge.vivoglobal.com",
    ),
    (
        "eu2",
        &[
            "tr", "au", "co", "pe", "il", "cl", "mx", "za", "br", "pa", "gt", "cr", "ni", "md",
        ],
        "-cross-file-bridge.vivoglobal.com",
    ),
    ("kz", &["kz"], "kz-cross-file-bridge.vivoglobal.com"),
];

/// Pick the relay host for an account's region.
///
/// Mirrors `getFeelFreeTransUrlByCountryCode`: normally the country's server
/// room decides, with three exceptions baked into the original — the UK shares
/// the EU room but gets its own host, and the second EU room is split further
/// into a South Africa, a Latin America and a Brazil host. An unrecognised
/// country falls back to `sg`, exactly as the original does.
pub fn bridge_host(country_code: &str) -> String {
    let cc = country_code.trim().to_ascii_lowercase();
    if cc.is_empty() || cc == "cn" {
        // The domestic build never region-maps. "cn" appears in no server room.
        return DOMESTIC_BRIDGE_HOST.to_string();
    }

    let Some((label, _, prod_host)) = ROOMS.iter().find(|(_, ccs, _)| ccs.contains(&cc.as_str()))
    else {
        return ROOMS[0].2.to_string();
    };

    if *label == "eu2" {
        return match cc.as_str() {
            "za" => "zasc-cross-file-bridge.vivoglobal.com".to_string(),
            "co" | "pe" | "cl" | "pa" | "gt" | "cr" | "ni" | "md" => {
                "sca-cross-file-bridge.vivoglobal.com".to_string()
            }
            "br" => "br-cross-file-bridge.jovimobile.com".to_string(),
            other => format!("{other}{prod_host}"),
        };
    }

    if cc == "uk" || cc == "gb" {
        let bare = prod_host.split_once('-').map(|(_, r)| r).unwrap_or(prod_host);
        return format!("uk-{bare}");
    }

    prod_host.to_string()
}

/// One transfer the phone uploaded for this PC.
#[derive(Clone, Debug, Default)]
pub struct TransferRecord {
    pub task_id: String,
    pub total_count: u32,
    pub total_size: u64,
    /// Upload time in epoch milliseconds; [`EXPIRY_HOURS`] after this the relay
    /// drops the files.
    pub create_time_millis: i64,
    pub download_status: i64,
    /// Display name of the phone that sent it, when the server supplies one.
    pub sender: String,
    /// First file's name — what the official UI shows as the transfer's title.
    pub first_file_name: String,
    /// Server's own verdict on expiry. [`TransferRecord::is_expired`] falls back
    /// to the timestamp when the field is absent.
    pub server_expired: Option<bool>,
}

impl TransferRecord {
    /// Whether the relay has (or is about to have) dropped these files.
    pub fn is_expired(&self) -> bool {
        if let Some(e) = self.server_expired {
            return e;
        }
        if self.create_time_millis <= 0 {
            return false;
        }
        let age_ms = now_millis() - self.create_time_millis;
        age_ms >= EXPIRY_HOURS * 3600 * 1000
    }
}

/// One file inside a transfer, as `initDownload` describes it.
#[derive(Clone, Debug, Default)]
pub struct CloudFile {
    pub file_id: String,
    pub file_name: String,
    pub file_size: u64,
    /// Directory of this file relative to the save root; empty for a flat file.
    pub file_path: String,
    pub is_folder: bool,
    /// Already fetched in an earlier attempt — the official client skips these.
    pub has_download: bool,
    /// Echoed back verbatim in the download request; meaning is the server's.
    pub send_source: Value,
    /// Echoed back verbatim in the download request; meaning is the server's.
    pub send_source_entry: Value,
}

/// Progress reported while receiving.
#[derive(Clone, Debug)]
pub enum CloudEvent {
    /// A transfer was picked up and its file list fetched.
    Started { task_id: String, files: Vec<String> },
    /// One file finished and was written under `dir`.
    FileDone { task_id: String, name: String, bytes: u64 },
    /// Every file of the transfer arrived; the relay was told it can forget it.
    Done { task_id: String, files: Vec<String>, dir: String },
    /// The transfer did not complete. The record stays on the server, so a later
    /// attempt can retry it.
    Failed { task_id: String, error: String },
}

impl CloudEvent {
    /// Serialize in the same shape [`crate::filetrans::FileTransEvent`] uses, so
    /// the app can feed cloud transfers and the two LAN paths into one handler.
    /// `source` and `taskId` are extra keys that existing readers ignore.
    ///
    /// `None` for [`CloudEvent::FileDone`]: the shared shape has no per-file
    /// event, and the UI reports batches, so forwarding it would only add noise.
    pub fn to_file_trans_json(&self) -> Option<String> {
        let v = match self {
            CloudEvent::Started { task_id, files } => {
                json!({"type": "started", "files": files, "source": "cloud", "taskId": task_id})
            }
            CloudEvent::Done { task_id, files, dir } => {
                json!({"type": "done", "files": files, "dir": dir,
                       "source": "cloud", "taskId": task_id})
            }
            CloudEvent::Failed { task_id, error } => {
                json!({"type": "failed", "files": [], "error": error,
                       "source": "cloud", "taskId": task_id})
            }
            CloudEvent::FileDone { .. } => return None,
        };
        Some(v.to_string())
    }
}

/// A configured cloud-transfer client.
pub struct CloudShare {
    account: Account,
    device_id: String,
    host: String,
    system_version: String,
}

impl CloudShare {
    /// Build a client for `account`, choosing the relay host from its region and
    /// deriving this machine's device id (the same one the connection center and
    /// the phone's device list know it by).
    pub fn new(account: Account) -> Result<Self> {
        if !account.is_complete() {
            bail!("not signed in: cloud transfer needs an openId + token (run the QR login)");
        }
        let host = bridge_host(&account.country_code);
        Ok(Self {
            device_id: cloud::pc_device_id()?,
            account,
            host,
            system_version: host_system_version(),
        })
    }

    /// Point the client at a different relay host than the region table picked.
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The nine headers every cloud-transfer call carries.
    ///
    /// The signature is recomputed per call because it is bound to the timestamp
    /// in the same header set — the server checks one against the other.
    fn headers(&self) -> Vec<(String, String)> {
        let ts = now_millis();
        vec![
            ("openId".into(), self.account.open_id.clone()),
            ("token".into(), self.account.token.clone()),
            ("source".into(), SOURCE_MAC.to_string()),
            ("timestamp".into(), ts.to_string()),
            ("sign".into(), request_sign(ts)),
            ("deviceId".into(), self.device_id.clone()),
            ("model".into(), MODEL_MAC.into()),
            ("systemVersion".into(), self.system_version.clone()),
            ("appVersion".into(), cloud::CLIENT_VERSION.into()),
        ]
    }

    /// Send one JSON call and unwrap the `{code, msg, data}` envelope.
    async fn call(&self, method: &str, path: &str, body: Value) -> Result<Value> {
        let encoded = serde_json::to_vec(&body)?;
        let res = https::request(&self.host, method, path, &self.headers(), Some(&encoded)).await?;

        if !res.ok() {
            bail!("{method} {path} failed: HTTP {}", res.status);
        }
        let v = res.json()?;
        let code = v.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            let msg = v.get("msg").and_then(Value::as_str).unwrap_or("");
            bail!("{path}: {}", describe_code(code, msg));
        }
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Transfers waiting for this PC.
    ///
    /// `statuses` selects which records to ask for; the official history page
    /// sends `[3, 8]` (waiting plus cancelled-but-retryable) and its connection
    /// page sends `[3]` just to decide whether to show a badge.
    pub async fn records(&self, statuses: &[i64]) -> Result<Vec<TransferRecord>> {
        let data = self
            .call(
                "POST",
                "/api/v1/upload/record",
                json!({
                    "receiveDeviceType": RECEIVE_DEVICE_TYPE_PC,
                    "receiveDeviceId": self.device_id,
                    "status": statuses,
                }),
            )
            .await?;

        let list = data.as_array().cloned().unwrap_or_default();
        Ok(list.iter().map(parse_record).collect())
    }

    /// Transfers that are waiting *and* have not expired — what a receiver
    /// actually wants to act on.
    pub async fn pending_records(&self) -> Result<Vec<TransferRecord>> {
        let all = self.records(&[STATUS_PENDING, STATUS_CANCELLED]).await?;
        Ok(all.into_iter().filter(|r| !r.is_expired()).collect())
    }

    /// Fetch the file list for one transfer. Files already downloaded in an
    /// earlier attempt are filtered out, so an empty result means "nothing left".
    pub async fn download_init(&self, task_id: &str) -> Result<Vec<CloudFile>> {
        let data = self
            .call(
                "POST",
                "/api/v1/file/initDownload",
                json!({
                    "taskId": task_id,
                    "receiveDeviceType": RECEIVE_DEVICE_TYPE_PC,
                    "receiveDeviceId": self.device_id,
                }),
            )
            .await?;

        let Some(chunks) = data.get("chunks").and_then(Value::as_array) else {
            bail!("initDownload returned no file list for task {task_id}");
        };
        Ok(chunks
            .iter()
            .map(parse_file)
            .filter(|f| !f.has_download)
            .collect())
    }

    /// Download one file's bytes straight to `dest`. Returns the byte count.
    ///
    /// There is no range/resume support in the vendor's protocol: a failure
    /// means the whole file is fetched again.
    pub async fn download_file(&self, task_id: &str, file: &CloudFile, dest: &Path) -> Result<u64> {
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("create {}", dir.display()))?;
        }
        let body = serde_json::to_vec(&json!({
            "fileName": file.file_name,
            "fileId": file.file_id,
            "taskId": task_id,
            "fileSize": file.file_size,
            "sendSourceEntry": file.send_source_entry,
            "sendSource": file.send_source,
        }))?;

        let mut out = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("create {}", dest.display()))?;
        let outcome = https::request_streaming(
            &self.host,
            "POST",
            "/api/v1/file/download",
            &self.headers(),
            Some(&body),
            &mut out,
        )
        .await;

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => {
                tokio::fs::remove_file(dest).await.ok();
                return Err(e);
            }
        };
        if !(200..300).contains(&outcome.status) {
            tokio::fs::remove_file(dest).await.ok();
            let detail = String::from_utf8_lossy(&outcome.error_body);
            bail!(
                "download of {} failed: HTTP {} {}",
                file.file_name,
                outcome.status,
                detail.trim()
            );
        }
        Ok(outcome.written)
    }

    /// Tell the relay the whole transfer arrived, so it stops offering it.
    ///
    /// Only call this when every file succeeded — the official client leaves the
    /// record alone otherwise, which is what makes a partial transfer resumable.
    pub async fn download_complete(&self, task_id: &str) -> Result<()> {
        self.call(
            "POST",
            "/api/v1/download/complete",
            json!({ "taskId": task_id }),
        )
        .await?;
        Ok(())
    }

    /// Receive one transfer end to end into `save_dir`.
    ///
    /// Files land under their in-transfer relative path, written to a
    /// `.download` temporary first and renamed on success, with a ` (n)` suffix
    /// if the name is taken. A failed file leaves the record on the server.
    pub async fn receive_task<F>(&self, task_id: &str, save_dir: &str, on_event: &F) -> Result<usize>
    where
        F: Fn(CloudEvent),
    {
        let files = self.download_init(task_id).await?;
        if files.is_empty() {
            // Nothing left to fetch, but the relay still lists it: close it out.
            self.download_complete(task_id).await?;
            return Ok(0);
        }

        on_event(CloudEvent::Started {
            task_id: task_id.to_string(),
            files: files.iter().map(|f| f.file_name.clone()).collect(),
        });

        let mut saved: Vec<String> = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let dir = if file.file_path.is_empty() {
                save_dir.to_string()
            } else {
                format!("{save_dir}/{}", file.file_path.trim_matches('/'))
            };

            if file.is_folder {
                tokio::fs::create_dir_all(&dir)
                    .await
                    .with_context(|| format!("create {dir}"))?;
                continue;
            }

            let temp = format!("{dir}/{}{index}.download", file.file_name);
            let bytes = match self.download_file(task_id, file, Path::new(&temp)).await {
                Ok(n) => n,
                Err(e) => {
                    let error = format!("{e:#}");
                    on_event(CloudEvent::Failed {
                        task_id: task_id.to_string(),
                        error: error.clone(),
                    });
                    bail!(error);
                }
            };

            let final_path = crate::share::dedup_path(&dir, &file.file_name);
            tokio::fs::rename(&temp, &final_path)
                .await
                .with_context(|| format!("rename {temp} -> {final_path}"))?;

            let name = Path::new(&final_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| file.file_name.clone());
            on_event(CloudEvent::FileDone {
                task_id: task_id.to_string(),
                name: name.clone(),
                bytes,
            });
            saved.push(name);
        }

        self.download_complete(task_id).await?;
        on_event(CloudEvent::Done {
            task_id: task_id.to_string(),
            files: saved.clone(),
            dir: save_dir.to_string(),
        });
        Ok(saved.len())
    }

    /// Receive everything currently waiting. Returns how many transfers were
    /// completed; a transfer that fails is reported through `on_event` and does
    /// not stop the others.
    pub async fn receive_pending<F>(&self, save_dir: &str, on_event: &F) -> Result<usize>
    where
        F: Fn(CloudEvent),
    {
        let records = self.pending_records().await?;
        let mut done = 0;
        for rec in records {
            match self.receive_task(&rec.task_id, save_dir, on_event).await {
                Ok(_) => done += 1,
                Err(e) => {
                    tracing::warn!(task_id = %rec.task_id, error = %format!("{e:#}"),
                                   "云传输: task failed, leaving it on the server");
                }
            }
        }
        Ok(done)
    }
}

/// `Base64(HMAC-SHA256(key = the timestamp as a decimal string, msg = the
/// client's fixed name))`. No shared secret is involved: the official client
/// carries an appKey/secret pair but never uses it for this.
fn request_sign(timestamp_millis: i64) -> String {
    let mac = pcsuite_crypto::hmac_sha256(
        timestamp_millis.to_string().as_bytes(),
        CONNECT_NAME.as_bytes(),
    );
    STANDARD.encode(mac)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Turn a business code into something a user can act on. Codes are the
/// client's own table; anything else is passed through with the server's text.
fn describe_code(code: i64, msg: &str) -> String {
    let known = match code {
        1001 => "the account's daily upload quota is used up",
        1002 => "the account token is not valid — sign in again",
        1003 => "the server rejected this client's app key",
        1004 => "signature check failed",
        1015 => "the account token has expired — sign in again",
        _ => "",
    };
    match (known.is_empty(), msg.is_empty()) {
        (false, _) => format!("{known} (code {code})"),
        (true, false) => format!("{msg} (code {code})"),
        (true, true) => format!("server returned code {code}"),
    }
}

fn parse_record(v: &Value) -> TransferRecord {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|t| t.parse().ok())))
            .unwrap_or(0)
    };
    TransferRecord {
        task_id: s("taskId"),
        total_count: n("totalCount").max(0) as u32,
        total_size: n("totalSize").max(0) as u64,
        create_time_millis: n("createTimeMillis"),
        download_status: n("downloadStatus"),
        sender: v
            .get("sendDeviceInfo")
            .and_then(|d| d.get("deviceName"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        first_file_name: v
            .get("firstFileInfo")
            .and_then(|d| d.get("fileName"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        server_expired: v.get("isExpired").and_then(Value::as_bool),
    }
}

fn parse_file(v: &Value) -> CloudFile {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    CloudFile {
        file_id: s("fileId"),
        file_name: s("fileName"),
        file_size: v
            .get("fileSize")
            .and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|t| t.parse().ok())))
            .unwrap_or(0),
        file_path: s("filePath"),
        is_folder: v.get("isFolder").and_then(Value::as_bool).unwrap_or(false),
        has_download: v.get("hasDownload").and_then(Value::as_bool).unwrap_or(false),
        send_source: v.get("sendSource").cloned().unwrap_or(json!(2)),
        send_source_entry: v.get("sendSourceEntry").cloned().unwrap_or(Value::Null),
    }
}

/// OS version for the `systemVersion` header. Best effort: the server logs it
/// but has never been observed to reject a value.
#[cfg(target_os = "macos")]
fn host_system_version() -> String {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0.0".into())
}

#[cfg(not(target_os = "macos"))]
fn host_system_version() -> String {
    "0.0".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_table_maps_the_documented_cases() {
        assert_eq!(bridge_host("hk"), "asia-cross-file-bridge.vivoglobal.com");
        assert_eq!(bridge_host("SG"), "asia-cross-file-bridge.vivoglobal.com");
        assert_eq!(bridge_host("in"), "in-cross-file-bridge.vivoglobal.com");
        assert_eq!(bridge_host("de"), "eu-cross-file-bridge.vivoglobal.com");
        // The UK shares the EU room but gets its own host.
        assert_eq!(bridge_host("uk"), "uk-cross-file-bridge.vivoglobal.com");
        assert_eq!(bridge_host("gb"), "uk-cross-file-bridge.vivoglobal.com");
        // The second EU room splits three ways before falling back to <cc>-.
        assert_eq!(bridge_host("za"), "zasc-cross-file-bridge.vivoglobal.com");
        assert_eq!(bridge_host("cl"), "sca-cross-file-bridge.vivoglobal.com");
        assert_eq!(bridge_host("br"), "br-cross-file-bridge.jovimobile.com");
        assert_eq!(bridge_host("au"), "au-cross-file-bridge.vivoglobal.com");
        // Domestic build, and the sg fallback for anything unrecognised.
        assert_eq!(bridge_host("cn"), DOMESTIC_BRIDGE_HOST);
        assert_eq!(bridge_host(""), DOMESTIC_BRIDGE_HOST);
        assert_eq!(bridge_host("zz"), "asia-cross-file-bridge.vivoglobal.com");
    }

    #[test]
    fn signature_is_hmac_over_the_client_name_keyed_by_the_timestamp() {
        // Frozen vector: locks the key/message order, which is the easy thing to
        // get backwards and the hard thing to debug against a live server.
        assert_eq!(request_sign(1_700_000_000_000), {
            let mac = pcsuite_crypto::hmac_sha256(
                b"1700000000000",
                b"com.vivo.pcsuite.connect",
            );
            STANDARD.encode(mac)
        });
        // Cross-checked against the runtime the official client actually uses:
        //   node -e 'const {createHmac}=require("crypto");
        //            const h=createHmac("sha256","1700000000000");
        //            h.update("com.vivo.pcsuite.connect");
        //            console.log(h.digest("base64"))'
        assert_eq!(
            request_sign(1_700_000_000_000),
            "MGvVaeilP33jcS7pGDsPzxivt8P/gboBKoksK1THAlc="
        );
    }

    #[test]
    fn expiry_falls_back_to_the_upload_timestamp() {
        let fresh = TransferRecord {
            create_time_millis: now_millis() - 3600 * 1000,
            ..Default::default()
        };
        assert!(!fresh.is_expired());

        let old = TransferRecord {
            create_time_millis: now_millis() - (EXPIRY_HOURS + 1) * 3600 * 1000,
            ..Default::default()
        };
        assert!(old.is_expired());

        // The server's own verdict wins when it sends one.
        let contradicted = TransferRecord {
            create_time_millis: now_millis(),
            server_expired: Some(true),
            ..Default::default()
        };
        assert!(contradicted.is_expired());
    }

    #[test]
    fn record_and_file_parsing_tolerates_stringly_typed_numbers() {
        let rec = parse_record(&json!({
            "taskId": "t1",
            "totalCount": 2,
            "totalSize": "4096",
            "createTimeMillis": 1700000000000i64,
            "downloadStatus": 3,
            "sendDeviceInfo": {"deviceName": "iQOO 15"},
            "firstFileInfo": {"fileName": "a.jpg"},
            "isExpired": false
        }));
        assert_eq!(rec.task_id, "t1");
        assert_eq!(rec.total_size, 4096);
        assert_eq!(rec.sender, "iQOO 15");
        assert_eq!(rec.first_file_name, "a.jpg");
        assert_eq!(rec.server_expired, Some(false));

        let f = parse_file(&json!({
            "fileId": "f1", "fileName": "a.jpg", "fileSize": "10",
            "filePath": "", "isFolder": false, "hasDownload": false
        }));
        assert_eq!(f.file_size, 10);
        // Absent sendSource defaults to the client's own fallback of 2.
        assert_eq!(f.send_source, json!(2));
    }
}
