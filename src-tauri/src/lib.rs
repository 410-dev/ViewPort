mod commands;
mod error;
mod media;
mod recent;
mod thumbs;
mod workspace;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use vp_driver::DriverRegistry;
use workspace::Workspace;

pub struct AppState {
    pub workspace: RwLock<Option<Arc<Workspace>>>,
    pub drivers: Arc<DriverRegistry>,
    pub backfill_running: AtomicBool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            workspace: RwLock::new(None),
            drivers: Arc::new(DriverRegistry::with_builtin()),
            backfill_running: AtomicBool::new(false),
        })
        .register_asynchronous_uri_scheme_protocol(thumbs::SCHEME, thumbs::handle)
        .register_asynchronous_uri_scheme_protocol(media::SCHEME, media::handle)
        .setup(|app| {
            create_main_window(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::create_bundle,
            commands::open_bundle,
            commands::close_bundle,
            commands::current_bundle,
            commands::recent_bundles,
            commands::get_settings,
            commands::save_settings,
            commands::list_sources,
            commands::add_local_source,
            commands::remove_source,
            commands::scan_source,
            commands::cancel_scan,
            commands::search,
            commands::media_brief,
            commands::check_query,
            commands::save_bundle,
            commands::prefetch,
            commands::record_view,
            commands::cache_stats,
            commands::get_app_settings,
            commands::save_app_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Chromium switches derived from the device's hardware settings. The first switch is
/// what Tauri passes by default on Windows; overriding the arguments must keep it.
fn browser_args(hw: &vp_core::settings::HardwareSettings) -> String {
    let mut args = vec!["--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string()];
    if !hw.gpu {
        args.push("--disable-gpu".into());
    }
    if !hw.video_decode {
        args.push("--disable-accelerated-video-decode".into());
    }
    args.join(" ")
}

/// The main window is created here rather than in `tauri.conf.json` so that hardware
/// settings can be applied to the WebView before it starts.
fn create_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let hw = recent::app_settings(app).hardware;
    let builder = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
        .title("ViewPort")
        .inner_size(1280.0, 800.0)
        .min_inner_size(480.0, 360.0);
    #[cfg(windows)]
    let builder = builder.additional_browser_args(&browser_args(&hw));
    #[cfg(not(windows))]
    let _ = browser_args(&hw);
    builder.build()?;
    Ok(())
}
