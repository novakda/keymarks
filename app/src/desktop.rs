//! Desktop presence: tray icon and menu, global hotkey, close-to-tray, start with Windows.
//! Settings live in `<data dir>/settings.json` because they apply before the UI loads.

use crate::{Db, open_id};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

pub const TRAY_ID: &str = "main";
const TRAY_RESULTS: usize = 15;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// When started with Windows, stay in the tray.
    pub start_minimized: bool,
    /// Global shortcut that brings the window up ("" = none), e.g. "Ctrl+Alt+P".
    pub hotkey: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { close_to_tray: false, start_minimized: true, hotkey: "Ctrl+Alt+P".into() }
    }
}

pub struct SettingsState {
    pub path: PathBuf,
    pub current: Mutex<Settings>,
}

impl SettingsState {
    pub fn load(path: PathBuf) -> Self {
        let current = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        SettingsState { path, current: Mutex::new(current) }
    }

    pub fn save(&self, s: &Settings) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }
}

/// Show, un-minimise and focus the main window, and put the cursor in the search box.
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        let _ = w.emit("focus-search", ());
    }
}

/// Replace the registered global shortcut. On failure the old one stays registered.
pub fn apply_hotkey<R: Runtime>(app: &AppHandle<R>, old: &str, new: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    if !old.is_empty() {
        let _ = gs.unregister(old);
    }
    if new.is_empty() {
        return Ok(());
    }
    gs.register(new).map_err(|e| {
        if !old.is_empty() {
            let _ = gs.register(old);
        }
        format!("can't use {new} as a hotkey: {e}")
    })
}

pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Powermarks")
        .menu(&tray_menu(app, &[])?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            id => {
                if let Some(Ok(bookmark)) = id.strip_prefix("open:").map(str::parse::<i64>) {
                    let db = app.state::<Db>();
                    let store = db.lock().unwrap();
                    if let Err(e) = open_id(app, &store, bookmark) {
                        eprintln!("tray open failed: {e}");
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Tray menu: Show, the top results of the current search (like the original), Quit.
pub fn tray_menu<R: Runtime>(app: &AppHandle<R>, results: &[(i64, String)]) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "show", "Show Powermarks", true, None::<&str>)?)?;
    if !results.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        for (id, title) in results.iter().take(TRAY_RESULTS) {
            let label: String = if title.chars().count() > 60 {
                title.chars().take(59).chain(['…']).collect()
            } else {
                title.clone()
            };
            menu.append(&MenuItem::with_id(app, format!("open:{id}"), label, true, None::<&str>)?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}
