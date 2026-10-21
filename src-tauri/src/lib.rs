mod archive;
mod audio;
mod commands;
mod error;
mod media;
mod media_server;
mod platform;
mod recent;
mod secrets;
mod shards;
mod tasks;
mod thumbs;
mod video;
mod windows;
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
    let builder = tauri::Builder::default();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(platform::init())
        .manage(AppState {
            workspace: RwLock::new(None),
            drivers: Arc::new(DriverRegistry::with_builtin()),
            backfill_running: AtomicBool::new(false),
        })
        .register_asynchronous_uri_scheme_protocol(thumbs::SCHEME, thumbs::handle)
        .register_asynchronous_uri_scheme_protocol(media::SCHEME, media::handle)
        .setup(|app| {
            use tauri::Manager;
            if let Ok(dir) = app.path().app_data_dir() {
                secrets::init(dir);
            }
            tasks::init(app.handle());
            // Android plays media over loopback HTTP (see media_server.rs).
            #[cfg(target_os = "android")]
            media_server::start(app.handle())?;
            create_main_window(app.handle())?;
            audio::init(app.handle());
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            windows::setup_desktop(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::create_bundle,
            commands::open_bundle,
            commands::close_bundle,
            commands::current_bundle,
            commands::recent_bundles,
            commands::forget_recent_bundle,
            commands::last_view,
            commands::save_last_view,
            commands::get_settings,
            commands::save_settings,
            commands::list_sources,
            commands::add_local_source,
            commands::remove_source,
            commands::scan_source,
            commands::cancel_scan,
            commands::search,
            commands::offline_sources,
            commands::check_media,
            commands::media_brief,
            commands::check_query,
            commands::save_bundle,
            commands::prefetch,
            commands::record_view,
            commands::cache_stats,
            commands::get_app_settings,
            commands::save_app_settings,
            commands::list_fields,
            commands::create_field,
            commands::rename_field,
            commands::delete_field,
            commands::media_detail,
            commands::set_values,
            commands::add_value,
            commands::remove_value,
            commands::suggest_values,
            commands::query_vocabulary,
            commands::list_saved_queries,
            commands::save_query,
            commands::delete_saved_query,
            commands::ffmpeg_status,
            commands::convert_media,
            commands::cancel_convert,
            commands::incompatible_videos,
            commands::set_ignore_warning,
            commands::probe_media,
            commands::loop_segments,
            commands::save_loop_segment,
            commands::delete_loop_segment,
            commands::driver_kinds,
            commands::test_source,
            commands::add_source,
            commands::update_source,
            commands::source_config,
            commands::unlock_bundle,
            commands::set_lock,
            commands::remove_lock,
            commands::child_folders,
            commands::list_tasks,
            commands::cancel_task,
            commands::export_media,
            commands::delete_media,
            commands::rename_media,
            commands::move_media_to,
            shards::shard_status,
            shards::inspect_shard,
            shards::create_shard,
            shards::attach_shard,
            shards::unlock_shard,
            shards::lock_shard,
            shards::shard_change_password,
            shards::shard_import,
            shards::shard_from_source,
            shards::shard_export,
            shards::shard_verify,
            shards::shard_compact,
            shards::shard_set_encryption,
            shards::shard_relocate,
            audio::get_audio,
            audio::set_audio,
            audio::set_audio_focus,
            windows::open_window,
            windows::take_window_payload,
            windows::panic,
            windows::restore_windows,
            platform::platform_info,
            platform::request_storage_access,
            platform::set_system_fullscreen,
            platform::set_system_bars,
            platform::storage_roots,
            platform::system_pick,
            platform::browse_dir,
            media_base_url,
        ])
        .on_window_event(|window, event| {
            // Mobile: the OS may end a backgrounded app without notice; write what a locked
            // bundle holds in memory while that is still possible.
            #[cfg(mobile)]
            if matches!(event, tauri::WindowEvent::Suspended) {
                use tauri::Manager;
                if let Some(ws) = window.app_handle().state::<AppState>().workspace.read().unwrap().as_ref() {
                    ws.flush();
                }
            }
            // Closing the main window ends the app, extra windows included.
            if window.label() == windows::MAIN && matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                use tauri::Manager;
                windows::close_secondary(window.app_handle());
                window.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // macOS: clicking the Dock icon brings back windows hidden by the panic key.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                windows::restore(app);
            }
            if let tauri::RunEvent::Exit = event {
                // Write what a locked bundle still holds in memory before the process ends.
                use tauri::Manager;
                if let Some(ws) = app.state::<AppState>().workspace.read().unwrap().as_ref() {
                    ws.closed.store(true, std::sync::atomic::Ordering::Relaxed);
                    ws.flush();
                }
            }
        });
}

/// Base URL for original media when the platform needs the loopback server (Android),
/// otherwise `None` and the UI uses `vpmedia://`.
#[tauri::command]
fn media_base_url() -> Option<String> {
    media_server::base()
}

/// Chromium switches derived from the device's hardware settings. The first switch is
/// what Tauri passes by default on Windows; overriding the arguments must keep it.
pub(crate) fn browser_args(hw: &vp_core::settings::HardwareSettings) -> String {
    let mut args = vec!["--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string()];
    if !hw.gpu {
        args.push("--disable-gpu".into());
    } else {
        if hw.gpu_rasterization {
            args.push("--enable-gpu-rasterization".into());
        }
        if hw.zero_copy {
            args.push("--enable-zero-copy".into());
        }
    }
    if !hw.video_decode {
        args.push("--disable-accelerated-video-decode".into());
    }
    args.join(" ")
}

/// The main window is created here rather than in `tauri.conf.json` so that hardware
/// settings can be applied to the WebView before it starts.
fn create_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    windows::build(app, windows::MAIN, "ViewPort", 1280.0, 800.0)?;
    Ok(())
}
