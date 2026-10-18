//! Master audio shared by every window: one volume, one mute, and which window has
//! audio focus (with `audio.focus_only`, only that window's active pane plays sound).
//! Changes are broadcast as `audio` events and remembered in the device settings.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "audio";

#[derive(Debug, Clone, Serialize)]
pub struct AudioState {
    pub volume: f32,
    pub muted: bool,
    /// Label of the window that has audio focus.
    pub focus: Option<String>,
}

static STATE: Mutex<Option<AudioState>> = Mutex::new(None);

/// Loads the starting state: the last volume, muted if `start_muted` is on (whatever the
/// state was at exit) or if it was muted when the app last closed.
pub fn init(app: &AppHandle) {
    let a = crate::recent::app_settings(app).audio;
    *STATE.lock().unwrap() = Some(AudioState {
        volume: a.volume.clamp(0.0, 1.0),
        muted: a.start_muted || a.muted,
        focus: Some("main".into()),
    });
}

pub fn current() -> AudioState {
    STATE.lock().unwrap().clone().unwrap_or(AudioState { volume: 1.0, muted: true, focus: None })
}

fn update(app: &AppHandle, f: impl FnOnce(&mut AudioState)) -> AudioState {
    let state = {
        let mut guard = STATE.lock().unwrap();
        let s = guard.get_or_insert_with(current);
        f(s);
        s.clone()
    };
    let _ = app.emit(EVENT, state.clone());
    state
}

/// Remembers volume and mute for the next run.
fn persist(app: &AppHandle, s: &AudioState) {
    let mut settings = crate::recent::app_settings(app);
    if settings.audio.volume != s.volume || settings.audio.muted != s.muted {
        settings.audio.volume = s.volume;
        settings.audio.muted = s.muted;
        let _ = crate::recent::save_app_settings(app, &settings);
    }
}

pub fn set_muted(app: &AppHandle, muted: bool) {
    let s = update(app, |s| s.muted = muted);
    persist(app, &s);
}

#[tauri::command]
pub fn get_audio() -> AudioState {
    current()
}

#[tauri::command]
pub fn set_audio(app: AppHandle, volume: Option<f32>, muted: Option<bool>) -> AudioState {
    let s = update(&app, |s| {
        if let Some(v) = volume {
            s.volume = v.clamp(0.0, 1.0);
        }
        if let Some(m) = muted {
            s.muted = m;
        }
    });
    persist(&app, &s);
    s
}

#[tauri::command]
pub fn set_audio_focus(app: AppHandle, label: String) {
    if current().focus.as_deref() != Some(label.as_str()) {
        update(&app, |s| s.focus = Some(label));
    }
}
