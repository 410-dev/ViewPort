//! Extra windows on the same workspace (viewer and explorer windows), the panic key
//! (mute + hide everything) and the ways back: a tray icon and a global shortcut.
//!
//! Every window loads the same SPA; a new window fetches what it should show with
//! `take_window_payload`, so large id lists never go through the URL.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{AppHandle, Manager, WebviewWindow};

use crate::error::{CmdError, CmdResult};

pub const MAIN: &str = "main";

static NEXT: AtomicU64 = AtomicU64::new(1);
static PAYLOADS: Mutex<Option<HashMap<String, serde_json::Value>>> = Mutex::new(None);
/// Windows hidden by the panic key, restored together.
static HIDDEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Creates a window with the device's hardware settings applied to its WebView.
pub fn build(app: &AppHandle, label: &str, title: &str, width: f64, height: f64) -> tauri::Result<WebviewWindow> {
    let hw = crate::recent::app_settings(app).hardware;
    let builder = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .title(title)
        .inner_size(width, height)
        .min_inner_size(480.0, 360.0);
    // WebView2 shares one browser process per app; every window must use the same arguments.
    #[cfg(windows)]
    let builder = builder.additional_browser_args(&crate::browser_args(&hw));
    #[cfg(not(windows))]
    let _ = crate::browser_args(&hw);
    builder.build()
}

/// Opens a `viewer` or `library` window showing `payload`. Returns its label.
#[tauri::command]
pub fn open_window(app: AppHandle, kind: String, payload: serde_json::Value) -> CmdResult<String> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return Err(CmdError::new("unsupported", "multiple windows are not available on mobile; use split view"));
    }
    let kind = match kind.as_str() {
        "viewer" | "library" => kind,
        _ => return Err(CmdError::new("invalid", "unknown window kind")),
    };
    let label = format!("{kind}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    PAYLOADS.lock().unwrap().get_or_insert_with(HashMap::new).insert(label.clone(), payload);
    let w = crate::recent::app_settings(&app).windows;
    build(&app, &label, "ViewPort", w.new_width.max(480) as f64, w.new_height.max(360) as f64)
        .map_err(|e| CmdError::new("window", e))?;
    Ok(label)
}

/// What the calling window was opened to show (once).
#[tauri::command]
pub fn take_window_payload(window: tauri::Window) -> Option<serde_json::Value> {
    PAYLOADS.lock().unwrap().as_mut().and_then(|m| m.remove(window.label()))
}

/// Closes every window except the main one (when the bundle closes or changes).
pub fn close_secondary(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label != MAIN {
            let _ = w.close();
        }
    }
}

/// The panic key: mute, then hide every window.
#[tauri::command]
pub fn panic(app: AppHandle) {
    let p = crate::recent::app_settings(&app).privacy;
    if p.panic_mute {
        crate::audio::set_muted(&app, true);
    }
    if !p.panic_hide || cfg!(any(target_os = "android", target_os = "ios")) {
        return;
    }
    let mut hidden = HIDDEN.lock().unwrap();
    for (label, w) in app.webview_windows() {
        if w.is_visible().unwrap_or(false) && w.hide().is_ok() && !hidden.contains(&label) {
            hidden.push(label);
        }
    }
}

/// Brings back the windows the panic key hid (or the main window).
pub fn restore(app: &AppHandle) {
    let labels = std::mem::take(&mut *HIDDEN.lock().unwrap());
    let labels = if labels.is_empty() { vec![MAIN.to_string()] } else { labels };
    let mut last = None;
    for label in labels {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.show();
            #[cfg(desktop)]
            let _ = w.unminimize();
            last = Some(w);
        }
    }
    if let Some(w) = app.get_webview_window(MAIN).or(last) {
        let _ = w.set_focus();
    }
}

#[tauri::command]
pub fn restore_windows(app: AppHandle) {
    restore(&app);
}

/// Tray icon and global restore shortcut (desktop only).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn setup_desktop(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let settings = crate::recent::app_settings(app);
    if settings.privacy.tray_icon {
        let ko = settings.ui.locale == "ko"
            || (settings.ui.locale == "auto" && std::env::var("LANG").map(|l| l.starts_with("ko")).unwrap_or(false));
        let show = MenuItem::with_id(app, "show", if ko { "창 복원" } else { "Show windows" }, true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", if ko { "종료" } else { "Quit" }, true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&show, &quit])?;
        let mut tray = TrayIconBuilder::with_id("main").menu(&menu).show_menu_on_left_click(false).tooltip("ViewPort");
        if let Some(icon) = app.default_window_icon() {
            tray = tray.icon(icon.clone());
        }
        tray.on_menu_event(|app, event| match event.id.as_ref() {
            "show" => restore(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                restore(tray.app_handle());
            }
        })
        .build(app)?;
    }
    register_shortcut(app, &settings.privacy.restore_shortcut);
    Ok(())
}

/// (Re)registers the global restore shortcut. An invalid one is ignored.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn register_shortcut(app: &AppHandle, accelerator: &str) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if accelerator.trim().is_empty() {
        return;
    }
    let _ = gs.on_shortcut(accelerator.trim(), |app, _shortcut, event| {
        if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
            restore(app);
        }
    });
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn register_shortcut(_app: &AppHandle, _accelerator: &str) {}
