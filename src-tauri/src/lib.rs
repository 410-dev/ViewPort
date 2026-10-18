mod commands;
mod error;
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
