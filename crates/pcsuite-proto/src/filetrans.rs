//! Phone→PC "快传 / 发送到电脑" file-transfer messages (text frames on the 10380
//! control WS, alongside the clipboard/notify traffic — no extra registration).
//!
//! The phone announces a batch as `FILE_TRANS_TAG:[{...}]` (the phone-side
//! "原子岛 / 发送到电脑" entry; the system share panel's「办公套件」entry uses the
//! `SHARE_TRANS_FILE:` prefix instead); each item carries the phone-side absolute
//! `path` + `fileSize` + `fileName`, which the PC then pulls over the mdfs HTTP
//! plane (`download_info` + `download` tar stream, see `pcsuite_core::mdfs`).
//!
//! Wire shapes below are taken verbatim from the official Windows client's logs
//! (2026-09-12, `artifacts/captures/win-official-file-share-20260912/`):
//!
//! ```text
//! phone→PC  FILE_TRANS_TAG:[{"downloadCount":0,"fileId":"<uuid>","fileName":"a.jpg",
//!             "filePath":"/storage/emulated/0/…/a.jpg","fileSize":1266678,"fileType":"jpg",
//!             "hasDownload":false,"isDirectory":false,"isFolder":false,"isLivePhoto":false,
//!             "modifiedTime":1789176282000,"notificationID":0,"path":"/storage/emulated/0/…/a.jpg",
//!             "successTime":0}]
//! PC        POST /pc_file_manager/download_info?id=<taskId>  → GET /pc_file_manager/download?id=<taskId>
//! PC→phone  TRANS_FILE_SUCCESS:{"successCount":1,"id":"<taskId>"}
//! PC→phone  TRANS_FILE_CANCEL:{"failCount":1,"successCount":0,"type":"send","id":"<taskId>"}   (PC gave up)
//! phone→PC  TRANS_FILE_CANCEL:{"failCount":0,"successCount":0,"type":"send"}                  (phone cancelled)
//! phone→PC  TRANS_FILE_FAIL:{...}                                                              (phone-side failure)
//! ```
//!
//! `taskId` is minted by the PC per batch (the official client uses a cuid; any
//! unique string is accepted) and must be the same value on both mdfs requests
//! and in the receipt. Note the PC never sends `TRANS_FILE_FAIL:` — failure from
//! the PC side is spelled `TRANS_FILE_CANCEL:` with a non-zero `failCount`.

use serde_json::Value;

/// Batch announcement prefix (原子岛 / 发送到电脑).
pub const FILE_TRANS_PREFIX: &str = "FILE_TRANS_TAG:";
/// Alternate batch announcement prefix (system share panel →「办公套件」).
pub const SHARE_TRANS_PREFIX: &str = "SHARE_TRANS_FILE:";
/// Both directions: phone cancelled the in-flight transfer / PC reports failure.
pub const CANCEL_PREFIX: &str = "TRANS_FILE_CANCEL:";
/// PC → phone receipt: batch pulled OK.
pub const SUCCESS_PREFIX: &str = "TRANS_FILE_SUCCESS:";
/// Phone → PC: the phone side failed (official client only ever *receives* this).
pub const FAIL_PREFIX: &str = "TRANS_FILE_FAIL:";

/// One file the phone wants to send to the PC.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileTransItem {
    pub file_name: String,
    /// Phone-side absolute path (feed to mdfs `download_info`'s `downloadList`).
    pub path: String,
    /// Declared size in bytes (0 if absent).
    pub size: u64,
}

/// Parse a `FILE_TRANS_TAG:` / `SHARE_TRANS_FILE:` announcement into the file
/// batch, or `None` if `text` is some other control message (keepalive, shadow,
/// notify, …) or the payload is not a JSON array.
pub fn parse_file_trans_tag(text: &str) -> Option<Vec<FileTransItem>> {
    let rest = text
        .strip_prefix(FILE_TRANS_PREFIX)
        .or_else(|| text.strip_prefix(SHARE_TRANS_PREFIX))?;
    let start = rest.find('[')?;
    let arr: Value = serde_json::from_str(&rest[start..]).ok()?;
    let arr = arr.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for it in arr {
        let path = it.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            continue; // not a pullable item
        }
        let file_name = it
            .get("fileName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.rsplit('/').next().unwrap_or(path).to_string()
            });
        let size = it
            .get("fileSize")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0);
        out.push(FileTransItem {
            file_name,
            path: path.to_string(),
            size,
        });
    }
    Some(out)
}

/// `true` when the phone aborted the in-flight batch: either an explicit
/// `TRANS_FILE_CANCEL:` (user cancelled) or `TRANS_FILE_FAIL:` (phone-side
/// failure). Both mean "stop pulling, nothing to ack".
pub fn is_cancel(text: &str) -> bool {
    text.starts_with(CANCEL_PREFIX) || text.starts_with(FAIL_PREFIX)
}

/// Build the PC→phone success receipt (official shape:
/// `TRANS_FILE_SUCCESS:{"successCount":N,"id":"<taskId>"}`).
pub fn success_receipt(task_id: &str, success_count: u32) -> String {
    format!(
        "{SUCCESS_PREFIX}{{\"successCount\":{success_count},\"id\":{}}}",
        Value::String(task_id.to_string())
    )
}

/// Build the PC→phone failure receipt. The official client spells this with the
/// *cancel* prefix: `TRANS_FILE_CANCEL:{"failCount":N,"successCount":M,"type":"send","id":"<taskId>"}`.
pub fn fail_receipt(task_id: &str, fail_count: u32, success_count: u32) -> String {
    format!(
        "{CANCEL_PREFIX}{{\"failCount\":{fail_count},\"successCount\":{success_count},\"type\":\"send\",\"id\":{}}}",
        Value::String(task_id.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_trans_tag_batch() {
        let text = r#"FILE_TRANS_TAG:[{"fileName":"a.pdf","path":"/storage/emulated/0/Download/a.pdf","fileSize":123,"extra":1},{"path":"/storage/emulated/0/DCIM/b.jpg","fileSize":"456"}]"#;
        let items = parse_file_trans_tag(text).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].file_name, "a.pdf");
        assert_eq!(items[0].path, "/storage/emulated/0/Download/a.pdf");
        assert_eq!(items[0].size, 123);
        // fileName absent → basename of path; string size tolerated.
        assert_eq!(items[1].file_name, "b.jpg");
        assert_eq!(items[1].size, 456);
    }

    #[test]
    fn parses_share_trans_prefix() {
        let text = r#"SHARE_TRANS_FILE:[{"fileName":"c.png","path":"/sdcard/c.png","fileSize":1}]"#;
        let items = parse_file_trans_tag(text).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/sdcard/c.png");
    }

    #[test]
    fn rejects_other_messages_and_bad_json() {
        assert!(parse_file_trans_tag("SHADOW_LIKE:{}").is_none());
        assert!(parse_file_trans_tag("KEEPALIVE").is_none());
        assert!(parse_file_trans_tag("FILE_TRANS_TAG:not-json").is_none());
        assert!(parse_file_trans_tag("FILE_TRANS_TAG:{\"a\":1}").is_none()); // not an array
    }

    #[test]
    fn items_without_path_are_skipped() {
        let text = r#"FILE_TRANS_TAG:[{"fileName":"x"},{"fileName":"y","path":"/sdcard/y","fileSize":2}]"#;
        let items = parse_file_trans_tag(text).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].file_name, "y");
    }

    #[test]
    fn parses_official_windows_announcement() {
        // Verbatim from the official Windows client log (2026-09-12).
        let text = r#"FILE_TRANS_TAG:[{"downloadCount":0,"fileId":"59ca04eb-5718-4b10-b12e-adae3b8c0fba","fileName":"Screenshot_20260912_092439.jpg","filePath":"/storage/emulated/0/Pictures/Screenshots/Screenshot_20260912_092439.jpg","fileSize":1266678,"fileType":"jpg","hasDownload":false,"isDirectory":false,"isFolder":false,"isLivePhoto":false,"modifiedTime":1789176282000,"notificationID":0,"path":"/storage/emulated/0/Pictures/Screenshots/Screenshot_20260912_092439.jpg","successTime":0}] "#;
        let items = parse_file_trans_tag(text).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].file_name, "Screenshot_20260912_092439.jpg");
        assert_eq!(items[0].size, 1_266_678);
    }

    #[test]
    fn cancel_and_receipts() {
        // Phone-side cancel / failure, as the official client receives them.
        assert!(is_cancel(
            r#"TRANS_FILE_CANCEL:{"failCount":0,"successCount":0,"type":"send"}"#
        ));
        assert!(is_cancel("TRANS_FILE_FAIL:{}"));
        assert!(!is_cancel("FILE_TRANS_TAG:[]"));
        // PC-side receipts, byte-for-byte the official spellings.
        assert_eq!(
            success_receipt("cmtxrqnla00013y6pmk9f80aq", 1),
            r#"TRANS_FILE_SUCCESS:{"successCount":1,"id":"cmtxrqnla00013y6pmk9f80aq"}"#
        );
        assert_eq!(
            fail_receipt("cmtxroyjt00093y6plyxecs34", 1, 0),
            r#"TRANS_FILE_CANCEL:{"failCount":1,"successCount":0,"type":"send","id":"cmtxroyjt00093y6plyxecs34"}"#
        );
    }
}
