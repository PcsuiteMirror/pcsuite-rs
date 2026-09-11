//! ConnectFlow JSON frames (carried inside [`crate::payload1`] frames on 10191).
//!
//! Two frames are sent: a device-info exchange (`bytes:[22]`) and the connect
//! frame carrying `seed` + `sign` in a stringified `extra_info` object. The phone
//! replies with a `bytes:[code]` array — see [`ReplyCode`].

use serde::Serialize;
use serde_json::{json, Value};

/// The PC's identity, as the phone expects to see it on the wire. These are
/// config values (not secrets); `service_id` is a protocol-mandated constant.
#[derive(Clone, Debug)]
pub struct PcIdentity {
    /// Logged-in account openId the phone matches the sign against.
    pub open_id: String,
    /// PC MAC, used as `target_id` / SSDP deviceId (e.g. "001122334455").
    pub pc_mac: String,
    /// Display account string.
    pub account: String,
    /// Display device name.
    pub device_name: String,
    /// Service id constant (e.g. "com.vivo.pcsuite.SERVICE").
    pub service_id: String,
    /// Fixed JSON `id` field (e.g. 87654321).
    pub frame_id: i64,
}

/// Phone reply status carried in the `bytes[0]` field of a ConnectFlow reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyCode {
    /// `1` — success; the phone stored the token and will open 10380.
    Success,
    /// `2` — rejected.
    Reject,
    /// `22` — device-info request.
    DeviceInfoReq,
    /// `23` — device-info ack.
    DeviceInfoAck,
    /// `28` — sign present but openId mismatch (key mismatch).
    OpenIdMismatch,
    /// Any other code.
    Other(i64),
}

impl ReplyCode {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => ReplyCode::Success,
            2 => ReplyCode::Reject,
            22 => ReplyCode::DeviceInfoReq,
            23 => ReplyCode::DeviceInfoAck,
            28 => ReplyCode::OpenIdMismatch,
            other => ReplyCode::Other(other),
        }
    }
}

/// Build the device-info exchange frame (`bytes:[code]`, code is usually 22).
pub fn device_info_frame(id: &PcIdentity, code: i64) -> Value {
    json!({
        "channel": 0,
        "id": id.frame_id,
        "open_Id": id.open_id,
        "target_id": id.pc_mac,
        "account": id.account,
        "type": 1,
        "service_id": id.service_id,
        "bytes": [code],
        "deviceName": id.device_name,
    })
}

/// Build the connect frame carrying `seed_b` + `sign`.
///
/// `connect_type`: `2` for LAN (per-IP stored seed), `1` for the remote path
/// (`key = SHA256(seed_b)`, no pre-shared seed).
///
/// `auto_connect` sets `isAutoConnect`/`isAutoConnectNew` ("1" when true). The
/// discoverable-presence path (holding 10191 open so the phone lists this PC as
/// "可连") uses `true` + `pc_version = 680`, matching the official service captured
/// on Windows; the one-shot active-connect path keeps `false` + its own version.
pub fn connect_frame(
    id: &PcIdentity,
    seed_b: &str,
    sign: &str,
    connect_type: i64,
    auto_connect: bool,
    pc_version: i64,
) -> Value {
    let ac = if auto_connect { "1" } else { "0" };
    let extra = json!({
        "isAutoConnect": ac,
        "isAutoConnectNew": ac,
        "seed": seed_b,
        "sign": sign,
        "connectType": connect_type,
        "pcPcsuiteVersion": pc_version,
    });
    json!({
        "target_id": id.pc_mac,
        "service_id": id.service_id,
        "extra_info": serde_json::to_string(&extra).expect("extra_info serialize"),
        "type": 1,
        "id": id.frame_id,
        "open_Id": id.open_id,
        "deviceName": id.device_name,
        "bytes": [0],
        "channel": 0,
        "account": id.account,
    })
}

/// Extract the `bytes[0]` status code from a phone reply JSON.
pub fn reply_code(v: &Value) -> Option<ReplyCode> {
    let code = v.get("bytes")?.as_array()?.first()?.as_i64()?;
    Some(ReplyCode::from_i64(code))
}

/// Whether a phone push is a "connect requested" event — the user tapped 「连接」
/// in the connection center. On the wire it is `bytes:[24]` with an `extra_info`
/// carrying `connectType:"Connect"`. The official desktop reacts by opening the
/// 10380 control session (it does NOT start mirroring — that is a later step).
pub fn is_connect_request(v: &Value) -> bool {
    let code = v
        .get("bytes")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_i64);
    if code != Some(24) {
        return false;
    }
    // Confirm connectType is "Connect" (vs "PreConnect", the remote-wake variant)
    // when the field is present; tolerate its absence.
    match v.get("extra_info").and_then(Value::as_str) {
        Some(ei) => serde_json::from_str::<Value>(ei)
            .ok()
            .and_then(|e| {
                e.get("ext")
                    .and_then(|x| x.get("connectType"))
                    .and_then(Value::as_str)
                    .map(|ct| ct.eq_ignore_ascii_case("Connect"))
            })
            .unwrap_or(true),
        None => true,
    }
}

/// Extract `auth_status` from a phone reply. The status lives in a doubly-encoded
/// JSON string: `extra_info` (a JSON string) → `forwardData` (a JSON string) →
/// `{ "auth_status": bool, "connect_status": bool }`. `true` means the phone
/// accepted our sign; `connect_status` stays `false` for mere presence.
pub fn auth_status(v: &Value) -> Option<bool> {
    let ei = v.get("extra_info")?.as_str()?;
    let ei: Value = serde_json::from_str(ei).ok()?;
    let fd = ei.get("forwardData")?.as_str()?;
    let fd: Value = serde_json::from_str(fd).ok()?;
    fd.get("auth_status")?.as_bool()
}

/// SSDP `compatGsonStr` presence payload describing this PC (base64-encoded by
/// the caller). `serviceRecord` is a protocol-mandated constant string.
#[derive(Serialize)]
pub struct Presence<'a> {
    #[serde(rename = "deviceId")]
    pub device_id: &'a str,
    #[serde(rename = "openId")]
    pub open_id: &'a str,
    pub account: &'a str,
    #[serde(rename = "deviceName")]
    pub device_name: &'a str,
    #[serde(rename = "serviceRecord")]
    pub service_record: &'a str,
    pub port: u16,
    #[serde(rename = "deviceType")]
    pub device_type: &'a str,
    pub extra: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> PcIdentity {
        PcIdentity {
            open_id: "0123456789abcdef".into(),
            pc_mac: "001122334455".into(),
            account: "138****000".into(),
            device_name: "test MacBook".into(),
            service_id: "com.vivo.pcsuite.SERVICE".into(),
            frame_id: 87654321,
        }
    }

    #[test]
    fn device_info_shape() {
        let v = device_info_frame(&id(), 22);
        assert_eq!(v["open_Id"], "0123456789abcdef");
        assert_eq!(v["target_id"], "001122334455");
        assert_eq!(v["type"], 1);
        assert_eq!(v["bytes"][0], 22);
        assert_eq!(v["deviceName"], "test MacBook");
    }

    #[test]
    fn connect_extra_info_is_stringified_json() {
        let v = connect_frame(&id(), "SEED-B-UUID", "deadbeef", 1, false, 620);
        // extra_info must be a JSON *string*, not an object.
        let s = v["extra_info"].as_str().expect("extra_info is a string");
        let extra: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(extra["seed"], "SEED-B-UUID");
        assert_eq!(extra["sign"], "deadbeef");
        assert_eq!(extra["connectType"], 1);
        assert_eq!(extra["pcPcsuiteVersion"], 620);
        assert_eq!(extra["isAutoConnect"], "0");
        assert_eq!(v["bytes"][0], 0);
    }

    #[test]
    fn presence_connect_frame_uses_auto_connect() {
        let v = connect_frame(&id(), "SEED-B-UUID", "deadbeef", 2, true, 680);
        let s = v["extra_info"].as_str().unwrap();
        let extra: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(extra["isAutoConnect"], "1");
        assert_eq!(extra["isAutoConnectNew"], "1");
        assert_eq!(extra["pcPcsuiteVersion"], 680);
    }

    #[test]
    fn auth_status_parses_doubly_encoded_forward_data() {
        let reply = json!({
            "bytes": [1],
            "extra_info": serde_json::to_string(&json!({
                "forwardData": serde_json::to_string(&json!({
                    "auth_status": true, "connect_status": false
                })).unwrap()
            })).unwrap(),
        });
        assert_eq!(auth_status(&reply), Some(true));
        assert_eq!(auth_status(&json!({"bytes":[1]})), None);
    }

    #[test]
    fn reply_codes() {
        assert_eq!(reply_code(&json!({"bytes":[1]})), Some(ReplyCode::Success));
        assert_eq!(reply_code(&json!({"bytes":[28]})), Some(ReplyCode::OpenIdMismatch));
        assert_eq!(reply_code(&json!({"bytes":[99]})), Some(ReplyCode::Other(99)));
        assert_eq!(reply_code(&json!({"nope":true})), None);
    }
}
