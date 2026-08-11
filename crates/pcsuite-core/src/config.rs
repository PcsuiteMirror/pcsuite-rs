//! Protocol-mandated wire constants and runtime-loaded PC identity.
//!
//! The string constants below (`service_id`, `serviceRecord`, the WS subprotocol)
//! contain the upstream vendor name because the **phone requires those exact
//! values on the wire** — they are protocol identifiers, not names we are free to
//! choose. They are intentionally the only place such strings appear.
//!
//! The *identity* values — account openId, PC MAC, display name, and the per-IP
//! pairing seeds — are deployment-specific and are NEVER hardcoded here. They are
//! loaded at runtime (see [`default_identity`] / [`default_stored_seed`]) from, in
//! priority order:
//!   1. environment variables (`PCSUITE_OPEN_ID`, `PCSUITE_PC_MAC`,
//!      `PCSUITE_ACCOUNT`, `PCSUITE_DEVICE_NAME`, `PCSUITE_SEED`),
//!   2. a JSON config file (`$PCSUITE_CONFIG`, else `./pcsuite.json`, else
//!      `$HOME/.config/pcsuite/config.json`),
//!   3. obviously-fake placeholder defaults.
//! See `pcsuite.example.json` for the file format.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use pcsuite_proto::PcIdentity;

/// Service id the phone matches against (protocol-mandated wire constant).
pub const SERVICE_ID: &str = "com.vivo.pcsuite.SERVICE";

/// SSDP `serviceRecord` advertised in presence (protocol-mandated wire constant).
pub const SERVICE_RECORD: &str = "com.vivo.pcsuite.SERVICE--com.vivo.share.CONNECT_PC";

/// Base WebSocket subprotocol for the control/mirror channels (protocol-mandated).
/// The control channel appends `,<token>`; the mirror channel uses it bare.
pub const SUBPROTOCOL_BASE: &str = "v1.hc.vivo.com.cn";

/// Placeholder PC device id (super-clipboard) when nothing is configured. The
/// phone routes its clipboard *pushes* to the id this PC registered at pairing, so
/// the value MUST match that for phone→PC sync — set it via `clip_pc_id` in the
/// config / `PCSUITE_CLIP_PC_ID` / [`set_clip_pc_id`]. See [`clip_pc_id`].
const CLIP_PC_ID_DEFAULT: &str = "pc0000";

/// Display nickname announced in SHADOW_LIKE / clipboard content.
pub const CLIP_NICK: &str = "pcsuite";

/// Fixed JSON `id` field used in ConnectFlow frames.
pub const FRAME_ID: i64 = 87654321;

/// Which identity source the app runs against. Chosen once by the user; every
/// other behaviour follows from it.
///
/// - [`Mode::Serverless`] (default) — no vendor server is ever contacted. The
///   identity values come from the config file / settings panel, and pairing is
///   USB, a hand-entered LAN IP, or the local QR flow (`pair.rs`).
/// - [`Mode::VivoAccount`] — the user signs in with a vivo account, which
///   supplies the openId and registers this PC with the connection center (see
///   [`crate::cloud`]) so the phone can list it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Serverless,
    VivoAccount,
}

impl Mode {
    /// Parse the wire/config spelling; anything unrecognised is serverless, so a
    /// typo can never silently opt a user into contacting a server.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "vivo" | "vivo_account" | "vivoaccount" | "account" | "cloud" => Mode::VivoAccount,
            _ => Mode::Serverless,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Serverless => "serverless",
            Mode::VivoAccount => "vivo_account",
        }
    }

    /// Whether this mode is allowed to make cloud requests.
    pub fn uses_cloud(self) -> bool {
        self == Mode::VivoAccount
    }
}

/// Runtime identity + pairing seeds, parsed once from env / config file.
struct UserConfig {
    open_id: String,
    pc_mac: String,
    account: String,
    device_name: String,
    clip_pc_id: String,
    /// Optional single seed used for any IP without a per-IP entry.
    default_seed: Option<String>,
    /// Per-IP stored pairing seeds (`ip` -> seed UUID).
    seeds: HashMap<String, String>,
    mode: Mode,
}

fn load() -> &'static UserConfig {
    static CFG: OnceLock<UserConfig> = OnceLock::new();
    CFG.get_or_init(|| {
        let file = read_config_file();
        // env var wins over file value, file value wins over the placeholder.
        let pick = |env_key: &str, json_key: &str, default: &str| -> String {
            std::env::var(env_key)
                .ok()
                .or_else(|| file.get(json_key).and_then(|v| v.as_str()).map(str::to_owned))
                .unwrap_or_else(|| default.to_owned())
        };

        let mut seeds = HashMap::new();
        if let Some(map) = file.get("seeds").and_then(|v| v.as_object()) {
            for (ip, v) in map {
                if let Some(s) = v.as_str() {
                    seeds.insert(ip.clone(), s.to_owned());
                }
            }
        }
        let default_seed = std::env::var("PCSUITE_SEED")
            .ok()
            .or_else(|| file.get("seed").and_then(|v| v.as_str()).map(str::to_owned));

        UserConfig {
            open_id: pick("PCSUITE_OPEN_ID", "open_id", OPEN_ID_PLACEHOLDER),
            pc_mac: pick("PCSUITE_PC_MAC", "pc_mac", "000000000000"),
            account: pick("PCSUITE_ACCOUNT", "account", ""),
            device_name: pick("PCSUITE_DEVICE_NAME", "device_name", "pcsuite-pc"),
            clip_pc_id: pick("PCSUITE_CLIP_PC_ID", "clip_pc_id", CLIP_PC_ID_DEFAULT),
            default_seed,
            seeds,
            mode: Mode::parse(&pick("PCSUITE_MODE", "mode", Mode::Serverless.as_str())),
        }
    })
}

/// Locate and parse the JSON config file; returns `Null` if absent/unreadable.
fn read_config_file() -> serde_json::Value {
    let path = std::env::var("PCSUITE_CONFIG")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            let cwd = std::path::PathBuf::from("pcsuite.json");
            cwd.exists().then_some(cwd)
        })
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::PathBuf::from(h).join(".config/pcsuite/config.json"))
                .filter(|p| p.exists())
        });

    if let Some(p) = path {
        match std::fs::read_to_string(&p) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(v) => return v,
                Err(e) => {
                    tracing::warn!(path = %p.display(), %e, "invalid PCSUITE config JSON; using defaults")
                }
            },
            Err(e) => {
                tracing::warn!(path = %p.display(), %e, "cannot read PCSUITE config; using defaults")
            }
        }
    }
    serde_json::Value::Null
}

/// Runtime overrides set by an embedding app (e.g. the SwiftUI settings panel via
/// FFI). Take precedence over the env/file/placeholder base, so a GUI can supply
/// the pairing identity without a config file. Empty strings clear a field.
#[derive(Default)]
struct Overrides {
    open_id: Option<String>,
    pc_mac: Option<String>,
    account: Option<String>,
    device_name: Option<String>,
    clip_pc_id: Option<String>,
    seeds: HashMap<String, String>,
    mode: Option<Mode>,
}

fn overrides() -> &'static RwLock<Overrides> {
    static O: OnceLock<RwLock<Overrides>> = OnceLock::new();
    O.get_or_init(|| RwLock::new(Overrides::default()))
}

/// Override the PC identity at runtime. An empty field falls back to the
/// env/file/placeholder value. Call before connecting.
pub fn set_identity(open_id: String, pc_mac: String, account: String, device_name: String) {
    let opt = |s: String| (!s.is_empty()).then_some(s);
    let mut o = overrides().write().unwrap();
    o.open_id = opt(open_id);
    o.pc_mac = opt(pc_mac);
    o.account = opt(account);
    o.device_name = opt(device_name);
}

/// Override (empty = clear) the super-clipboard PC device id. Must match the id
/// the phone registered for this PC at pairing, or phone→PC clipboard won't push.
pub fn set_clip_pc_id(id: String) {
    overrides().write().unwrap().clip_pc_id = (!id.is_empty()).then_some(id);
}

/// Select the identity mode at runtime (the settings panel does this at startup).
pub fn set_mode(mode: Mode) {
    overrides().write().unwrap().mode = Some(mode);
}

/// The active mode: runtime override, then `PCSUITE_MODE` / the config file's
/// `"mode"`, then [`Mode::Serverless`].
pub fn mode() -> Mode {
    overrides().read().unwrap().mode.unwrap_or_else(|| load().mode)
}

/// Placeholder openId from the env/file/placeholder base — an obviously-fake value
/// that won't pass the phone's account check. Keep in sync with `load()`.
pub const OPEN_ID_PLACEHOLDER: &str = "0000000000000000";

/// Override just the account openId (empty = clear). Used to self-fill the openId
/// learned from the phone's `/base-info` when a session connected without one (e.g.
/// QR pairing), so the cowork clipboard handshake carries the real account value.
pub fn set_open_id(open_id: String) {
    overrides().write().unwrap().open_id = (!open_id.is_empty()).then_some(open_id);
}

/// Whether a *real* account openId is configured (not empty, not the placeholder).
pub fn has_open_id() -> bool {
    let id = default_identity().open_id;
    !id.is_empty() && id != OPEN_ID_PLACEHOLDER
}

/// The super-clipboard PC device id: runtime override, then env / config file,
/// then the placeholder. Resolved fresh each call (no caching) so a settings
/// change followed by a reconnect takes effect without restarting.
pub fn clip_pc_id() -> String {
    if let Some(id) = &overrides().read().unwrap().clip_pc_id {
        return id.clone();
    }
    load().clip_pc_id.clone()
}

/// Override (or, with an empty `seed`, clear) the stored pairing seed for one IP.
pub fn set_seed(ip: String, seed: String) {
    let mut o = overrides().write().unwrap();
    if seed.is_empty() {
        o.seeds.remove(&ip);
    } else {
        o.seeds.insert(ip, seed);
    }
}

/// The PC identity used by default: runtime overrides win, then env / config file
/// / placeholders. With nothing configured it returns obviously-fake values that
/// will not pair with a real phone.
pub fn default_identity() -> PcIdentity {
    let c = load();
    let o = overrides().read().unwrap();
    let pick = |ov: &Option<String>, base: &str| ov.clone().unwrap_or_else(|| base.to_owned());
    PcIdentity {
        open_id: pick(&o.open_id, &c.open_id),
        pc_mac: pick(&o.pc_mac, &c.pc_mac),
        account: pick(&o.account, &c.account),
        device_name: pick(&o.device_name, &c.device_name),
        service_id: SERVICE_ID.into(),
        frame_id: FRAME_ID,
    }
}

/// Per-IP stored pairing seed (historyPhone `ext.seeds`), used for the LAN
/// `connectType=2` path. Runtime override wins, then the per-IP config entry, then
/// the single `PCSUITE_SEED` / `"seed"` value. The remote `connectType=1` path
/// needs no seed.
pub fn default_stored_seed(ip: &str) -> Option<String> {
    if let Some(s) = overrides().read().unwrap().seeds.get(ip) {
        return Some(s.clone());
    }
    let c = load();
    c.seeds.get(ip).cloned().or_else(|| c.default_seed.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_spellings_map_to_vivo_account() {
        for s in ["vivo", "vivo_account", "vivoAccount", "account", "cloud", " CLOUD "] {
            assert_eq!(Mode::parse(s), Mode::VivoAccount, "{s:?}");
        }
    }

    #[test]
    fn unknown_mode_falls_back_to_serverless() {
        // A typo must never silently opt the user into contacting a server.
        for s in ["", "serverless", "vivoo", "local", "true"] {
            assert_eq!(Mode::parse(s), Mode::Serverless, "{s:?}");
        }
        assert!(!Mode::Serverless.uses_cloud());
        assert!(Mode::VivoAccount.uses_cloud());
    }

    #[test]
    fn mode_round_trips_through_its_string_form() {
        for m in [Mode::Serverless, Mode::VivoAccount] {
            assert_eq!(Mode::parse(m.as_str()), m);
        }
    }
}
