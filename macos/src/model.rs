//! Saved connections and settings, persisted as JSON in
//! `~/Library/Application Support/RdpLauncher/`.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_RDP_PORT: u16 = 3389;

const CONNECTIONS_FILE: &str = "connections.json";
const SETTINGS_FILE: &str = "settings.json";

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    #[default]
    Fullscreen,
    Windowed,
}

/// One saved RDP connection. Passwords are never stored here (see `keychain`).
///
/// Unknown fields — such as the Windows app's `password_enc` — are ignored, so a
/// Windows `connections.json` loads as-is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    #[serde(default = "new_id")]
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display: DisplayMode,
    /// Stable display keys (see `displays`); empty = main display.
    #[serde(default)]
    pub monitors: Vec<String>,
    #[serde(default)]
    pub shared_folder: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub sound: bool,
    #[serde(default)]
    pub cert_fingerprint: Option<String>,
}

impl Connection {
    pub fn new(name: &str, host: &str) -> Self {
        Connection {
            id: new_id(),
            name: name.to_owned(),
            host: host.to_owned(),
            username: String::new(),
            display: DisplayMode::default(),
            monitors: Vec::new(),
            shared_folder: None,
            sound: true,
            cert_fingerprint: None,
        }
    }

    /// A copy with a fresh id, inserted below the original as "<name> (copy)".
    pub fn duplicate(&self) -> Self {
        Connection {
            id: new_id(),
            name: format!("{} (copy)", self.name),
            ..self.clone()
        }
    }

    pub fn endpoint(&self) -> Result<(String, u16), HostError> {
        parse_host(&self.host)
    }

    pub fn credentials(&self) -> (String, Option<String>) {
        split_username(&self.username)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    Empty,
    InvalidPort,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HostError::Empty => "Enter a host name or IP address.",
            HostError::InvalidPort => "The port must be a number from 1 to 65535.",
        })
    }
}

impl std::error::Error for HostError {}

/// `host`, `host:port`, `[ipv6]:port` or a bare IPv6 address.
pub fn parse_host(input: &str) -> Result<(String, u16), HostError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(HostError::Empty);
    }
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (addr, after) = rest.split_once(']').ok_or(HostError::Empty)?;
        match after.strip_prefix(':') {
            Some(port) => (addr, Some(port)),
            None if after.is_empty() => (addr, None),
            None => return Err(HostError::InvalidPort),
        }
    } else if s.matches(':').count() == 1 {
        let (host, port) = s.split_once(':').expect("one colon");
        (host, Some(port))
    } else {
        (s, None)
    };
    if host.trim().is_empty() {
        return Err(HostError::Empty);
    }
    let port = match port {
        None => DEFAULT_RDP_PORT,
        Some(p) => match p.trim().parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => return Err(HostError::InvalidPort),
        },
    };
    Ok((host.trim().to_owned(), port))
}

/// `DOMAIN\user` → (`user`, `DOMAIN`); `user@domain` and `user` stay whole.
pub fn split_username(input: &str) -> (String, Option<String>) {
    let s = input.trim();
    match s.split_once('\\') {
        Some((domain, user)) if !domain.is_empty() => (user.to_owned(), Some(domain.to_owned())),
        _ => (s.to_owned(), None),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub always_on_top: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { always_on_top: true }
    }
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store { dir }
    }

    pub fn default_dir() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        home.join("Library/Application Support/RdpLauncher")
    }

    /// Missing file → empty list. Unparseable file → copied to `connections.json.bak`
    /// (and left untouched) before returning an empty list, so nothing is lost.
    pub fn load_connections(&self) -> Vec<Connection> {
        let path = self.dir.join(CONNECTIONS_FILE);
        let Ok(text) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        let raw: Vec<serde_json::Value> = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return self.back_up_corrupt(&path, &e.to_string()),
        };
        let missing_ids = raw.iter().any(|v| v.get("id").is_none());
        let conns: Vec<Connection> = match raw.into_iter().map(serde_json::from_value).collect() {
            Ok(c) => c,
            Err(e) => return self.back_up_corrupt(&path, &e.to_string()),
        };
        if missing_ids {
            if let Err(e) = self.save_connections(&conns) {
                tracing::warn!(error = %e, "could not persist generated connection ids");
            }
        }
        conns
    }

    fn back_up_corrupt(&self, path: &Path, error: &str) -> Vec<Connection> {
        let backup = self.dir.join(format!("{CONNECTIONS_FILE}.bak"));
        tracing::warn!(%error, backup = %backup.display(), "connections.json is unreadable; backed it up");
        if let Err(e) = fs::copy(path, &backup) {
            tracing::error!(error = %e, "could not back up unreadable connections.json");
        }
        Vec::new()
    }

    pub fn save_connections(&self, conns: &[Connection]) -> std::io::Result<()> {
        self.write_json(CONNECTIONS_FILE, &conns)
    }

    pub fn load_settings(&self) -> Settings {
        fs::read_to_string(self.dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> {
        self.write_json(SETTINGS_FILE, s)
    }

    /// Write to a temp file then rename, so a crash never leaves a half-written file.
    fn write_json<T: Serialize + ?Sized>(&self, file: &str, value: &T) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
        let tmp = self.dir.join(format!("{file}.tmp"));
        fs::write(&tmp, json)?;
        fs::rename(tmp, self.dir.join(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn parse_host_plain_ip() {
        assert_eq!(parse_host(" 192.168.0.161 ").unwrap(), ("192.168.0.161".into(), 3389));
    }

    #[test]
    fn parse_host_with_port() {
        assert_eq!(parse_host("server:3390").unwrap(), ("server".into(), 3390));
    }

    #[test]
    fn parse_host_bracketed_ipv6() {
        assert_eq!(parse_host("[::1]:3390").unwrap(), ("::1".into(), 3390));
    }

    #[test]
    fn parse_host_bare_ipv6() {
        assert_eq!(parse_host("fe80::1").unwrap(), ("fe80::1".into(), 3389));
    }

    #[test]
    fn parse_host_rejects_bad_port() {
        assert_eq!(parse_host("host:abc"), Err(HostError::InvalidPort));
        assert_eq!(parse_host("host:0"), Err(HostError::InvalidPort));
    }

    #[test]
    fn parse_host_rejects_empty() {
        assert_eq!(parse_host("   "), Err(HostError::Empty));
    }

    #[test]
    fn split_username_forms() {
        assert_eq!(split_username("pi"), ("pi".into(), None));
        assert_eq!(split_username("CORP\\kevin"), ("kevin".into(), Some("CORP".into())));
        assert_eq!(split_username("kevin@corp.local"), ("kevin@corp.local".into(), None));
    }

    #[test]
    fn windows_format_loads_with_defaults() {
        // exact shape written by the Windows app
        let json = r#"[{"name":"Pi5-16GB","host":"192.168.0.161","username":"pi","password_enc":"01ab"}]"#;
        let c: Vec<Connection> = serde_json::from_str(json).unwrap();
        assert_eq!(c[0].display, DisplayMode::Fullscreen);
        assert!(c[0].sound && c[0].monitors.is_empty() && c[0].shared_folder.is_none());
        assert_eq!(c[0].id.len(), 36);
    }

    #[test]
    fn duplicate_gets_new_id_and_copy_suffix() {
        let a = Connection::new("Media Server", "192.168.0.201");
        let b = a.duplicate();
        assert_ne!(a.id, b.id);
        assert_eq!(b.name, "Media Server (copy)");
        assert_eq!(b.host, a.host);
    }

    #[test]
    fn save_then_load_round_trips_and_preserves_order() {
        let (_dir, store) = store();
        let a = Connection::new("A", "10.0.0.1");
        let mut b = Connection::new("B", "10.0.0.2:3390");
        b.display = DisplayMode::Windowed;
        b.monitors = vec!["dell-1".into(), "dell-2".into()];
        b.shared_folder = Some("/Users/me/Downloads".into());
        b.sound = false;
        b.cert_fingerprint = Some("sha256:ab".into());
        let c = Connection::new("C", "10.0.0.3");
        store.save_connections(&[a.clone(), b.clone(), c.clone()]).unwrap();
        assert_eq!(store.load_connections(), vec![a, b, c]);
    }

    #[test]
    fn missing_ids_are_generated_and_persisted() {
        let (dir, store) = store();
        fs::write(
            dir.path().join("connections.json"),
            r#"[{"name":"Pi4-4GB","host":"192.168.0.146"}]"#,
        )
        .unwrap();
        let first = store.load_connections();
        let second = store.load_connections();
        assert_eq!(first[0].id, second[0].id);
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let (dir, store) = store();
        let path = dir.path().join("connections.json");
        fs::write(&path, "{not json").unwrap();
        assert!(store.load_connections().is_empty());
        assert_eq!(fs::read_to_string(dir.path().join("connections.json.bak")).unwrap(), "{not json");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn settings_default_always_on_top() {
        let (_dir, store) = store();
        assert_eq!(store.load_settings(), Settings { always_on_top: true });
    }

    #[test]
    fn settings_round_trip() {
        let (_dir, store) = store();
        store.save_settings(&Settings { always_on_top: false }).unwrap();
        assert_eq!(store.load_settings(), Settings { always_on_top: false });
    }
}
