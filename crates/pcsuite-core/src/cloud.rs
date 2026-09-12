//! vivo-account mode: register this PC with the vendor's connection center.
//!
//! Only reached when [`config::mode`] is [`config::Mode::VivoAccount`]. The
//! serverless mode (USB / LAN / local QR pairing) never talks to any server and
//! is unaffected by everything here.
//!
//! # What the cloud is for
//!
//! Registering makes the PC show up in the phone's connection center, which is
//! what lets the *phone* start a session instead of the PC always dialling out.
//! The wake-up itself rides the vendor's MQTT push channel and is **not
//! implemented yet** — see `docs/VIVO_ACCOUNT_LOGIN.md` §5. What is implemented:
//! account credentials, device registration, and the device roster (which
//! carries each phone's current LAN address, so a connect target can come from
//! the cloud instead of being typed in).
//!
//! # Wire facts (from the official client's own plaintext request logs)
//!
//! - Base: `https://connection-center.vivo.com.cn` (region-specific hosts exist;
//!   `cn` is the only one this implements).
//! - Auth is **six plain headers, no signature**: `userId` (the account openId),
//!   `token` (the account token from login), `source: 2` (= PC), `version`,
//!   `countryCode`, `deviceId`.
//! - `POST /device/report` takes a JSON **array** of device objects.
//!
//! # deviceId
//!
//! `SHA256(lowercased IOPlatformUUID) || IOPlatformSerialNumber` — verified to
//! reproduce the official client's id for this machine byte for byte. Its first
//! 6 hex digits are the super-clipboard `clip_pc_id`, which is why logging in
//! also fixes up that value (previously it had to be copied out of an existing
//! official pairing).

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use pcsuite_net::https;

use crate::config;

/// Connection-center host for the `cn` region.
pub const CONNECT_CENTER_HOST: &str = "connection-center.vivo.com.cn";

/// `source` header value identifying the caller as a PC client.
const SOURCE_PC: &str = "2";

/// `version` header — the client version the cloud expects to see. Kept at the
/// official build we captured; the API rejects implausibly old values.
const CLIENT_VERSION: &str = "6.6.0.0";

/// `type` field in a device report: 3 = PC.
const DEVICE_TYPE_PC: i64 = 3;

/// Account credentials obtained by the QR login (see `docs/VIVO_ACCOUNT_LOGIN.md` §2).
///
/// `open_id` doubles as the LAN identity's openId — the phone checks the LAN
/// sign against exactly this value — so a successful login also supplies the one
/// piece of identity that previously had to be entered by hand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// Account openId; sent as the `userId` header.
    pub open_id: String,
    /// Account token, `<32 hex>.<epoch millis>`. Expires — re-login on 401.
    pub token: String,
    /// Region of the account, e.g. `cn`.
    pub country_code: String,
}

impl Account {
    pub fn new(open_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            open_id: open_id.into(),
            token: token.into(),
            country_code: "cn".into(),
        }
    }

    /// Whether both credential fields are non-empty. Does not prove the token is
    /// still valid — only the server can say that.
    pub fn is_complete(&self) -> bool {
        !self.open_id.is_empty() && !self.token.is_empty()
    }
}

/// Where the CLI keeps the signed-in account: `~/.config/pcsuite/account.json`,
/// written `0600`. The macOS app does not use this — it keeps the token in the
/// keychain and pushes it in over FFI.
pub fn account_path() -> Option<std::path::PathBuf> {
    std::env::var("PCSUITE_ACCOUNT_FILE")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::PathBuf::from(h).join(".config/pcsuite/account.json"))
        })
}

/// Load the stored account, if any. A missing or malformed file is simply "not
/// signed in".
pub fn load_account() -> Option<Account> {
    let text = std::fs::read_to_string(account_path()?).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let acc = Account {
        open_id: s("open_id"),
        token: s("token"),
        country_code: {
            let c = s("country_code");
            if c.is_empty() { "cn".into() } else { c }
        },
    };
    acc.is_complete().then_some(acc)
}

/// Persist the account for later CLI runs, owner-readable only.
pub fn save_account(acc: &Account) -> Result<()> {
    let path = account_path().context("no HOME to store the account under")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let body = serde_json::to_string_pretty(&json!({
        "open_id": acc.open_id,
        "token": acc.token,
        "country_code": acc.country_code,
    }))?;
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(())
}

/// Forget the stored account (sign out).
pub fn clear_account() -> Result<()> {
    if let Some(p) = account_path() {
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
        }
    }
    Ok(())
}

/// One device as the connection center knows it.
#[derive(Clone, Debug, Default)]
pub struct CloudDevice {
    /// Cloud-side device id (the long one).
    pub device_id: String,
    /// Short id used on the wire — the phone's is the `bleId` the QR-pairing
    /// callback reports, a PC's is its MAC. Lives under `bizInfo.<biz>.businessId`.
    pub external_id: String,
    pub name: String,
    pub model: String,
    /// 1 = phone, 2 = pad, 3 = PC (mirrors the `type` we report).
    pub device_type: i64,
    /// LAN addresses the device last reported — a connect target for LAN mode.
    pub inets: Vec<String>,
    /// `pushInfo.clientId`: the device's MQTT push registration. Empty means the
    /// cloud has no way to reach it, which the phone shows as "not discovered".
    pub push_client_id: String,
    /// When the device last reported itself, e.g. `2026-08-11 10:59:09.374`. The
    /// API has no online flag, so freshness of this is the only liveness signal.
    pub report_time: String,
    /// Per-IP LAN pairing seeds the phone published (`ext.seeds`), keyed by IP
    /// with dots turned into underscores upstream — normalised back here. These
    /// are exactly the `connectType=2` seeds that otherwise have to be copied out
    /// of an official pairing.
    pub seeds: HashMap<String, String>,
}

impl CloudDevice {
    /// First usable LAN address, if the device reported one.
    pub fn ip(&self) -> Option<&str> {
        self.inets.iter().find(|s| !s.is_empty()).map(String::as_str)
    }

    /// Whether this entry is a phone (i.e. a plausible connect target).
    pub fn is_phone(&self) -> bool {
        self.device_type != DEVICE_TYPE_PC
    }

    /// The LAN address to dial for this device *right now*: one sharing a /24 with a
    /// local address wins, because the list also carries addresses the device reported
    /// on other networks (a phone that roamed keeps a `192.168.1.x` entry while we are
    /// on `192.168.31.x`). Falls back to the first address it reported.
    pub fn reachable_ip(&self) -> Option<String> {
        let locals: Vec<String> = local_ipv4s().iter().filter_map(|s| subnet24(s)).collect();
        self.inets
            .iter()
            .find(|ip| subnet24(ip).map(|s| locals.contains(&s)).unwrap_or(false))
            .or_else(|| self.inets.first())
            .cloned()
    }

    /// The `connectType=2` seed the phone published for `ip` (its seeds are per-IP), or
    /// any it published if that address has none.
    pub fn seed_for(&self, ip: &str) -> Option<String> {
        self.seeds.get(ip).or_else(|| self.seeds.values().next()).cloned()
    }
}

/// The `a.b.c` /24 prefix of a dotted IPv4.
fn subnet24(ip: &str) -> Option<String> {
    let mut it = ip.split('.');
    let (a, b, c) = (it.next()?, it.next()?, it.next()?);
    it.next()?; // require a 4th octet
    Some(format!("{a}.{b}.{c}"))
}

/// Where to reach the account's phone on the LAN, as the connection center reports it.
#[derive(Debug, Clone)]
pub struct PhoneTarget {
    pub name: String,
    pub ip: String,
    /// Per-IP `connectType=2` seed, when the phone published one for this address.
    pub seed: Option<String>,
}

/// This PC's cloud device id: `SHA256(platform UUID)` + hardware serial.
///
/// Stable across reinstalls and identical to the official client's id for the
/// same machine, so registering does not create a duplicate entry alongside an
/// existing official pairing.
pub fn pc_device_id() -> Result<String> {
    let (uuid, serial) = platform_ids()?;
    Ok(format!("{}{}", sha256_hex(uuid.to_lowercase().as_bytes()), serial))
}

/// The super-clipboard PC id derived from [`pc_device_id`] — its first 6 hex
/// digits, which is how the official client derives it too.
pub fn derived_clip_pc_id() -> Result<String> {
    Ok(pc_device_id()?.chars().take(6).collect())
}

/// Read this machine's platform UUID and serial number.
#[cfg(target_os = "macos")]
fn platform_ids() -> Result<(String, String)> {
    let out = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .context("run ioreg to read the platform id")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let field = |key: &str| -> Option<String> {
        text.lines()
            .find(|l| l.contains(key))
            .and_then(|l| l.split('=').nth(1))
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    };

    let uuid = field("IOPlatformUUID").context("no IOPlatformUUID in ioreg output")?;
    let serial = field("IOPlatformSerialNumber").unwrap_or_default();
    Ok((uuid, serial))
}

#[cfg(not(target_os = "macos"))]
fn platform_ids() -> Result<(String, String)> {
    anyhow::bail!("cloud device id derivation is implemented for macOS only")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(pcsuite_crypto::sha256(bytes))
}

/// A configured connection-center client.
pub struct ConnectCenter {
    account: Account,
    device_id: String,
    host: String,
}

impl ConnectCenter {
    /// Build a client for `account`, deriving this PC's device id.
    pub fn new(account: Account) -> Result<Self> {
        if !account.is_complete() {
            bail!("not signed in: vivo-account mode needs an openId + token (run the QR login)");
        }
        Ok(Self {
            device_id: pc_device_id()?,
            account,
            host: CONNECT_CENTER_HOST.into(),
        })
    }

    /// Point the client at a different region host (default: [`CONNECT_CENTER_HOST`]).
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The six auth/context headers every connection-center call carries.
    fn headers(&self) -> Vec<(String, String)> {
        let country = if self.account.country_code.is_empty() {
            "cn"
        } else {
            &self.account.country_code
        };
        vec![
            ("userId".into(), self.account.open_id.clone()),
            ("token".into(), self.account.token.clone()),
            ("source".into(), SOURCE_PC.into()),
            ("version".into(), CLIENT_VERSION.into()),
            ("countryCode".into(), country.into()),
            ("deviceId".into(), self.device_id.clone()),
        ]
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let encoded = body.map(|v| serde_json::to_vec(&v)).transpose()?;
        let res = https::request(
            &self.host,
            method,
            path,
            &self.headers(),
            encoded.as_deref(),
        )
        .await?;

        if res.status == 401 || res.status == 403 {
            bail!("connection center rejected the account token ({}) — sign in again", res.status);
        }
        if !res.ok() {
            bail!("{method} {path} failed: HTTP {}", res.status);
        }

        // The response shape isn't documented anywhere; log it so a mismatch is
        // diagnosable without a proxy. DEBUG, because it names the account's devices.
        tracing::debug!(
            path,
            status = res.status,
            body = %String::from_utf8_lossy(&res.body),
            "connection center response"
        );

        let v = res.json()?;
        // Envelope is `{code, msg, data}`; code 0 = success.
        let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            let msg = v.get("msg").and_then(Value::as_str).unwrap_or("unknown error");
            bail!("{path} returned code {code}: {msg}");
        }
        Ok(v)
    }

    /// Register (or refresh) this PC so the phone's connection center lists it.
    ///
    /// `inets` are this Mac's LAN addresses — the phone uses them to reach us,
    /// so an empty list registers a PC that cannot be connected to.
    ///
    /// `push_client_id` is the MQTT push registration to publish. **A report
    /// replaces the whole record**, so passing `None` when the cloud already
    /// holds one wipes it, and the phone then shows this PC as "not discovered".
    /// Use [`Self::register_self`], which carries the existing value over.
    pub async fn report_device(
        &self,
        name: &str,
        pc_mac: &str,
        inets: &[String],
        push_client_id: Option<&str>,
    ) -> Result<()> {
        let body = json!([self.device_payload(name, pc_mac, inets, push_client_id)]);
        self.call("POST", "/device/report", Some(body)).await?;
        tracing::info!(
            device_id = %self.device_id,
            push_client_id = push_client_id.unwrap_or("<none>"),
            "registered PC with the connection center"
        );
        Ok(())
    }

    /// The push client id the cloud currently holds for this PC, if any.
    async fn existing_push_client_id(&self) -> Option<String> {
        let list = self.device_list().await.ok()?;
        list.into_iter()
            .find(|d| d.device_id == self.device_id)
            .map(|d| d.push_client_id)
            .filter(|s| !s.is_empty())
    }

    /// The device object `/device/report` expects, shaped like the official one.
    fn device_payload(
        &self,
        name: &str,
        pc_mac: &str,
        inets: &[String],
        push_client_id: Option<&str>,
    ) -> Value {
        let inets: Vec<Value> = inets
            .iter()
            .map(|ip| json!({ "inet": ip, "localInet": ip, "netmark": "255.255.255.0", "ssid": "" }))
            .collect();
        // ── Experiment switch: which OS this PC claims to be ──────────────────
        // The phone's 互传 picks a transfer path by device type. A Mac-registered
        // PC becomes ShareDevice type 6, whose branch never serves files itself —
        // it calls `sendFilesByPCSuite()` and so needs a live pcsuite session. A
        // Windows-registered PC takes the type 3/4 branch, which is the plain-LAN
        // flow this crate already implements (see `share.rs`). Setting
        // `PCSUITE_CLOUD_OS=windows` registers with the official Windows client's
        // identity values so that path can be tested; anything else (the default)
        // registers honestly as a Mac.
        let claim_windows = std::env::var("PCSUITE_CLOUD_OS")
            .map(|v| v.eq_ignore_ascii_case("windows"))
            .unwrap_or(false);
        let (os_name, company, model) = if claim_windows {
            ("Windows", "LENOVO", "82GL".to_string())
        } else {
            ("Mac", "apple", host_model())
        };
        let mut ext = json!({
            "deviceType": os_name,
            "businessId": pc_mac,
            "pc_pcsuite_version": CLIENT_VERSION,
        });
        let mut device = json!({
            "userId": self.account.open_id,
            "deviceId": self.device_id,
            "name": name,
            "model": model,
            "company": company,
            "type": DEVICE_TYPE_PC,
            "inets": inets,
            "bizInfo": { "office_suite": { "businessId": pc_mac } },
            "bluetooth": pc_mac,
            "wifiSwitch": 1,
            "btSwitch": 0,
        });
        // The official client puts the push client id in both places; mirror that
        // exactly, and omit both when there is none rather than sending a null.
        if let Some(cid) = push_client_id.filter(|s| !s.is_empty()) {
            ext["clientId"] = json!(cid);
            device["pushInfo"] = json!({ "clientId": cid });
        }
        device["ext"] = ext;
        device
    }

    /// Every device bound to this account, phones included.
    pub async fn device_list(&self) -> Result<Vec<CloudDevice>> {
        let v = self.call("GET", "/device/list", None).await?;
        Ok(parse_device_list(&v))
    }

    /// Where to reach the account's phone on the LAN right now — its current address and
    /// the seed that goes with it.
    ///
    /// Worth re-asking rather than remembering: a phone that leaves and comes back
    /// (pocketed, Wi-Fi off, another network) usually returns on a different address, and
    /// its `connectType=2` seed is **per address** — so a presence hold that keeps dialing
    /// the address it started with never recovers, and an upgrade signed with the old
    /// seed is rejected. Prefers a phone whose name matches `prefer_name` when the account
    /// has several.
    pub async fn phone_lan_target(&self, prefer_name: Option<&str>) -> Result<PhoneTarget> {
        let list = self.device_list().await?;
        let phones: Vec<&CloudDevice> = list.iter().filter(|d| d.is_phone()).collect();
        let phone = prefer_name
            .and_then(|n| phones.iter().find(|d| d.name == n).copied())
            .or_else(|| phones.first().copied())
            .context("account has no phone in its device list")?;
        let ip = phone
            .reachable_ip()
            .filter(|ip| !ip.is_empty())
            .context("the phone has not reported a LAN address")?;
        Ok(PhoneTarget {
            name: phone.name.clone(),
            seed: phone.seed_for(&ip),
            ip,
        })
    }

    /// Remove this PC from the account.
    pub async fn unbind(&self) -> Result<()> {
        let path = format!("/device/unBind?deviceId={}", self.device_id);
        self.call("POST", &path, None).await?;
        tracing::info!(device_id = %self.device_id, "unregistered PC from the connection center");
        Ok(())
    }

    /// Register this PC using the identity the rest of the core already resolves
    /// (device name / PC MAC from [`config::default_identity`]) plus the detected
    /// LAN addresses. The one call a frontend needs after a successful login.
    pub async fn register_self(&self) -> Result<()> {
        self.register_self_with(None).await
    }

    /// As [`Self::register_self`], but publishing `push_client_id` instead of
    /// whatever the cloud already holds. Only for a caller that owns a real push
    /// registration — publishing an id nothing is listening on makes the cloud
    /// push into a void.
    pub async fn register_self_with(&self, push_client_id: Option<&str>) -> Result<()> {
        let id = config::default_identity();
        if config::is_pc_mac_placeholder(&id.pc_mac) {
            bail!(
                "refusing to register with placeholder businessId {:?}: the phone would accept the \
                 LAN connection but never link it to this device, so it stays 「未发现」. Set a real, \
                 stable businessId first — `PCSUITE_PC_MAC=<12hex> pcsuite cloud register` (it gets \
                 persisted), or add \"pc_mac\" to the config. Use the value the phone already knows \
                 this PC by (see docs/LAN_DISCOVERY_HANDOFF.md).",
                id.pc_mac
            );
        }
        let inets = local_ipv4s();
        if inets.is_empty() {
            tracing::warn!("no LAN address detected; the phone will not be able to reach this PC");
        }
        // Carry over whatever push registration this PC already has unless the
        // caller supplied one. We have no MQTT client of our own yet, and a
        // report with no `pushInfo` *clears* the stored one — which would also
        // break the official client's wake-up.
        let push_client_id = match push_client_id {
            Some(id) => Some(id.to_string()),
            None => self.existing_push_client_id().await,
        };
        if push_client_id.is_none() {
            // Discovery does NOT need a push client id — that was disproven
            // 2026-09-11. The phone lists this PC as "可连" while a 10191
            // ConnectFlow connection is held open (`pcsuite cloud presence`);
            // pushInfo.clientId is only for *remote* wake-up (vpush), a separate
            // line. See docs/LAN_DISCOVERY_HANDOFF.md.
            tracing::info!(
                "registered without a push client id — fine for LAN discovery; run \
                 `pcsuite cloud presence` to hold the connection that makes the phone show 「可连」. \
                 (pushInfo.clientId is only needed for remote wake-up.)"
            );
        }
        self.report_device(&id.device_name, &id.pc_mac, &inets, push_client_id.as_deref())
            .await
    }

    /// Fetch connection-center events. The phone's "connect" tap creates one
    /// (`handleType: CREATE_EVENT`) and the cloud normally *pushes* its id over
    /// MQTT; this asks for it over HTTP instead, which is what a client without
    /// a push channel would need. Pass `None` to try enumerating pending events.
    ///
    /// Returns the raw envelope — the response shape is not yet known.
    pub async fn events(&self, event_id: Option<&str>) -> Result<Value> {
        // The official client appends `?eventId=` / `?deviceId=` to the path —
        // POST with a query string, not a JSON body (a body yields code 20000).
        let path = match event_id {
            Some(id) => format!("/event/get?eventId={id}"),
            None => format!("/event/get?deviceId={}", self.device_id),
        };
        self.call("POST", &path, None).await
    }

    /// Issue an arbitrary connection-center call. For probing endpoints whose
    /// shape isn't known yet; returns the raw envelope.
    pub async fn raw(&self, method: &str, path: &str) -> Result<Value> {
        self.call(method, path, None).await
    }

    /// Acknowledge a connection-center event (the phone's "connect" tap creates
    /// one; the official client reports the outcome back).
    pub async fn report_event(&self, event_id: &str, success: bool) -> Result<()> {
        let body = json!({
            "eventId": event_id,
            "deviceId": self.device_id,
            "status": if success { 1 } else { 0 },
        });
        self.call("POST", "/event/report", Some(body)).await?;
        Ok(())
    }
}

/// Pull devices out of a `/device/list` envelope (`{code,msg,data:[…],ok}`).
fn parse_device_list(v: &Value) -> Vec<CloudDevice> {
    let Some(arr) = v.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };

    arr.iter()
        .map(|d| {
            let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            CloudDevice {
                device_id: s("deviceId"),
                external_id: business_id(d),
                name: s("name"),
                model: s("model"),
                device_type: d.get("type").and_then(Value::as_i64).unwrap_or(0),
                inets: parse_inets(d.get("inets")),
                report_time: s("reportTime"),
                push_client_id: d
                    .get("pushInfo")
                    .and_then(|p| p.get("clientId"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                seeds: parse_seeds(d.get("ext")),
            }
        })
        .collect()
}

/// Addresses from an `inets` array. A phone reports `{"inet": null,
/// "localInet": "192.168.x.y"}`, so `inet` being present-but-null must fall
/// through to `localInet` rather than count as an answer.
fn parse_inets(inets: Option<&Value>) -> Vec<String> {
    let Some(arr) = inets.and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            e.get("inet")
                .and_then(Value::as_str)
                .or_else(|| e.get("localInet").and_then(Value::as_str))
                .or_else(|| e.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

/// The short wire id, from whichever `bizInfo.<biz>.businessId` the device
/// carries (`conn_base` on a phone, `office_suite` on a PC).
fn business_id(d: &Value) -> String {
    d.get("bizInfo")
        .and_then(Value::as_object)
        .and_then(|biz| {
            biz.values()
                .find_map(|v| v.get("businessId").and_then(Value::as_str))
        })
        .unwrap_or("")
        .to_string()
}

/// `ext.seeds` — LAN `connectType=2` pairing seeds, keyed by IP with `.`
/// replaced by `_` (`"192_168_31_250"`). Normalised back to a real IP here.
fn parse_seeds(ext: Option<&Value>) -> HashMap<String, String> {
    let Some(map) = ext.and_then(|e| e.get("seeds")).and_then(Value::as_object) else {
        return HashMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| Some((k.replace('_', "."), v.as_str()?.to_owned())))
        .collect()
}

/// Hardware model identifier, e.g. `Mac15,6`.
fn host_model() -> String {
    std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.model"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

/// This machine's non-loopback IPv4 addresses, for `report_device`.
pub fn local_ipv4s() -> Vec<String> {
    let Ok(out) = std::process::Command::new("/sbin/ifconfig").arg("-a").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("inet ")?;
            let ip = rest.split_whitespace().next()?;
            (ip != "127.0.0.1" && !ip.starts_with("169.254.")).then(|| ip.to_string())
        })
        .collect()
}

/// Sign in and register in one step, returning this PC's device id. Convenience
/// wrapper over [`ConnectCenter::register_self`] for callers that only hold an
/// [`Account`] (the CLI).
pub async fn register_this_pc(account: Account) -> Result<String> {
    let cc = ConnectCenter::new(account)?;
    cc.register_self().await?;
    Ok(cc.device_id().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_sha256_of_the_lowercased_uuid() {
        // Synthetic UUID — the derivation itself was verified against a real
        // machine (the official client produced a byte-identical id), but no real
        // hardware identifier belongs in a public repository.
        let uuid = "0f9a1b2c-3d4e-5f60-7182-93a4b5c6d7e8";
        let digest = sha256_hex(uuid.as_bytes());
        assert_eq!(
            digest,
            "514ff9ea97ec9759ac271842afec8f659966f3aaf33c8d219501b6baebf2f76b"
        );
        // Casing matters: the official id hashes the lowercased form.
        assert_ne!(sha256_hex(uuid.to_uppercase().as_bytes()), digest);
        assert_eq!(&digest[..6], "514ff9"); // → clip_pc_id
    }

    #[test]
    fn incomplete_account_is_rejected() {
        assert!(!Account::default().is_complete());
        assert!(!Account::new("openid", "").is_complete());
        assert!(Account::new("openid", "tok.1").is_complete());
    }

    #[test]
    fn headers_carry_the_six_documented_fields() {
        let cc = ConnectCenter {
            account: Account::new("oid", "tok.1"),
            device_id: "dev".into(),
            host: CONNECT_CENTER_HOST.into(),
        };
        let h = cc.headers();
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            ["userId", "token", "source", "version", "countryCode", "deviceId"]
        );
        assert_eq!(h[2].1, "2"); // source = PC
    }

    #[test]
    fn report_payload_matches_the_official_shape() {
        let cc = ConnectCenter {
            account: Account::new("oid", "tok.1"),
            device_id: "dev".into(),
            host: CONNECT_CENTER_HOST.into(),
        };
        let p = cc.device_payload("My Mac", "aabbccddeeff", &["192.0.2.10".into()], None);
        assert_eq!(p["type"], 3);
        assert_eq!(p["userId"], "oid");
        assert_eq!(p["inets"][0]["inet"], "192.0.2.10");
        assert_eq!(p["inets"][0]["localInet"], "192.0.2.10");
        assert_eq!(p["bizInfo"]["office_suite"]["businessId"], "aabbccddeeff");
        assert_eq!(p["ext"]["deviceType"], "Mac");
    }

    #[test]
    fn a_push_client_id_lands_in_both_places_the_official_client_uses() {
        let cc = ConnectCenter {
            account: Account::new("oid", "tok.1"),
            device_id: "dev".into(),
            host: CONNECT_CENTER_HOST.into(),
        };
        let p = cc.device_payload("My Mac", "aabbccddeeff", &[], Some("1234567890"));
        assert_eq!(p["pushInfo"]["clientId"], "1234567890");
        assert_eq!(p["ext"]["clientId"], "1234567890");
    }

    #[test]
    fn no_push_client_id_omits_the_keys_rather_than_nulling_them() {
        // Regression: a report replaces the whole record, so emitting
        // `pushInfo: null` wipes the id a push client had registered — the cloud
        // then has no way to reach this PC and the phone shows it as "not
        // discovered". Absent keys must stay absent.
        let cc = ConnectCenter {
            account: Account::new("oid", "tok.1"),
            device_id: "dev".into(),
            host: CONNECT_CENTER_HOST.into(),
        };
        for empty in [None, Some("")] {
            let p = cc.device_payload("My Mac", "aabbccddeeff", &[], empty);
            assert!(p.get("pushInfo").is_none(), "pushInfo must be absent, not null");
            assert!(p["ext"].get("clientId").is_none());
        }
    }

    #[test]
    fn push_client_id_is_read_back_from_a_device_list() {
        let v = serde_json::json!({
            "code": 0,
            "data": [{ "deviceId": "dev", "type": 3, "pushInfo": { "clientId": "1234567890" } },
                     { "deviceId": "other", "type": 3, "pushInfo": null }]
        });
        let list = parse_device_list(&v);
        assert_eq!(list[0].push_client_id, "1234567890");
        assert_eq!(list[1].push_client_id, "");
    }

    /// A `/device/list` reply with the **shape** of a real one (captured
    /// 2026-08-11) but every identifier and address replaced by a synthetic one.
    /// The two shapes that matter: a phone reports `inet: null` plus a real
    /// `localInet`, a PC fills both; a phone's short id lives under
    /// `bizInfo.conn_base`, a PC's under `bizInfo.office_suite`.
    fn live_device_list() -> Value {
        serde_json::json!({
            "code": 0, "msg": "成功", "ok": true,
            "data": [
                {
                    "deviceId": "0000phone0000", "type": 1, "name": "A Phone", "model": "V0000A",
                    "ext": { "seeds": { "192_0_2_20": "00000000-1111-2222-3333-444444444444" },
                             "mobile_pcsuite_version": 65008 },
                    "inets": [{ "inet": null, "localInet": "192.0.2.20",
                                "netmark": "255.255.255.0", "ssid": "some-wifi" }],
                    "bizInfo": { "conn_base": { "businessId": "aa11bb" } },
                    "reportTime": "2026-08-11 10:59:09.374"
                },
                {
                    "deviceId": "0000pc0000SERIAL", "type": 3, "name": "A MacBook Pro",
                    "model": "MAC00,0",
                    "ext": { "deviceType": "Mac", "businessId": "aabbccddeeff" },
                    "inets": [{ "inet": "192.0.2.10", "localInet": "192.0.2.10" },
                              { "inet": "198.51.100.7", "localInet": "198.51.100.7" }],
                    "bizInfo": { "office_suite": { "businessId": "aabbccddeeff" } },
                    "reportTime": "2026-08-11 10:59:08.569"
                }
            ]
        })
    }

    #[test]
    fn parses_a_real_device_list() {
        let list = parse_device_list(&live_device_list());
        assert_eq!(list.len(), 2);

        let phone = &list[0];
        assert_eq!(phone.name, "A Phone");
        assert!(phone.is_phone());
        assert_eq!(phone.external_id, "aa11bb"); // = the QR-pairing bleId
        assert_eq!(phone.report_time, "2026-08-11 10:59:09.374");

        let pc = &list[1];
        assert!(!pc.is_phone());
        assert_eq!(pc.external_id, "aabbccddeeff");
        assert_eq!(pc.inets.len(), 2);
    }

    #[test]
    fn phone_address_falls_through_null_inet_to_local_inet() {
        // Regression: `inet` is present-but-null on a phone, so a naive
        // `get("inet").or_else(localInet)` yields Some(Null) and drops the only
        // address there is — i.e. every phone becomes unconnectable.
        let list = parse_device_list(&live_device_list());
        assert_eq!(list[0].ip(), Some("192.0.2.20"));
        assert_eq!(list[1].ip(), Some("192.0.2.10"));
    }

    #[test]
    fn seed_keys_are_normalised_back_to_ip_form() {
        let list = parse_device_list(&live_device_list());
        assert_eq!(
            list[0].seeds.get("192.0.2.20").map(String::as_str),
            Some("00000000-1111-2222-3333-444444444444")
        );
        assert!(list[1].seeds.is_empty()); // a PC publishes none
    }

    #[test]
    fn device_list_of_an_unexpected_shape_is_empty_not_an_error() {
        assert!(parse_device_list(&serde_json::json!({ "code": 0 })).is_empty());
        assert!(parse_device_list(&serde_json::json!({ "code": 0, "data": null })).is_empty());
    }
}
