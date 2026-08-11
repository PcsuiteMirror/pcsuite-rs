//! LAN presence: keep announcing this PC so phones can find it.
//!
//! The phone's "connect to a computer" search is **passive** — it listens on the
//! SSDP group for a PC beacon and lists whatever answers. A PC that is bound to
//! the account but never beacons shows up as *not discovered*, no matter how
//! correctly it is registered with the connection center (verified 2026-08-11
//! against a second PC on the same LAN, which the phone did find: the only
//! difference was that it was beaconing).
//!
//! [`connect`](crate::connect) already announces during the connect handshake,
//! but only unicast to one phone and only for the seconds the handshake lasts.
//! This module runs the same payload as a continuous multicast beacon for as
//! long as the app is up — what the official client does.
//!
//! Purely local: no server, no account API. It is therefore appropriate in both
//! modes (see [`config::Mode`]).

use std::time::Duration;

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use tokio::task::JoinHandle;

use pcsuite_proto::connect::Presence;

use crate::config;

/// How often to beacon. The official client sends one about every 5 seconds.
const INTERVAL: Duration = Duration::from_secs(5);

/// A running presence beacon; announcing stops when this is dropped.
pub struct PresenceBeacon {
    handle: JoinHandle<std::io::Result<()>>,
}

impl Drop for PresenceBeacon {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Start beaconing this PC's identity on the LAN.
///
/// The payload is built fresh from the current identity, so a settings change
/// takes effect on the next start. Fails only if the identity can't be encoded;
/// a down or changing network is handled inside the loop.
pub fn start() -> Result<PresenceBeacon> {
    let id = config::default_identity();
    let payload = Presence {
        device_id: &id.pc_mac,
        open_id: &id.open_id,
        account: &id.account,
        device_name: &id.device_name,
        service_record: config::SERVICE_RECORD,
        port: 10191,
        device_type: "pc",
        extra: "null",
    };
    let b64 = STANDARD.encode(serde_json::to_vec(&payload).context("encode presence")?);

    tracing::info!(
        device_id = %id.pc_mac,
        device_name = %id.device_name,
        "LAN presence beacon started"
    );
    Ok(PresenceBeacon {
        handle: tokio::spawn(pcsuite_net::ssdp::announce_loop(b64, INTERVAL)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beacon_payload_matches_the_official_field_set() {
        // Field-for-field with a beacon captured from the official Windows
        // client (values here are synthetic).
        let payload = Presence {
            device_id: "aabbccddeeff",
            open_id: "0123456789abcdef",
            account: "138****000",
            device_name: "a-pc",
            service_record: config::SERVICE_RECORD,
            port: 10191,
            device_type: "pc",
            extra: "null",
        };
        let v: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&payload).unwrap()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "account",
                "deviceId",
                "deviceName",
                "deviceType",
                "extra",
                "openId",
                "port",
                "serviceRecord"
            ]
        );
        assert_eq!(v["deviceType"], "pc");
        assert_eq!(v["port"], 10191);
        assert_eq!(v["extra"], "null"); // the string "null", as the phone expects
    }
}
