// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backups;

use std::path::PathBuf;
use std::sync::Mutex;
use store::{Bookmark, CleanupItem, KeywordCount, SearchOptions, Store};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Db = Mutex<Store>;

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
    let store = db.lock().unwrap();
    let url = store.get(id).map_err(|e| e.to_string())?.ok_or("bookmark not found")?.url;
    let lower = url.to_ascii_lowercase();
    if !OPENABLE.iter().any(|p| lower.starts_with(p)) {
        return Err(format!("not opening {url}: unsupported scheme"));
    }
    app.opener().open_url(&url, None::<&str>).map_err(|e| e.to_string())?;
    store.mark_visited(id).map_err(|e| e.to_string())
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

/// Polled by the UI; changes when the browser extension's host adds a bookmark.
#[tauri::command]
fn data_version(db: State<Db>) -> Result<i64, String> {
    db.lock().unwrap().data_version().map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // POWERMARKS_DATA_DIR lets tests run against a copy instead of the real database.
            let dir = match std::env::var_os("POWERMARKS_DATA_DIR") {
                Some(d) => PathBuf::from(d),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("bookmarks.db");
            let store = Store::open(path.to_str().ok_or("data dir path is not UTF-8")?)?;
            app.manage(Mutex::new(store));
            app.manage(DataDir(dir.clone()));
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
            restore_backup
        ])
        .run(tauri::generate_context!())
        .expect("error while running Powermarks");
}
