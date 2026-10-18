use serde::Serialize;
use vp_query::{FieldRegistry, QueryError, ast::Span};

/// Result of checking query text in the editor. Spans are UTF-16 offsets for JavaScript.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum QueryCheck {
    Ok { normalized: String },
    Error { error: QueryError, span: Span },
}

#[tauri::command]
fn check_query(src: &str, locale: &str) -> QueryCheck {
    // TODO: use the open workspace's registry once bundles are wired in.
    let reg = FieldRegistry::with_base_fields();
    match vp_query::parse_query(src, &reg) {
        Ok(q) => QueryCheck::Ok { normalized: vp_query::render(&q, &reg, locale) },
        Err(e) => {
            let span = e.span.to_utf16(src);
            QueryCheck::Error { error: e, span }
        }
    }
}

#[tauri::command]
fn default_settings() -> vp_core::settings::Settings {
    vp_core::settings::Settings::default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![check_query, default_settings])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
