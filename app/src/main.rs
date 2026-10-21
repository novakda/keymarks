// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use store::{Bookmark, Store};
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

#[tauri::command]
fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !OPENABLE.iter().any(|p| lower.starts_with(p)) {
        return Err(format!("not opening {url}: unsupported scheme"));
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
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
        .invoke_handler(tauri::generate_handler![search, count, import_pm35, open_url])
        .run(tauri::generate_context!())
        .expect("error while running Powermarks");
}
