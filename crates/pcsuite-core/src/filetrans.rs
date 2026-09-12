//! Phone→PC「快传 / 发送到电脑」receiver as a feature on the shared control WS.
//!
//! The phone announces a batch as `FILE_TRANS_TAG:[{path,fileSize,fileName}]`
//! (see `pcsuite_proto::filetrans`) on the same control WS everything else
//! multiplexes over — no extra registration. This loop subscribes to that
//! broadcast, pulls each announced batch over the mdfs HTTP plane
//! (`download_info` + `download` tar stream, see [`crate::mdfs`]), writes the
//! files into a local directory, and acks on the control WS
//! (`TRANS_FILE_SUCCESS:` / `TRANS_FILE_FAIL:`). Batches are handled one at a
//! time (inline), so a transfer in progress naturally queues later ones.

use anyhow::{Context, Result};
use tokio::sync::broadcast::error::RecvError;

use crate::mdfs;
use crate::session::ControlHandle;

// Re-export the proto message helpers through this module so frontends keep a
// single `pcsuite_core::filetrans` path (the CLI already uses it).
pub use pcsuite_proto::filetrans::{
    fail_receipt, is_cancel, parse_file_trans_tag, success_receipt, FileTransItem,
};

/// Where to pull from and where to save.
pub struct FileTransConfig {
    /// Control host (`127.0.0.1` for USB; the phone IP for LAN).
    pub data_ip: String,
    /// Session connect token.
    pub token: String,
    /// The phone's `mobileDeviceId` (mdfs routing header).
    pub device_id: String,
    /// Local directory received files are written into (created if missing).
    pub save_dir: String,
}

/// One observable step of the receiver, pushed to the frontend.
#[derive(Debug, Clone, PartialEq)]
pub enum FileTransEvent {
    /// A batch was announced; a pull is starting. `files` are the phone-declared names.
    Started { files: Vec<String> },
    /// The batch was pulled and written. `files` are the basenames actually saved.
    Done { files: Vec<String>, dir: String },
    /// The batch could not be pulled/written (a fail receipt was sent).
    Failed { files: Vec<String>, error: String },
    /// The phone cancelled the in-flight transfer.
    Cancelled,
}

impl FileTransEvent {
    /// Serialize for the FFI boundary: `{"type":…, "files":[…], "dir":…, "error":…}`.
    pub fn to_json(&self) -> String {
        use serde_json::json;
        match self {
            FileTransEvent::Started { files } => json!({"type": "started", "files": files}),
            FileTransEvent::Done { files, dir } => {
                json!({"type": "done", "files": files, "dir": dir})
            }
            FileTransEvent::Failed { files, error } => {
                json!({"type": "failed", "files": files, "error": error})
            }
            FileTransEvent::Cancelled => json!({"type": "cancelled"}),
        }
        .to_string()
    }
}

/// Run the receiver until the control channel closes, reporting each step via
/// `on_event`. Shares the session's one control WS with clipboard/notify/etc.
pub async fn filetrans_feature<F>(control: ControlHandle, cfg: FileTransConfig, on_event: F)
where
    F: Fn(FileTransEvent) + Send + 'static,
{
    let mut rx = control.subscribe();
    tracing::info!(dir = %cfg.save_dir, "file-transfer receiver armed");
    loop {
        let text = match rx.recv().await {
            Ok(t) => t,
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "file-transfer: control messages lagged");
                continue;
            }
            Err(RecvError::Closed) => break,
        };
        if is_cancel(&text) {
            tracing::info!("file-transfer: phone cancelled the transfer");
            on_event(FileTransEvent::Cancelled);
            continue;
        }
        let Some(batch) = parse_file_trans_tag(&text) else {
            continue;
        };
        if batch.is_empty() {
            continue;
        }
        let names: Vec<String> = batch.iter().map(|it| it.file_name.clone()).collect();
        // One transfer id per batch: used on both mdfs requests and echoed in the
        // receipt, exactly like the official client.
        let task_id = mdfs::new_transfer_id();
        tracing::info!(files = batch.len(), task = %task_id, "★ file-transfer batch announced");
        on_event(FileTransEvent::Started {
            files: names.clone(),
        });
        match recv_batch(&cfg, &task_id, &batch).await {
            Ok(saved) => {
                let receipt = success_receipt(&task_id, saved.len() as u32);
                if let Err(e) = control.send(receipt).await {
                    tracing::warn!(err = %e, "file-transfer: receipt send failed");
                }
                on_event(FileTransEvent::Done {
                    files: saved,
                    dir: cfg.save_dir.clone(),
                });
            }
            Err(e) => {
                let error = format!("{e:#}");
                tracing::warn!(err = %error, "file-transfer: batch failed");
                let receipt = fail_receipt(&task_id, batch.len() as u32, 0);
                if let Err(e2) = control.send(receipt).await {
                    tracing::warn!(err = %e2, "file-transfer: fail receipt send failed");
                }
                on_event(FileTransEvent::Failed {
                    files: names,
                    error,
                });
            }
        }
    }
}

/// Pull one announced batch over mdfs and write every tar entry into
/// `cfg.save_dir`. Returns the basenames actually written.
async fn recv_batch(
    cfg: &FileTransConfig,
    task_id: &str,
    batch: &[FileTransItem],
) -> Result<Vec<String>> {
    std::fs::create_dir_all(&cfg.save_dir).with_context(|| format!("mkdir {}", cfg.save_dir))?;
    let paths: Vec<String> = batch.iter().map(|it| it.path.clone()).collect();
    let total: u64 = batch.iter().map(|it| it.size).sum();
    let files =
        mdfs::download_batch(&cfg.data_ip, &cfg.token, &cfg.device_id, task_id, &paths, total)
            .await?;
    let mut saved = Vec::with_capacity(files.len());
    for (name, bytes) in &files {
        // Tar entry names come from the phone — keep only the basename.
        let safe = std::path::Path::new(name)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("recv-{}.bin", saved.len()));
        let dest = format!("{}/{safe}", cfg.save_dir);
        std::fs::write(&dest, bytes).with_context(|| format!("write {dest}"))?;
        tracing::info!(name = %safe, bytes = bytes.len(), "file-transfer: saved");
        saved.push(safe);
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_json_shapes() {
        let started = FileTransEvent::Started {
            files: vec!["a.pdf".into()],
        };
        assert_eq!(started.to_json(), r#"{"files":["a.pdf"],"type":"started"}"#);

        let done = FileTransEvent::Done {
            files: vec!["a.pdf".into(), "b.jpg".into()],
            dir: "/tmp/x".into(),
        };
        assert_eq!(
            done.to_json(),
            r#"{"dir":"/tmp/x","files":["a.pdf","b.jpg"],"type":"done"}"#
        );

        let failed = FileTransEvent::Failed {
            files: vec![],
            error: "boom".into(),
        };
        assert_eq!(failed.to_json(), r#"{"error":"boom","files":[],"type":"failed"}"#);

        assert_eq!(FileTransEvent::Cancelled.to_json(), r#"{"type":"cancelled"}"#);
    }
}
