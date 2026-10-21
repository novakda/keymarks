// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use store::{Bookmark, CleanupItem, KeywordCount, Store};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Db = Mutex<Store>;

/// Schemes we hand to the OS. Imported files are untrusted: no ms-msdt:, javascript:, etc.
const OPENABLE: [&str; 5] = ["http://", "https://", "ftp://", "file:", "mailto:"];

#[tauri::command]
fn search(db: State<Db>, query: String) -> Result<Vec<Bookmark>, String> {
    db.lock().unwrap().search(&query, 500).map_err(|e| e.to_string())
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
fn keywords(db: State<Db>, query: String) -> Result<Vec<KeywordCount>, String> {
    db.lock().unwrap().keywords(&query).map_err(|e| e.to_string())
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
/// and the backup's path.
#[tauri::command]
fn apply_cleanup(app: AppHandle, db: State<Db>, changes: Vec<(String, String)>) -> Result<(usize, String), String> {
    let err = |e: &dyn std::fmt::Display| e.to_string();
    let dir = app.path().app_data_dir().map_err(|e| err(&e))?.join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| err(&e))?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let backup = dir.join(format!("before-cleanup-{secs}.db"));
    let mut store = db.lock().unwrap();
    store.backup_to(backup.to_str().ok_or("backup path is not UTF-8")?).map_err(|e| err(&e))?;
    let n = store.apply_cleanup(&changes).map_err(|e| err(&e))?;
    Ok((n, backup.display().to_string()))
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
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("bookmarks.db");
            let store = Store::open(path.to_str().ok_or("data dir path is not UTF-8")?)?;
            app.manage(Mutex::new(store));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search,
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
            apply_cleanup
        ])
        .run(tauri::generate_context!())
        .expect("error while running Powermarks");
}
