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
    /// Link checks update URLs of pages that moved permanently (undoable).
    pub repair_moved_urls: bool,
    /// Keywords given to bookmarks added without a dialog (fast add, clipboard), so they can
    /// be reviewed later. The browser host reads this too.
    pub fast_add_keywords: String,
    /// Link-check schedules, one per line: "Name = interval" (see `parse_schedules`).
    pub schedules: String,
    /// Check links on their schedules in the background. Off until the user turns it on:
    /// imported Powermarks schedules would otherwise start thousands of checks at once.
    pub scheduled_checks: bool,
    /// Bookmarks always open in a new browser window (as Shift+Enter does).
    pub open_new_window: bool,
    /// Hide Keymarks to the tray after opening a bookmark.
    pub hide_after_open: bool,
    /// Minimising hides the window from the taskbar (it stays in the tray).
    pub hide_when_minimized: bool,
    /// View and search preferences (view, columns, widths, panes, search options), kept by
    /// the page in localStorage and copied here so they travel with this file.
    pub ui: std::collections::BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            close_to_tray: false,
            start_minimized: true,
            hotkey: "Ctrl+Alt+P".into(),
            repair_moved_urls: true,
            fast_add_keywords: "new".into(),
            schedules: "Daily = 1d\nWeekly = 1w\nMonthly = 1mo".into(),
            scheduled_checks: false,
            open_new_window: false,
            hide_after_open: false,
            hide_when_minimized: false,
            ui: Default::default(),
        }
    }
}

/// "Name = interval" lines into (name, seconds). Intervals as in Powermarks: a number and a
/// unit, min (minutes), h, d, w, mo (months of 30 days) or y; "1.5d" works too.
pub fn parse_schedules(text: &str) -> Result<Vec<(String, i64)>, String> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (name, every) = line.split_once('=').ok_or_else(|| format!("\"{line}\": write it as Name = interval, e.g. Weekly = 1w"))?;
        let (name, every) = (name.trim(), every.trim().to_lowercase());
        let split = every.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(every.len());
        let n: f64 = every[..split].parse().map_err(|_| format!("\"{line}\": the interval needs a number, e.g. 1w"))?;
        let unit = match every[split..].trim() {
            "min" | "mi" | "m" => 60,
            "h" => 3600,
            "d" => 86400,
            "w" => 7 * 86400,
            "mo" => 30 * 86400,
            "y" => 365 * 86400,
            u => return Err(format!("\"{line}\": unknown unit \"{u}\" (use min, h, d, w, mo or y)")),
        };
        let seconds = (n * unit as f64) as i64;
        if name.is_empty() || seconds < 3600 {
            return Err(format!("\"{line}\": needs a name and at least an hour"));
        }
        if out.iter().any(|(o, _)| o.eq_ignore_ascii_case(name)) {
            return Err(format!("\"{name}\" is listed twice"));
        }
        out.push((name.to_string(), seconds));
    }
    Ok(out)
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

/// Test mode (KEYMARKS_NO_FOCUS, set only by the e2e and screenshot scripts): the window is
/// shown but can never be activated (WS_EX_NOACTIVATE), so automation can't take focus from
/// whatever the user is doing. Tests drive the page over CDP and need no OS focus.
pub fn test_mode() -> bool {
    std::env::var_os("KEYMARKS_NO_FOCUS").is_some()
}

/// `--background` (or the older `--no-focus`): start hidden in the tray and show nothing until
/// the user opens it (tray, hotkey). Used when automation puts the user's own copy back.
pub fn background_start() -> bool {
    std::env::args().any(|a| a == "--background" || a == "--no-focus")
}

/// Show, un-minimise and focus the main window, and put the cursor in the search box.
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if test_mode() {
        return;
    }
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
        .tooltip("Keymarks")
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
    menu.append(&MenuItem::with_id(app, "show", "Show Keymarks", true, None::<&str>)?)?;
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

#[cfg(test)]
mod tests {
    use super::parse_schedules;

    #[test]
    fn schedules_parse_like_powermarks() {
        assert_eq!(
            parse_schedules("Daily = 1d\n\n Weekly=1w \nTwice a day = 12h\nQuarterly = 3mo\nSlow = 1.5d").unwrap(),
            [("Daily".into(), 86400), ("Weekly".into(), 604800), ("Twice a day".into(), 43200), ("Quarterly".into(), 7776000), ("Slow".into(), 129600)]
        );
        assert!(parse_schedules("Daily").is_err());
        assert!(parse_schedules("Fast = 5min").is_err(), "at least an hour");
        assert!(parse_schedules("A = 1d\na = 2d").is_err(), "duplicate names");
        assert!(parse_schedules("A = 1 fortnight").is_err());
    }
}
