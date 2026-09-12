//! Control-WS text messages (sent over the 10380 control WS / 10381 mirror WS).
//!
//! These are line-style messages: a literal keyword optionally followed by a
//! compact JSON object (sometimes with a `:` separator, sometimes glued on).

use serde::Serialize;

/// `SCREEN_START:` parameters (sent on the mirror WS to start the HEVC stream).
///
/// Field names match the wire exactly (note the lone camelCase `screenPrivacy`).
/// Defaults mirror the known-good values used by the working client.
#[derive(Serialize, Clone, Debug)]
pub struct ScreenParams {
    pub device_type: i64,
    pub first_file_open: bool,
    /// Ask the phone to prefix every media packet with a send timestamp.
    ///
    /// ⚠️ The phone switches on the **presence of the key**, not its value
    /// (`ScreenController`: `if (json.has("frame_with_time")) setFrameWithTime(true)`),
    /// so serializing `false` turns it **on**. Omitted when false for exactly that
    /// reason — sending it cost us the audio: with timestamps on, audio packets also
    /// carry the `FRAME:` tag and were demuxed as video.
    #[serde(skip_serializing_if = "is_false")]
    pub frame_with_time: bool,
    pub image_quality: i64,
    pub max_size: i64,
    pub mime_type: String,
    pub msg_send_key_mode: String,
    pub need_open_file: bool,
    pub no_audio: bool,
    pub pc_version: String,
    #[serde(rename = "screenPrivacy")]
    pub screen_privacy: bool,
    pub show_touch_spot: bool,
    pub split_frame: bool,
    pub support_drag: bool,
    /// Encoder bitrate override (bps). The phone falls back to its own default
    /// (~4 Mbps) when this field is absent — so we omit it on the wire when 0,
    /// keeping the default `SCREEN_START` byte-identical to the official client.
    #[serde(skip_serializing_if = "is_zero")]
    pub bit_rate: i64,
    /// Encoder frame-rate cap (fps). Omitted when 0 (phone picks its default).
    #[serde(skip_serializing_if = "is_zero")]
    pub frame_rate: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for ScreenParams {
    fn default() -> Self {
        Self {
            device_type: 1,
            first_file_open: true,
            frame_with_time: false,
            image_quality: 3,
            max_size: 2336,
            mime_type: "video/hevc".into(),
            msg_send_key_mode: "0".into(),
            need_open_file: true,
            no_audio: true,
            pc_version: "6.0.1".into(),
            screen_privacy: false,
            show_touch_spot: false,
            split_frame: false,
            support_drag: true,
            bit_rate: 0,
            frame_rate: 0,
        }
    }
}

/// Build the `SCREEN_START:{...}` text message.
pub fn screen_start(p: &ScreenParams) -> String {
    format!(
        "SCREEN_START:{}",
        serde_json::to_string(p).expect("ScreenParams serialize")
    )
}

/// Build the `req_authrity{"source":N}` text message (note: no `:` separator).
pub fn req_authrity(source: i64) -> String {
    format!("req_authrity{{\"source\":{source}}}")
}

/// A request the phone's connection center sends over the control WS when the user
/// taps one of the PC's function buttons on the phone:
/// `connectCenterMsg:{"data":{"deviceId":"<this PC>"},"msgId":"…","name":"openVivoScreen"}`.
///
/// The PC answers on the same WS with `connectCenterResult:` carrying the *same*
/// `msgId` and a `{code, reason}` result (`code` 0 = done) — `MobileDevice.
/// replyMsgToConnectCenter` in the official bundle. `openVivoScreen` is the phone
/// asking this PC to start mirroring (the official desktop answers it by sending
/// `req_authrity{"source":2}` and opening the mirror once the phone authorizes);
/// `closeVivoScreen` stops it. `openExtScreen`/`closeExtScreen` are the
/// extended-desktop equivalents, which we do not implement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectCenterMsg {
    pub name: String,
    pub msg_id: String,
}

/// Parse a `connectCenterMsg:` control-WS text, or `None` for anything else.
pub fn parse_connect_center(text: &str) -> Option<ConnectCenterMsg> {
    let body = text.strip_prefix("connectCenterMsg:")?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    // msgId is a string in practice; tolerate a number so a reply still correlates.
    let msg_id = match v.get("msgId") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    Some(ConnectCenterMsg { name, msg_id })
}

/// Build the `connectCenterResult:` answer to a [`ConnectCenterMsg`].
pub fn connect_center_result(name: &str, msg_id: &str, code: i64, reason: &str) -> String {
    let v = serde_json::json!({
        "name": name,
        "msgId": msg_id,
        "data": { "code": code, "reason": reason },
    });
    format!("connectCenterResult:{v}")
}

/// Periodic control-WS keepalive text.
pub const KEEPALIVE: &str = "normal";

/// Mirror-WS binary frames are sometimes prefixed with this ASCII tag before the
/// raw HEVC payload; strip it if present.
pub const FRAME_PREFIX: &[u8; 6] = b"FRAME:";

/// With `frame_with_time`, the `FRAME:` tag is followed by an 8-byte
/// `System.currentTimeMillis()` and a 2-byte encode-duration short before the
/// payload (`PcTransportManager.sendVideo/sendAudio`) — on **audio packets too**.
pub const FRAME_TIME_HEADER_LEN: usize = 10;

/// Strip a leading `FRAME:` tag from a binary mirror frame, if present.
pub fn strip_frame_prefix(payload: &[u8]) -> &[u8] {
    strip_frame_header(payload, false)
}

/// Strip the phone's framing to get at the media payload.
///
/// `with_time` must match the `frame_with_time` we asked for in `SCREEN_START`:
/// the timestamp header can't be sniffed, because a millisecond timestamp begins
/// `00 00 01 …` and is indistinguishable from a 3-byte Annex-B start code.
pub fn strip_frame_header(payload: &[u8], with_time: bool) -> &[u8] {
    if payload.len() >= FRAME_PREFIX.len() && &payload[..FRAME_PREFIX.len()] == FRAME_PREFIX {
        let mut off = FRAME_PREFIX.len();
        if with_time && payload.len() >= off + FRAME_TIME_HEADER_LEN {
            off += FRAME_TIME_HEADER_LEN;
        }
        &payload[off..]
    } else {
        payload
    }
}

/// Does this binary mirror message carry a video frame? When `no_audio:false` the
/// phone interleaves non-video (audio) packets on the same WS; feeding those to the
/// HEVC decoder would corrupt the picture, so the data plane must demux. Video is
/// either `FRAME:`-prefixed or a bare Annex-B unit (NAL start code); anything else
/// (e.g. AAC/Opus audio) is treated as non-video.
pub fn is_video_frame(payload: &[u8]) -> bool {
    if payload.len() >= FRAME_PREFIX.len() && &payload[..FRAME_PREFIX.len()] == FRAME_PREFIX {
        return true;
    }
    payload.starts_with(&[0, 0, 0, 1]) || payload.starts_with(&[0, 0, 1])
}

/// Bytes of the ADTS header the phone puts on every AAC access unit.
pub const ADTS_HEADER_LEN: usize = 7;

/// Is this binary mirror message an AAC audio packet?
///
/// The phone's `AACEncoder.addADTStoPacket` prepends a 7-byte **ADTS** header to
/// every access unit (`ff f9 50 …` = MPEG-2 AAC-LC, no CRC), and
/// `ChannelHandler.writeAudio` puts it on the *same* WS as video with no extra
/// framing — so the syncword is the only thing telling audio and video apart.
/// Test it on the payload with any `FRAME:` prefix already stripped: with
/// `frame_with_time` the phone tags audio packets too.
pub fn is_audio_frame(payload: &[u8]) -> bool {
    // syncword 0xFFF + layer == 00 (mask keeps the sync low nibble and the layer
    // bits; the ID and protection_absent bits are free to vary).
    payload.len() > ADTS_HEADER_LEN && payload[0] == 0xFF && (payload[1] & 0xF6) == 0xF0
}

/// Sample rate carried in an ADTS header, or `None` if the index is reserved.
/// The phone encodes at 44100 Hz, but it honours a request override, so read it
/// from the stream rather than assuming.
pub fn adts_sample_rate(header: &[u8]) -> Option<u32> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    RATES
        .get(((header.get(2)? >> 2) & 0x0F) as usize)
        .copied()
}

/// Channel configuration carried in an ADTS header (0 = "in the AAC payload").
pub fn adts_channels(header: &[u8]) -> Option<u8> {
    let hi = (header.get(2)? & 0x01) << 2;
    Some(hi | (header.get(3)? >> 6))
}

/// Privacy / secure-screen state the phone reports via `NOTIFY_PASS:`. When the
/// phone shows a secure surface (fingerprint, password entry, lock screen) it
/// stops the mirror stream and sends this so the PC can show a "handle on phone"
/// prompt instead of a frozen / black picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyState {
    /// Back to a normal, mirrorable screen.
    Clear,
    /// A password / PIN entry surface.
    Password,
    /// A `FLAG_SECURE` window (e.g. fingerprint, banking, DRM).
    Secure,
    /// The device lock screen.
    LockScreen,
}

impl PrivacyState {
    /// Stable wire token used across the FFI boundary (matches the phone's
    /// `privacyState` strings; `Clear` collapses the "not secure" cases).
    pub fn token(self) -> &'static str {
        match self {
            PrivacyState::Clear => "clear",
            PrivacyState::Password => "password",
            PrivacyState::Secure => "safety",
            PrivacyState::LockScreen => "lockScreen",
        }
    }
}

/// Parse a `NOTIFY_PASS:{…}` control message into a [`PrivacyState`].
/// Returns `None` for any other message. `isPass=false` → [`PrivacyState::Clear`].
pub fn parse_notify_pass(line: &str) -> Option<PrivacyState> {
    let body = line.strip_prefix("NOTIFY_PASS:")?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if !v.get("isPass").and_then(|x| x.as_bool()).unwrap_or(false) {
        return Some(PrivacyState::Clear);
    }
    Some(match v.get("privacyState").and_then(|x| x.as_str()).unwrap_or("") {
        "password" => PrivacyState::Password,
        "lockScreen" => PrivacyState::LockScreen,
        // "safety" or any other non-empty state under isPass=true → treat as secure.
        _ => PrivacyState::Secure,
    })
}

/// Out-of-band privacy-channel token for "the device keyguard is locked" (derived
/// from `DEVICE_INFO:is_lock`, not `NOTIFY_PASS`). Kept distinct from the
/// [`PrivacyState`] tokens so the PC can track the keyguard lock independently of the
/// foreground-window privacy state — the phone reports `NOTIFY_PASS:clear` even while
/// locked, which must not clear the "please unlock" prompt.
pub const SCREEN_LOCKED: &str = "screenLocked";

/// Parse the phone's `DEVICE_INFO:{…}` reply for its lock flag (`is_lock`).
///
/// The phone sends `DEVICE_INFO` on the **mirror WS** right after `SCREEN_START`.
/// `is_lock:true` means the device is on its lock screen — and on Android 16+ the
/// phone then *blocks* the stream (sends no frames) until the user unlocks, after
/// which it resumes on its own. Surfacing this lets the PC prompt "unlock your
/// phone" instead of sitting on a frozen black picture. Returns `None` for any
/// other message.
pub fn parse_device_info_lock(line: &str) -> Option<bool> {
    let body = line.strip_prefix("DEVICE_INFO:")?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(v.get("is_lock").and_then(|x| x.as_bool()).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_start_default_keys() {
        let s = screen_start(&ScreenParams::default());
        assert!(s.starts_with("SCREEN_START:"));
        let json: serde_json::Value = serde_json::from_str(&s["SCREEN_START:".len()..]).unwrap();
        assert_eq!(json["mime_type"], "video/hevc");
        assert_eq!(json["max_size"], 2336);
        assert_eq!(json["screenPrivacy"], false);
        assert_eq!(json["no_audio"], true);
        // Must be ABSENT, not false: the phone enables timestamped framing on the
        // key's presence alone, which also re-frames audio and breaks the demux.
        assert!(json.get("frame_with_time").is_none());
        let on = screen_start(&ScreenParams { frame_with_time: true, ..ScreenParams::default() });
        assert!(on.contains("\"frame_with_time\":true"));
    }

    #[test]
    fn frame_header_strip_with_timestamps() {
        // FRAME: + 8B millis + 2B short + payload
        let mut pkt = b"FRAME:".to_vec();
        pkt.extend_from_slice(&1_753_000_000_000u64.to_be_bytes());
        pkt.extend_from_slice(&7u16.to_be_bytes());
        pkt.extend_from_slice(&[0xFF, 0xF9, 0x50, 0xA0, 0x01, 0xA0, 0x00, 0x21]);
        let body = strip_frame_header(&pkt, true);
        assert!(is_audio_frame(body), "timestamped audio must still read as audio");
        // Without the flag the 10-byte header stays, and the packet is unrecognisable —
        // the exact failure that sent audio into the video decoder.
        assert!(!is_audio_frame(strip_frame_header(&pkt, false)));
    }

    #[test]
    fn connect_center_roundtrip() {
        let raw = r#"connectCenterMsg:{"data":{"deviceId":"d114…"},"msgId":"1789179743827","name":"openVivoScreen"}"#;
        let m = parse_connect_center(raw).expect("parses");
        assert_eq!(m.name, "openVivoScreen");
        assert_eq!(m.msg_id, "1789179743827");
        assert!(parse_connect_center("normal").is_none());
        assert!(parse_connect_center("connectCenterMsg:not json").is_none());

        let reply = connect_center_result(&m.name, &m.msg_id, 0, "");
        let body = reply.strip_prefix("connectCenterResult:").expect("prefix");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["name"], "openVivoScreen");
        assert_eq!(v["msgId"], "1789179743827");
        assert_eq!(v["data"]["code"], 0);
    }

    #[test]
    fn req_authrity_format() {
        assert_eq!(req_authrity(1), r#"req_authrity{"source":1}"#);
    }

    #[test]
    fn frame_prefix_strip() {
        assert_eq!(strip_frame_prefix(b"FRAME:abc"), b"abc");
        assert_eq!(strip_frame_prefix(b"abc"), b"abc");
        assert_eq!(strip_frame_prefix(b"FRAME:"), b"");
    }

    /// The exact header `AACEncoder.addADTStoPacket` builds for a 100-byte AU:
    /// 0xFF 0xF9 = syncword + MPEG-2 + no CRC, 0x50 = AAC-LC / 44100 Hz,
    /// then channel_configuration = 2 and the 13-bit frame length.
    fn phone_adts(au_len: usize) -> Vec<u8> {
        let n = au_len + ADTS_HEADER_LEN;
        let mut v = vec![
            0xFF,
            0xF9,
            0x50,
            ((n >> 11) as u8) + 128,
            ((n & 0x7FF) >> 3) as u8,
            (((n & 7) << 5) as u8) + 31,
            0xFC,
        ];
        v.extend(std::iter::repeat(0xAB).take(au_len));
        v
    }

    #[test]
    fn audio_frames_are_recognised_and_described() {
        let pkt = phone_adts(100);
        assert!(is_audio_frame(&pkt));
        assert!(!is_video_frame(&pkt));
        assert_eq!(adts_sample_rate(&pkt), Some(44100));
        assert_eq!(adts_channels(&pkt), Some(2));
    }

    #[test]
    fn video_is_not_mistaken_for_audio() {
        assert!(!is_audio_frame(&[0, 0, 0, 1, 0x26, 0x01, 0xAF, 0x00]));
        assert!(!is_audio_frame(b"FRAME:\x00\x00\x00\x01xx"));
        assert!(!is_audio_frame(&[0xFF, 0xF9])); // header only, no payload
    }

    #[test]
    fn notify_pass_parse() {
        assert_eq!(parse_notify_pass("MOUSE_EVENT:{}"), None);
        assert_eq!(
            parse_notify_pass(r#"NOTIFY_PASS:{"appName":"x","isPass":false,"privacyState":""}"#),
            Some(PrivacyState::Clear)
        );
        assert_eq!(
            parse_notify_pass(r#"NOTIFY_PASS:{"isPass":true,"privacyState":"password"}"#),
            Some(PrivacyState::Password)
        );
        assert_eq!(
            parse_notify_pass(r#"NOTIFY_PASS:{"isPass":true,"privacyState":"safety"}"#),
            Some(PrivacyState::Secure)
        );
        assert_eq!(
            parse_notify_pass(r#"NOTIFY_PASS:{"isPass":true,"privacyState":"lockScreen"}"#),
            Some(PrivacyState::LockScreen)
        );
    }

    #[test]
    fn device_info_lock_parse() {
        assert_eq!(parse_device_info_lock("SCREEN_START:{}"), None);
        assert_eq!(
            parse_device_info_lock(r#"DEVICE_INFO:{"sessionId":1,"is_lock":true}"#),
            Some(true)
        );
        assert_eq!(
            parse_device_info_lock(r#"DEVICE_INFO:{"sessionId":1,"is_lock":false}"#),
            Some(false)
        );
        // Missing field → not locked (older phones omit it).
        assert_eq!(parse_device_info_lock(r#"DEVICE_INFO:{"sessionId":1}"#), Some(false));
    }
}
