// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backups;
mod checker;
mod desktop;

use std::path::PathBuf;
use std::sync::Mutex;
use store::{Bookmark, CleanupItem, ImportReport, KeywordCount, SavedItem, SearchOptions, Store, browsers};
use desktop::{Settings, SettingsState};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WindowEvent};
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
        return Err("a Powermarks database can only be imported into an empty collection for now".into());
    }
    store.import_pm35(&parsed).map(Some).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct SourceView {
    browser: &'static str,
    profile: String,
    /// Bookmarks found, or None if the file couldn't be read.
    count: Option<usize>,
}

/// Browsers and profiles found on this machine, with how many bookmarks each has.
#[tauri::command]
async fn browser_sources() -> Vec<SourceView> {
    browsers::detect()
        .into_iter()
        .map(|s| SourceView { count: browsers::read(&s).ok().map(|b| b.len()), browser: s.browser, profile: s.profile })
        .collect()
}

/// Merge-import one detected browser profile (looked up again here: the UI never passes paths).
#[tauri::command]
async fn import_browser(db: State<'_, Db>, browser: String, profile: String) -> Result<ImportReport, String> {
    let source = browsers::detect()
        .into_iter()
        .find(|s| s.browser == browser && s.profile == profile)
        .ok_or_else(|| format!("{browser} profile {profile} not found"))?;
    let items = browsers::read(&source)?;
    db.lock().unwrap().import_bookmarks(&format!("Import from {browser} ({profile})"), &items).map_err(err)
}

/// Trim, add "https://" when there is no scheme, and check it parses.
fn normalize_url(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Enter a URL".into());
    }
    let s = if s.contains("://") || s.starts_with("mailto:") || s.starts_with("file:") { s.to_string() } else { format!("https://{s}") };
    reqwest::Url::parse(&s).map(|u| u.to_string()).map_err(|e| format!("Not a valid URL: {e}"))
}

/// Add one bookmark by URL (Ctrl+N). Without a title, the page's own is fetched.
#[tauri::command]
fn add_bookmark(app: AppHandle, db: State<Db>, url: String, title: String) -> Result<ImportReport, String> {
    let url = normalize_url(&url)?;
    let title = if title.trim().is_empty() { url.clone() } else { title.trim().to_string() };
    let item = store::NewBookmark { url, title, keywords: String::new(), description: String::new(), added: None };
    let report = db.lock().unwrap().import_bookmarks("Add bookmark", &[item]).map_err(err)?;
    fetch_in_background(app, report.ids.clone());
    Ok(report)
}

/// Add every link on the clipboard (Powermarks' Alt+Ins), tagged with the fast-add keywords;
/// titles and descriptions are fetched in the background.
#[tauri::command]
fn add_from_clipboard(app: AppHandle, db: State<Db>, settings: State<SettingsState>) -> Result<ImportReport, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let text = app.clipboard().read_text().unwrap_or_default();
    let urls = checker::extract_urls(&text);
    if urls.is_empty() {
        return Err("No web links found on the clipboard".into());
    }
    let keywords = settings.current.lock().unwrap().fast_add_keywords.clone();
    let items: Vec<_> = urls
        .into_iter()
        .map(|url| store::NewBookmark { title: url.clone(), url, keywords: keywords.clone(), description: String::new(), added: None })
        .collect();
    let step = format!("Add {} link{} from the clipboard", items.len(), if items.len() == 1 { "" } else { "s" });
    let report = db.lock().unwrap().import_bookmarks(&step, &items).map_err(err)?;
    fetch_in_background(app, report.ids.clone());
    Ok(report)
}

/// Fill titles/descriptions of just-added bookmarks from their pages, then tell the UI.
fn fetch_in_background(app: AppHandle, ids: Vec<i64>) {
    use futures::StreamExt;
    if ids.is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let Ok(client) = checker::fetch_client() else { return };
        let urls: Vec<(i64, String)> = {
            let db = app.state::<Db>();
            let store = db.lock().unwrap();
            ids.iter().filter_map(|&id| store.get(id).ok().flatten().map(|b| (id, b.url))).collect()
        };
        let client = &client;
        let fetched: Vec<_> = futures::stream::iter(urls)
            .map(|(id, url)| async move { (id, checker::fetch_meta(client, &url).await) })
            .buffer_unordered(4)
            .collect()
            .await;
        {
            let db = app.state::<Db>();
            let store = db.lock().unwrap();
            for (id, meta) in fetched {
                if let Some(meta) = meta {
                    let _ = store.apply_fetched(id, &meta);
                }
            }
        }
        let _ = app.emit("bookmarks-changed", ());
    });
}

/// Pick an exported bookmarks HTML file (any browser) and merge it in.
#[tauri::command]
async fn import_html_file(app: AppHandle, db: State<'_, Db>) -> Result<Option<ImportReport>, String> {
    let picked = app.dialog().file().add_filter("Bookmarks HTML", &["html", "htm"]).blocking_pick_file();
    let Some(path) = picked else { return Ok(None) };
    let path = path.into_path().map_err(err)?;
    let bytes = std::fs::read(&path).map_err(err)?;
    let items = browsers::read_netscape_html(&String::from_utf8_lossy(&bytes));
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    db.lock().unwrap().import_bookmarks(&format!("Import from {label}"), &items).map(Some).map_err(err)
}

/// Export bookmarks (`ids` in that order, or all) as "html" (any browser) or "pm35"
/// (Powermarks 3.5) to a file the user picks. Returns (count, file name), or None if cancelled.
#[tauri::command]
async fn export_file(app: AppHandle, db: State<'_, Db>, format: String, ids: Option<Vec<i64>>) -> Result<Option<(usize, String)>, String> {
    let (default, filter, ext) = match format.as_str() {
        "html" => ("keymarks-bookmarks.html", "Bookmarks HTML", "html"),
        "pm35" => ("pm35.htm", "Powermarks 3.5 database", "htm"),
        other => return Err(format!("unknown export format {other}")),
    };
    let rows = db.lock().unwrap().export_rows(ids.as_deref()).map_err(err)?;
    let picked = app.dialog().file().set_file_name(default).add_filter(filter, &[ext]).blocking_save_file();
    let Some(path) = picked else { return Ok(None) };
    let path = path.into_path().map_err(err)?;
    let bytes = match format.as_str() {
        "html" => store::export::to_netscape(&rows).into_bytes(),
        _ => store::export::to_pm35(&rows),
    };
    std::fs::write(&path, bytes).map_err(err)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Some((rows.len(), name)))
}

/// Open a bookmark in the default browser and count the visit.
#[tauri::command]
fn open_bookmark(app: AppHandle, db: State<Db>, id: i64) -> Result<(), String> {
    open_id(&app, &db.lock().unwrap(), id)
}

pub(crate) fn open_id<R: Runtime>(app: &AppHandle<R>, store: &Store, id: i64) -> Result<(), String> {
    let url = store.get(id).map_err(err)?.ok_or("bookmark not found")?.url;
    open_checked(app, &url)?;
    store.mark_visited(id).map_err(err)
}

/// Hand a URL to the OS, but only for schemes on the allowlist.
fn open_checked<R: Runtime>(app: &AppHandle<R>, url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !OPENABLE.iter().any(|p| lower.starts_with(p)) {
        return Err(format!("not opening {url}: unsupported scheme"));
    }
    app.opener().open_url(url, None::<&str>).map_err(err)
}

#[tauri::command]
fn saved_items(db: State<Db>) -> Result<Vec<SavedItem>, String> {
    db.lock().unwrap().saved_items().map_err(err)
}

/// Add (id 0) or update a saved search or link. Links get the same URL clean-up as bookmarks.
#[tauri::command]
fn save_item(db: State<Db>, mut item: SavedItem) -> Result<i64, String> {
    if item.name.trim().is_empty() {
        return Err("Give it a name".into());
    }
    match item.kind.as_str() {
        "link" => item.url = normalize_url(&item.url)?,
        "search" => {}
        other => return Err(format!("unknown kind {other}")),
    }
    let store = db.lock().unwrap();
    if item.id == 0 {
        store.save_item(&item).map_err(err)
    } else {
        store.update_saved_item(&item).map_err(err)?;
        Ok(item.id)
    }
}

#[tauri::command]
fn delete_saved_item(db: State<Db>, id: i64) -> Result<(), String> {
    db.lock().unwrap().delete_saved_item(id).map_err(err)
}

#[tauri::command]
fn move_saved_item(db: State<Db>, id: i64, by: i64) -> Result<(), String> {
    db.lock().unwrap().move_saved_item(id, by).map_err(err)
}

/// Open a saved link.
#[tauri::command]
fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    open_checked(&app, &url)
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

/// Start checking the links of `ids` (all with checking on, if none given) in the background;
/// progress arrives as "check-progress" events. Returns how many will be checked (enabled ones only).
#[tauri::command]
fn start_check(
    app: AppHandle,
    db: State<Db>,
    checks: State<checker::CheckState>,
    settings: State<SettingsState>,
    ids: Option<Vec<i64>>,
) -> Result<usize, String> {
    use std::sync::atomic::Ordering;
    if checks.running.swap(true, Ordering::SeqCst) {
        return Err("a link check is already running".into());
    }
    checks.cancel.store(false, Ordering::SeqCst);
    let targets = match db.lock().unwrap().check_targets(ids.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            checks.running.store(false, Ordering::SeqCst);
            return Err(err(e));
        }
    };
    let n = targets.len();
    let repair = settings.current.lock().unwrap().repair_moved_urls;
    tauri::async_runtime::spawn(checker::run_batch(app, targets, repair));
    Ok(n)
}

#[tauri::command]
fn cancel_check(checks: State<checker::CheckState>) {
    checks.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tauri::command]
fn change_rating(db: State<Db>, ids: Vec<i64>, by: i64) -> Result<usize, String> {
    db.lock().unwrap().change_rating(&ids, by).map_err(err)
}

#[tauri::command]
fn set_check_enabled(db: State<Db>, ids: Vec<i64>, enabled: bool) -> Result<usize, String> {
    db.lock().unwrap().set_check_enabled(&ids, enabled).map_err(err)
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

/// Installed builds ship keymarks-host.exe beside the app: (re)register it for Chrome, Edge
/// and Firefox on each start, so the extension finds this install (cheap and idempotent).
/// Development builds skip this; register target\debug\keymarks-host.exe by hand.
#[cfg(not(debug_assertions))]
fn register_bundled_host(app: &AppHandle) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(host) = app.path().resource_dir().map(|d| d.join("keymarks-host.exe")) else { return };
    if !host.is_file() {
        return;
    }
    std::thread::spawn(move || {
        match std::process::Command::new(&host).arg("--install").creation_flags(CREATE_NO_WINDOW).status() {
            Ok(s) if s.success() => {}
            other => eprintln!("could not register the browser host: {other:?}"),
        }
    });
}

fn main() {
    tauri::Builder::default()
        // First: a second launch hands over to the running app and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| desktop::show_main(app)))
        // Size and position only: restoring "maximised" would show a window meant to start in
        // the tray. The test window is left alone, so a test never overwrites the user's.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(tauri_plugin_window_state::StateFlags::SIZE | tauri_plugin_window_state::StateFlags::POSITION)
                .with_denylist(if desktop::test_mode() { &["main"] } else { &[] })
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
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
            app.manage(checker::CheckState::default());

            let settings = SettingsState::load(dir.join("settings.json"));
            let current = settings.current.lock().unwrap().clone();
            app.manage(settings);
            if let Err(e) = desktop::apply_hotkey(app.handle(), "", &current.hotkey) {
                eprintln!("{e}");
            }
            desktop::build_tray(app.handle())?;
            #[cfg(not(debug_assertions))]
            register_bundled_host(app.handle());
            // Started with Windows (autostart passes --minimized): stay in the tray if asked.
            let autostarted = std::env::args().any(|a| a == "--minimized");
            // Created here (not from config) so test mode can make it impossible to activate.
            // Normally it starts hidden and show_main shows and focuses it.
            let test = desktop::test_mode();
            tauri::WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
                .focused(!test)
                .focusable(!test)
                .visible(test)
                .build()?;
            if !(autostarted && current.start_minimized) && !desktop::background_start() {
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
            set_tray_results,
            start_check,
            cancel_check,
            set_check_enabled,
            change_rating,
            browser_sources,
            import_browser,
            import_html_file,
            export_file,
            add_bookmark,
            add_from_clipboard,
            saved_items,
            save_item,
            delete_saved_item,
            move_saved_item,
            open_link
        ])
        .run(tauri::generate_context!())
        .expect("error while running Keymarks");
}

#[cfg(test)]
mod tests {
    use super::normalize_url;

    #[test]
    fn urls_are_trimmed_completed_and_checked() {
        assert_eq!(normalize_url("  example.com/a ").unwrap(), "https://example.com/a");
        assert_eq!(normalize_url("http://x.example").unwrap(), "http://x.example/");
        assert_eq!(normalize_url("mailto:me@example.com").unwrap(), "mailto:me@example.com");
        assert!(normalize_url("   ").is_err());
        assert!(normalize_url("http://exa mple.com").is_err());
    }
}
