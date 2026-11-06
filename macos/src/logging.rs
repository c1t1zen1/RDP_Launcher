//! Log to `~/Library/Logs/RDP Launcher/rdp-launcher.log` (passwords are never logged).
//! Set `RDPL_LOG=debug` for more detail.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::Level;

pub fn log_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Logs/RDP Launcher/rdp-launcher.log")
}

pub fn init() {
    let level = match std::env::var("RDPL_LOG").as_deref() {
        Ok("trace") => Level::TRACE,
        Ok("debug") => Level::DEBUG,
        _ => Level::INFO,
    };
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let builder = tracing_subscriber::fmt().with_max_level(level).with_ansi(false);
    let result = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => builder.with_writer(Mutex::new(file)).try_init(),
        Err(_) => builder.with_writer(std::io::stderr).try_init(),
    };
    if result.is_ok() {
        tracing::info!(version = env!("CARGO_PKG_VERSION"), "RDP Launcher starting");
    }
}
