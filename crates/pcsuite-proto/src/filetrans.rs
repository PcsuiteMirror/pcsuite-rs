//! Phone→PC "快传 / 发送到电脑" file-transfer messages (text frames on the 10380
//! control WS, alongside the clipboard/notify traffic — no extra registration).
//!
//! The phone announces a batch as `FILE_TRANS_TAG:[{...}]` (some builds use the
//! `SHARE_TRANS_FILE:` prefix instead); each item carries the phone-side absolute
//! `path` + `fileSize` + `fileName`, which the PC then pulls over the mdfs HTTP
//! plane (`download_info` + `download` tar stream, see `pcsuite_core::mdfs`).
//! `TRANS_FILE_CANCEL:` aborts the in-flight batch.
//!
//! Receipt direction (PC→phone): `TRANS_FILE_SUCCESS:{...}` /
//! `TRANS_FILE_FAIL:{...}`. NOTE: the exact receipt field names were not fully
//! reversed from the official app; `totalCount`/`successCount` below are the
//! agreed best guess and may need on-device confirmation.

use serde_json::Value;

/// Batch announcement prefix (primary spelling).
pub const FILE_TRANS_PREFIX: &str = "FILE_TRANS_TAG:";
/// Alternate batch announcement prefix (share flow).
pub const SHARE_TRANS_PREFIX: &str = "SHARE_TRANS_FILE:";
/// Phone cancelled the in-flight transfer.
pub const CANCEL_PREFIX: &str = "TRANS_FILE_CANCEL:";
/// PC → phone receipt: batch pulled OK.
pub const SUCCESS_PREFIX: &str = "TRANS_FILE_SUCCESS:";
/// PC → phone receipt: batch failed.
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

/// `true` for a `TRANS_FILE_CANCEL:` control message.
pub fn is_cancel(text: &str) -> bool {
    text.starts_with(CANCEL_PREFIX)
}

/// Build the PC→phone success receipt. Field names are best-guess (see module
/// docs — not fully reversed).
pub fn success_receipt(total_count: u32, success_count: u32) -> String {
    format!("{SUCCESS_PREFIX}{{\"totalCount\":{total_count},\"successCount\":{success_count}}}")
}

/// Build the PC→phone failure receipt (official spelling sends `successCount:0`).
pub fn fail_receipt(total_count: u32) -> String {
    format!("{FAIL_PREFIX}{{\"totalCount\":{total_count},\"successCount\":0}}")
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
    fn cancel_and_receipts() {
        assert!(is_cancel("TRANS_FILE_CANCEL:{}"));
        assert!(!is_cancel("FILE_TRANS_TAG:[]"));
        assert_eq!(
            success_receipt(3, 3),
            r#"TRANS_FILE_SUCCESS:{"totalCount":3,"successCount":3}"#
        );
        assert_eq!(fail_receipt(2), r#"TRANS_FILE_FAIL:{"totalCount":2,"successCount":0}"#);
    }
}
