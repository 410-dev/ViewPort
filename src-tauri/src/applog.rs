//! The app's log file (`viewport.log` in the config folder): what the user can copy or
//! open from the settings to report a problem. Lines are `time level area: message`.
//! It rolls over to `viewport.old.log` past `log.max_bytes`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::error::{CmdError, CmdResult};

static PATH: OnceLock<PathBuf> = OnceLock::new();
static WRITE: Mutex<()> = Mutex::new(());

pub fn init(app: &AppHandle) {
    if let Ok(dir) = app.path().app_config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = PATH.set(dir.join("viewport.log"));
    }
    info(
        "app",
        format!("ViewPort {} started ({} {})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH),
    );
}

fn write(level: &str, area: &str, message: &str) {
    let Some(path) = PATH.get() else { return };
    let _one = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let max = crate::recent::cached_app_settings().log.max_bytes.max(64 * 1024);
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let _ = std::fs::rename(path, path.with_file_name("viewport.old.log"));
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    let _ = writeln!(f, "{} {level} {area}: {}", timestamp(), message.replace('\n', " | "));
}

pub fn info(area: &str, message: impl AsRef<str>) {
    write("INFO", area, message.as_ref());
}

pub fn warn(area: &str, message: impl AsRef<str>) {
    write("WARN", area, message.as_ref());
}

/// UTC time as `YYYY-MM-DD HH:MM:SS.mmmZ`.
fn timestamp() -> String {
    let ms = crate::workspace::now_ms();
    let (secs, millis) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{millis:03}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

#[derive(serde::Serialize)]
pub struct LogInfo {
    pub path: String,
    /// The end of the log (the previous log first when the current one is short).
    pub text: String,
    pub bytes: u64,
}

/// The last `max_bytes` of the log, for copying.
#[tauri::command]
pub async fn log_read(max_bytes: Option<u64>) -> CmdResult<LogInfo> {
    let path = PATH.get().cloned().ok_or_else(|| CmdError::new("not_found", "no log"))?;
    let max = max_bytes.unwrap_or(1024 * 1024) as usize;
    tauri::async_runtime::spawn_blocking(move || {
        let old = std::fs::read(path.with_file_name("viewport.old.log")).unwrap_or_default();
        let cur = std::fs::read(&path).unwrap_or_default();
        let bytes = cur.len() as u64;
        let mut all = old;
        all.extend_from_slice(&cur);
        let tail = &all[all.len().saturating_sub(max)..];
        // Start at a whole line.
        let tail = match tail.iter().position(|&b| b == b'\n') {
            Some(i) if all.len() > max => &tail[i + 1..],
            _ => tail,
        };
        Ok(LogInfo { path: path.display().to_string(), text: String::from_utf8_lossy(tail).into_owned(), bytes })
    })
    .await
    .map_err(|e| CmdError::new("internal", e))?
}

/// Opens the log file (`folder`: shows it in its folder instead).
#[tauri::command]
pub fn log_open(app: AppHandle, folder: bool) -> CmdResult<()> {
    let path = PATH.get().cloned().ok_or_else(|| CmdError::new("not_found", "no log"))?;
    if !path.exists() {
        info("app", "log opened");
    }
    let opener = app.opener();
    let r = if folder { opener.reveal_item_in_dir(&path) } else { opener.open_path(path.display().to_string(), None::<&str>) };
    r.map_err(|e| CmdError::new("io", e))
}

#[tauri::command]
pub fn log_clear() -> CmdResult<()> {
    let path = PATH.get().cloned().ok_or_else(|| CmdError::new("not_found", "no log"))?;
    let _one = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file(path.with_file_name("viewport.old.log"));
    std::fs::write(&path, b"")?;
    Ok(())
}
