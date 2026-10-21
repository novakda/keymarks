// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backups;
mod desktop;

use std::path::PathBuf;
use std::sync::Mutex;
use store::{Bookmark, CleanupItem, KeywordCount, SearchOptions, Store};
use desktop::{Settings, SettingsState};
use tauri::{AppHandle, Manager, Runtime, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::ShortcutState;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

pub(crate) type Db = Mutex<Store>;

/// Where the database and its backups live.
struct DataDir(PathBuf);

impl DataDir {
    fn backups(&self) -> PathBuf {
        self.0.join("backups")
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Schemes we hand to the OS. Imported files are untrusted: no ms-msdt:, javascript:, etc.
const OPENABLE: [&str; 5] = ["http://", "https://", "ftp://", "file:", "mailto:"];

/// Every matching id in display order; the UI fetches rows lazily with `get_bookmarks`.
#[tauri::command]
fn search_ids(db: State<Db>, query: String, opts: SearchOptions) -> Result<Vec<i64>, String> {
    db.lock().unwrap().search_ids(&query, &opts).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_bookmarks(db: State<Db>, ids: Vec<i64>) -> Result<Vec<Bookmark>, String> {
    db.lock().unwrap().get_many(&ids).map_err(|e| e.to_string())
}

#[tauri::command]
fn count(db: State<Db>) -> Result<i64, String> {
    db.lock().unwrap().count().map_err(|e| e.to_string())
}

/// Pick a pm35.htm and import it. Only into an empty store, so re-importing can't duplicate.
#[tauri::command]
async fn import_pm35(app: AppHandle, db: State<'_, Db>) -> Result<Option<usize>, String> {
    let picked = app
        .dialog()
        .file()
        .add_filter("Powermarks database", &["htm", "html", "ba0"])
        .blocking_pick_file();
    let Some(path) = picked else { return Ok(None) };
    let bytes = std::fs::read(path.into_path().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let parsed = pm35::parse(&bytes)?;
    let mut store = db.lock().unwrap();
    if store.count().map_err(|e| e.to_string())? > 0 {
        return Err("bookmarks already imported".into());
    }
    store.import_pm35(&parsed).map(Some).map_err(|e| e.to_string())
}

/// Open a bookmark in the default browser and count the visit.
#[tauri::command]
fn open_bookmark(app: AppHandle, db: State<Db>, id: i64) -> Result<(), String> {
    open_id(&app, &db.lock().unwrap(), id)
}

pub(crate) fn open_id<R: Runtime>(app: &AppHandle<R>, store: &Store, id: i64) -> Result<(), String> {
    let url = store.get(id).map_err(err)?.ok_or("bookmark not found")?.url;
    let lower = url.to_ascii_lowercase();
    if !OPENABLE.iter().any(|p| lower.starts_with(p)) {
        return Err(format!("not opening {url}: unsupported scheme"));
    }
    app.opener().open_url(&url, None::<&str>).map_err(err)?;
    store.mark_visited(id).map_err(err)
}

#[tauri::command]
fn update_bookmark(db: State<Db>, bookmark: Bookmark) -> Result<bool, String> {
    if bookmark.url.trim().is_empty() {
        return Err("URL can't be empty".into());
    }
    db.lock().unwrap().update(&bookmark).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_bookmarks(db: State<Db>, ids: Vec<i64>) -> Result<usize, String> {
    db.lock().unwrap().delete(&ids).map_err(|e| e.to_string())
}

#[tauri::command]
fn keywords(db: State<Db>, query: String, opts: SearchOptions) -> Result<Vec<KeywordCount>, String> {
    db.lock().unwrap().keywords_with(&query, &opts).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_tagged(db: State<Db>, keyword: String, tagged: bool) -> Result<(), String> {
    db.lock().unwrap().set_tagged(&keyword, tagged).map_err(|e| e.to_string())
}

/// Rename (or merge, or with an empty `new` delete) a keyword on every bookmark.
#[tauri::command]
fn rename_keyword(db: State<Db>, old: String, new: String) -> Result<usize, String> {
    db.lock().unwrap().rename_keyword(&old, &new).map_err(|e| e.to_string())
}

#[tauri::command]
fn cleanup_plan(db: State<Db>) -> Result<Vec<CleanupItem>, String> {
    db.lock().unwrap().cleanup_plan().map_err(|e| e.to_string())
}

/// Back up the database, then apply `(keyword, replacement)` changes. Returns the count
/// and the backup's name.
#[tauri::command]
fn apply_cleanup(dir: State<DataDir>, db: State<Db>, changes: Vec<(String, String)>) -> Result<(usize, String), String> {
    let mut store = db.lock().unwrap();
    let backup = backups::take(&store, &dir.backups(), "before-cleanup")?;
    let n = store.apply_cleanup(&changes).map_err(err)?;
    Ok((n, backup))
}

#[tauri::command]
fn undo(db: State<Db>) -> Result<Option<String>, String> {
    db.lock().unwrap().undo().map_err(err)
}

#[tauri::command]
fn redo(db: State<Db>) -> Result<Option<String>, String> {
    db.lock().unwrap().redo().map_err(err)
}

#[derive(serde::Serialize)]
struct HistoryLabels {
    undo: Option<String>,
    redo: Option<String>,
}

#[tauri::command]
fn history(db: State<Db>) -> HistoryLabels {
    let (undo, redo) = db.lock().unwrap().history_labels();
    HistoryLabels { undo, redo }
}

#[tauri::command]
fn list_backups(dir: State<DataDir>) -> Vec<backups::BackupInfo> {
    backups::list(&dir.backups())
}

#[tauri::command]
fn backup_now(dir: State<DataDir>, db: State<Db>) -> Result<String, String> {
    backups::take(&db.lock().unwrap(), &dir.backups(), "manual")
}

/// Replace the database with a backup, after saving the current state. Returns the name of
/// that safety copy.
#[tauri::command]
fn restore_backup(dir: State<DataDir>, db: State<Db>, name: String) -> Result<String, String> {
    let path = backups::resolve(&dir.backups(), &name)?;
    let mut store = db.lock().unwrap();
    let safety = backups::take(&store, &dir.backups(), "before-restore")?;
    store.restore_from(path.to_str().ok_or("backup path is not UTF-8")?).map_err(err)?;
    Ok(safety)
}

#[derive(serde::Serialize)]
struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    autostart: bool,
}

#[tauri::command]
fn get_settings(app: AppHandle, settings: State<SettingsState>) -> SettingsView {
    SettingsView {
        settings: settings.current.lock().unwrap().clone(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    }
}

/// Apply and persist settings. The hotkey is applied first so a bad one changes nothing.
#[tauri::command]
fn save_settings(app: AppHandle, state: State<SettingsState>, settings: Settings, autostart: bool) -> Result<(), String> {
    let mut current = state.current.lock().unwrap();
    let hotkey = settings.hotkey.trim().to_string();
    if hotkey != current.hotkey {
        desktop::apply_hotkey(&app, &current.hotkey, &hotkey)?;
    }
    let al = app.autolaunch();
    if autostart != al.is_enabled().unwrap_or(false) {
        if autostart { al.enable() } else { al.disable() }.map_err(err)?;
    }
    let new = Settings { hotkey, ..settings };
    state.save(&new)?;
    *current = new;
    Ok(())
}

/// The UI reports the top results after each search; they become the tray menu.
#[tauri::command]
fn set_tray_results(app: AppHandle, items: Vec<(i64, String)>) -> Result<(), String> {
    let menu = desktop::tray_menu(&app, &items).map_err(err)?;
    if let Some(tray) = app.tray_by_id(desktop::TRAY_ID) {
        tray.set_menu(Some(menu)).map_err(err)?;
    }
    Ok(())
}

/// Polled by the UI; changes when the browser extension's host adds a bookmark.
#[tauri::command]
fn data_version(db: State<Db>) -> Result<i64, String> {
    db.lock().unwrap().data_version().map_err(|e| e.to_string())
}

/// The app was called Powermarks 2026 (com.xhiris.powermarks) before the rename. Move that
/// data folder's contents (database, settings, backups) over once, if there is no new data yet.
fn move_legacy_data(new: &std::path::Path) {
    let Some(old) = new.parent().map(|p| p.join("com.xhiris.powermarks")) else { return };
    if new.join("bookmarks.db").exists() || !old.join("bookmarks.db").exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(new) {
        eprintln!("data migration: {e}");
        return;
    }
    for entry in std::fs::read_dir(&old).into_iter().flatten().flatten() {
        // Same volume, so a rename moves each file (or the backups folder) in one step.
        if let Err(e) = std::fs::rename(entry.path(), new.join(entry.file_name())) {
            eprintln!("data migration: {}: {e}", entry.path().display());
        }
    }
    let _ = std::fs::remove_dir(&old); // only if now empty
}

fn main() {
    tauri::Builder::default()
        // First: a second launch hands over to the running app and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| desktop::show_main(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        desktop::show_main(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let settings = window.state::<SettingsState>();
                if settings.current.lock().unwrap().close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            // KEYMARKS_DATA_DIR lets tests run against a copy instead of the real database.
            let dir = match std::env::var_os("KEYMARKS_DATA_DIR") {
                Some(d) => PathBuf::from(d),
                None => {
                    let dir = app.path().app_data_dir()?;
                    move_legacy_data(&dir);
                    dir
                }
            };
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("bookmarks.db");
            let store = Store::open(path.to_str().ok_or("data dir path is not UTF-8")?)?;
            app.manage(Mutex::new(store));
            app.manage(DataDir(dir.clone()));

            let settings = SettingsState::load(dir.join("settings.json"));
            let current = settings.current.lock().unwrap().clone();
            app.manage(settings);
            if let Err(e) = desktop::apply_hotkey(app.handle(), "", &current.hotkey) {
                eprintln!("{e}");
            }
            desktop::build_tray(app.handle())?;
            // Started with Windows (autostart passes --minimized): stay in the tray if asked.
            let autostarted = std::env::args().any(|a| a == "--minimized");
            if !(autostarted && current.start_minimized) {
                desktop::show_main(app.handle());
            }
            // Startup backups off the UI thread; failures are logged, never fatal.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let db = handle.state::<Db>();
                let store = db.lock().unwrap();
                if let Err(e) = backups::on_startup(&store, &dir.join("backups")) {
                    eprintln!("startup backup failed: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_ids,
            get_bookmarks,
            count,
            import_pm35,
            open_bookmark,
            update_bookmark,
            delete_bookmarks,
            data_version,
            keywords,
            set_tagged,
            rename_keyword,
            cleanup_plan,
            apply_cleanup,
            undo,
            redo,
            history,
            list_backups,
            backup_now,
            restore_backup,
            get_settings,
            save_settings,
            set_tray_results
        ])
        .run(tauri::generate_context!())
        .expect("error while running Keymarks");
}
