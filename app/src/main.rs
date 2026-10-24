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
async fn import_pm35(app: AppHandle, db: State<'_, Db>, settings: State<'_, SettingsState>) -> Result<Option<usize>, String> {
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
    let n = store.import_pm35(&parsed).map_err(|e| e.to_string())?;
    // The file's check schedules join Settings, so its bookmarks' schedule names mean something.
    let mut current = settings.current.lock().unwrap();
    let known = desktop::parse_schedules(&current.schedules).unwrap_or_default();
    let mut added = String::new();
    for r in pm35::records(&parsed.head, "scs") {
        let (Some(name), Some(secs)) = (r.first(), r.get(2).and_then(|h| u64::from_str_radix(h, 16).ok())) else { continue };
        if secs >= 3600 && !known.iter().any(|(k, _)| k.eq_ignore_ascii_case(name)) {
            let every = if secs % 86400 == 0 { format!("{}d", secs / 86400) } else { format!("{}h", secs / 3600) };
            added += &format!("\n{} = {every}", store::decode_text(name).trim());
        }
    }
    if !added.is_empty() {
        let mut new = current.clone();
        new.schedules = format!("{}{added}", new.schedules.trim_end());
        if desktop::parse_schedules(&new.schedules).is_ok() {
            settings.save(&new)?;
            *current = new;
        }
    }
    Ok(Some(n))
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
async fn import_browser(db: State<'_, Db>, browser: String, profile: String, title_words: bool) -> Result<ImportReport, String> {
    let source = browsers::detect()
        .into_iter()
        .find(|s| s.browser == browser && s.profile == profile)
        .ok_or_else(|| format!("{browser} profile {profile} not found"))?;
    let items = with_title_words(browsers::read(&source)?, title_words);
    db.lock().unwrap().import_bookmarks(&format!("Import from {browser} ({profile})"), &items).map_err(err)
}

/// The import option "keywords from titles": each title's words join its folder keywords.
fn with_title_words(mut items: Vec<store::NewBookmark>, on: bool) -> Vec<store::NewBookmark> {
    if on {
        for b in &mut items {
            b.keywords = format!("{} {}", b.keywords, store::title_keywords(&b.title));
        }
    }
    items
}

/// Remove keywords from the selection (one undo step).
#[tauri::command]
fn remove_keywords(db: State<Db>, ids: Vec<i64>, words: String) -> Result<usize, String> {
    db.lock().unwrap().remove_keywords(&ids, &words).map_err(err)
}

/// Powermarks' Mark Visited: count a visit without opening the pages.
#[tauri::command]
fn mark_visited(db: State<Db>, ids: Vec<i64>) -> Result<usize, String> {
    let store = db.lock().unwrap();
    ids.iter().try_for_each(|&id| store.mark_visited(id)).map_err(err)?;
    Ok(ids.len())
}

/// Powermarks' Add Common Keywords, on the selection.
#[tauri::command]
fn add_keywords(db: State<Db>, ids: Vec<i64>, words: String) -> Result<usize, String> {
    db.lock().unwrap().add_keywords(&ids, &words).map_err(err)
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
fn add_from_clipboard(app: AppHandle) -> Result<ImportReport, String> {
    clipboard_add(&app)
}

/// Also run by the clipboard hotkey, from anywhere.
pub fn clipboard_add(app: &AppHandle) -> Result<ImportReport, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let text = app.clipboard().read_text().unwrap_or_default();
    let urls = checker::extract_urls(&text);
    if urls.is_empty() {
        return Err("No web links found on the clipboard".into());
    }
    let keywords = app.state::<SettingsState>().current.lock().unwrap().fast_add_keywords.clone();
    let items: Vec<_> = urls
        .into_iter()
        .map(|url| store::NewBookmark { title: url.clone(), url, keywords: keywords.clone(), description: String::new(), added: None })
        .collect();
    let step = format!("Add {} link{} from the clipboard", items.len(), if items.len() == 1 { "" } else { "s" });
    let report = app.state::<Db>().lock().unwrap().import_bookmarks(&step, &items).map_err(err)?;
    fetch_in_background(app.clone(), report.ids.clone());
    Ok(report)
}

/// Copy (and Cut, which then deletes): the URLs as text, one per line, and as HTML links so
/// a paste into a document or mail keeps the titles.
#[tauri::command]
fn copy_bookmarks(app: AppHandle, db: State<Db>, ids: Vec<i64>) -> Result<usize, String> {
    use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let rows = db.lock().unwrap().get_many(&ids).map_err(err)?;
    let plain = rows.iter().map(|b| b.url.as_str()).collect::<Vec<_>>().join("\r\n");
    let html: String = rows
        .iter()
        .map(|b| format!("<a href=\"{}\">{}</a><br>\r\n", attr(&b.url), text(if b.title.is_empty() { &b.url } else { &b.title })))
        .collect();
    app.clipboard().write_html(html, Some(plain)).map_err(err)?;
    Ok(rows.len())
}

/// Files and folders dropped from Explorer become file:/// bookmarks, opened with their
/// associated app. Titled by file name and given the fast-add keywords, like a clipboard add.
fn add_files(app: &AppHandle, paths: &[PathBuf]) -> Result<ImportReport, String> {
    let keywords = app.state::<SettingsState>().current.lock().unwrap().fast_add_keywords.clone();
    let items = file_bookmarks(paths, &keywords);
    let step = format!("Add {} file{}", items.len(), if items.len() == 1 { "" } else { "s" });
    app.state::<Db>().lock().unwrap().import_bookmarks(&step, &items).map_err(err)
}

fn file_bookmarks(paths: &[PathBuf], keywords: &str) -> Vec<store::NewBookmark> {
    paths
        .iter()
        .filter_map(|p| {
            let url = reqwest::Url::from_file_path(p).ok()?.to_string();
            let title = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
            Some(store::NewBookmark { url, title, keywords: keywords.into(), description: String::new(), added: None })
        })
        .collect()
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

/// Pick a file and merge it in: bookmarks HTML (any browser), any web page (its links), or a
/// Keymarks JSON export (every field kept).
#[tauri::command]
async fn import_html_file(app: AppHandle, db: State<'_, Db>, title_words: bool) -> Result<Option<ImportReport>, String> {
    let picked = app.dialog().file().add_filter("Bookmarks, web pages or Keymarks JSON", &["html", "htm", "json"]).blocking_pick_file();
    let Some(path) = picked else { return Ok(None) };
    let path = path.into_path().map_err(err)?;
    let text = String::from_utf8_lossy(&std::fs::read(&path).map_err(err)?).into_owned();
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let step = format!("Import from {label}");
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        let rows = store::export::read_json(&text)?;
        return db.lock().unwrap().import_rows(&step, &rows).map(Some).map_err(err);
    }
    let items = with_title_words(browsers::read_netscape_html(&text), title_words);
    db.lock().unwrap().import_bookmarks(&step, &items).map(Some).map_err(err)
}

/// Export bookmarks (`ids` in that order, or all) to a file the user picks: "html" (any
/// browser), "pm35" (Powermarks 3.5), "csv" or "print" (the chosen `fields`; `title` heads
/// the printable page) or "json" (everything). Returns (count, file name), or None if cancelled.
#[tauri::command]
async fn export_file(
    app: AppHandle,
    db: State<'_, Db>,
    format: String,
    ids: Option<Vec<i64>>,
    fields: Vec<String>,
    title: String,
) -> Result<Option<(usize, String)>, String> {
    let (default, filter, ext) = match format.as_str() {
        "html" => ("keymarks-bookmarks.html", "Bookmarks HTML", "html"),
        "pm35" => ("pm35.htm", "Powermarks 3.5 database", "htm"),
        "csv" => ("keymarks-bookmarks.csv", "CSV (spreadsheets)", "csv"),
        "print" => ("keymarks-list.html", "Web page", "html"),
        "json" => ("keymarks-bookmarks.json", "Keymarks JSON", "json"),
        other => return Err(format!("unknown export format {other}")),
    };
    let rows = db.lock().unwrap().export_rows(ids.as_deref()).map_err(err)?;
    let picked = app.dialog().file().set_file_name(default).add_filter(filter, &[ext]).blocking_save_file();
    let Some(path) = picked else { return Ok(None) };
    let path = path.into_path().map_err(err)?;
    let bytes = match format.as_str() {
        "html" => store::export::to_netscape(&rows).into_bytes(),
        "csv" => store::export::to_csv(&rows, &fields).into_bytes(),
        "print" => store::export::to_printable(&rows, &fields, &title).into_bytes(),
        "json" => store::export::to_json(&rows).into_bytes(),
        _ => db.lock().unwrap().export_pm35(ids.as_deref()).map_err(err)?,
    };
    std::fs::write(&path, bytes).map_err(err)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Some((rows.len(), name)))
}

/// Open a bookmark in the default browser and count the visit.
#[tauri::command]
fn open_bookmark(app: AppHandle, db: State<Db>, id: i64) -> Result<(), String> {
    open_id(&app, &db.lock().unwrap(), id)?;
    hide_after_open(&app);
    Ok(())
}

/// Opens as the settings say (a new window or not).
pub(crate) fn open_id<R: Runtime>(app: &AppHandle<R>, store: &Store, id: i64) -> Result<(), String> {
    let new_window = app.state::<SettingsState>().current.lock().unwrap().open_new_window;
    open_in(app, store, id, new_window)
}

/// Opens a bookmark, in a new window of the default browser when asked and the browser is
/// one known to take `--new-window` (others open normally), and counts the visit.
fn open_in<R: Runtime>(app: &AppHandle<R>, store: &Store, id: i64, new_window: bool) -> Result<(), String> {
    let url = store.get(id).map_err(err)?.ok_or("bookmark not found")?.url;
    let web = ["http://", "https://"].iter().any(|p| url.to_ascii_lowercase().starts_with(p));
    match default_browser().filter(|_| new_window && web) {
        Some(exe) => drop(std::process::Command::new(exe).arg("--new-window").arg(&url).spawn().map_err(err)?),
        None => open_checked(app, &url)?,
    }
    store.mark_visited(id).map_err(err)
}

/// Powermarks' "minimise after opening a URL": out of the way once the page is up.
fn hide_after_open(app: &AppHandle) {
    if app.state::<SettingsState>().current.lock().unwrap().hide_after_open {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
    }
}

/// Started by the browser extension's "Open Keymarks", this process may bring a window to
/// the front, but it hands over to the instance already running: let that one do it.
fn allow_foreground_handover() {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn AllowSetForegroundWindow(process_id: u32) -> i32;
        }
        const ASFW_ANY: u32 = u32::MAX;
        // SAFETY: no pointers; fails harmlessly when this process has no foreground rights.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    }
}

/// Powermarks' Shift+Enter: open in a new window of the default browser, counting the visit.
#[tauri::command]
fn open_bookmark_new_window(app: AppHandle, db: State<Db>, id: i64) -> Result<(), String> {
    open_in(&app, &db.lock().unwrap(), id, true)?;
    hide_after_open(&app);
    Ok(())
}

#[cfg(windows)]
fn default_browser() -> Option<String> {
    let reg = |args: &[&str]| {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("reg").arg("query").args(args).creation_flags(CREATE_NO_WINDOW).output().ok()?;
        reg_sz(&String::from_utf8_lossy(&out.stdout))
    };
    let prog_id = reg(&[r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice", "/v", "ProgId"])?;
    browser_exe(&reg(&[&format!(r"HKCR\{prog_id}\shell\open\command"), "/ve"])?)
}

#[cfg(not(windows))]
fn default_browser() -> Option<String> {
    None
}

/// The value of the first REG_SZ line in `reg query` output.
fn reg_sz(out: &str) -> Option<String> {
    out.lines().find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string())).filter(|v| !v.is_empty())
}

/// The executable of a browser's open command, if it's one that understands `--new-window`.
fn browser_exe(command: &str) -> Option<String> {
    let command = command.trim();
    let exe = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split(' ').next()?,
    };
    let name = exe.rsplit(['\\', '/']).next()?.to_ascii_lowercase();
    ["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "vivaldi.exe", "opera.exe"].contains(&name.as_str()).then(|| exe.to_string())
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

/// Apply and persist settings. The hotkeys are applied first so a bad one changes nothing.
#[tauri::command]
fn save_settings(app: AppHandle, state: State<SettingsState>, settings: Settings, autostart: bool) -> Result<(), String> {
    let mut current = state.current.lock().unwrap();
    let hotkey = settings.hotkey.trim().to_string();
    let clipboard_hotkey = settings.clipboard_hotkey.trim().to_string();
    if !hotkey.is_empty() && hotkey.eq_ignore_ascii_case(&clipboard_hotkey) {
        return Err("The two hotkeys must differ".into());
    }
    if hotkey != current.hotkey {
        desktop::apply_hotkey(&app, &current.hotkey, &hotkey)?;
    }
    if clipboard_hotkey != current.clipboard_hotkey {
        desktop::apply_hotkey(&app, &current.clipboard_hotkey, &clipboard_hotkey)?;
    }
    let al = app.autolaunch();
    if autostart != al.is_enabled().unwrap_or(false) {
        if autostart { al.enable() } else { al.disable() }.map_err(err)?;
    }
    desktop::parse_schedules(&settings.schedules)?;
    let new = Settings { hotkey, clipboard_hotkey, ui: current.ui.clone(), ..settings };
    state.save(&new)?;
    *current = new;
    Ok(())
}

/// The page's view and search preferences, saved whenever one changes.
#[tauri::command]
fn save_ui_prefs(state: State<SettingsState>, prefs: std::collections::BTreeMap<String, String>) -> Result<(), String> {
    let mut current = state.current.lock().unwrap();
    if current.ui != prefs {
        let new = Settings { ui: prefs, ..current.clone() };
        state.save(&new)?;
        *current = new;
    }
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
/// progress arrives as "check-progress" events. Returns the ones that will be checked (those
/// with checking on).
#[tauri::command]
fn start_check(
    app: AppHandle,
    db: State<Db>,
    checks: State<checker::CheckState>,
    settings: State<SettingsState>,
    ids: Option<Vec<i64>>,
) -> Result<Vec<i64>, String> {
    let repair = settings.current.lock().unwrap().repair_moved_urls;
    begin_check(&app, &checks, repair, false, || db.lock().unwrap().check_targets(ids.as_deref()))
}

/// Start a batch unless one is running; `targets` is only read once that's settled.
fn begin_check(
    app: &AppHandle,
    checks: &checker::CheckState,
    repair: bool,
    scheduled: bool,
    targets: impl FnOnce() -> store::Result<Vec<store::CheckTarget>>,
) -> Result<Vec<i64>, String> {
    use std::sync::atomic::Ordering;
    if !checker::online() {
        return Err("you seem to be offline, so no links were checked".into());
    }
    if checks.running.swap(true, Ordering::SeqCst) {
        return Err("a link check is already running".into());
    }
    checks.cancel.store(false, Ordering::SeqCst);
    let targets = match targets() {
        Ok(t) => t,
        Err(e) => {
            checks.running.store(false, Ordering::SeqCst);
            return Err(err(e));
        }
    };
    let queued = targets.iter().map(|t| t.id).collect();
    tauri::async_runtime::spawn(checker::run_batch(app.clone(), targets, repair, scheduled));
    Ok(queued)
}

/// Scheduled checks, when turned on: every 10 minutes, the 200 most overdue links (so a
/// backlog spreads out instead of hitting the network all at once). Never in test mode.
fn run_scheduler(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(120)); // let startup settle
        loop {
            let settings = app.state::<SettingsState>().current.lock().unwrap().clone();
            if settings.scheduled_checks {
                let schedules = desktop::parse_schedules(&settings.schedules).unwrap_or_default();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
                let due = app.state::<Db>().lock().unwrap().due_for_check(&schedules, now, 200).unwrap_or_default();
                // Nothing due, or a check is already running: try again next round.
                if !due.is_empty() {
                    let _ = begin_check(&app, &app.state::<checker::CheckState>(), settings.repair_moved_urls, true, || Ok(due));
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
    });
}

/// Put the selected bookmarks on a link-check schedule ("" = only when asked).
#[tauri::command]
fn set_schedule(db: State<Db>, ids: Vec<i64>, schedule: String) -> Result<usize, String> {
    let schedule = schedule.trim();
    db.lock().unwrap().set_schedule(&ids, (!schedule.is_empty()).then_some(schedule)).map_err(err)
}

#[tauri::command]
fn schedule_names(settings: State<SettingsState>) -> Vec<String> {
    desktop::parse_schedules(&settings.current.lock().unwrap().schedules).unwrap_or_default().into_iter().map(|(n, _)| n).collect()
}

#[tauri::command]
fn pause_check(checks: State<checker::CheckState>, paused: bool) {
    checks.paused.store(paused, std::sync::atomic::Ordering::SeqCst);
}

/// Powermarks' Fetch (F7): reload title, description and keywords from the pages, as one
/// undoable step. Returns (bookmarks changed, keywords added).
#[tauri::command]
async fn fetch_now(app: AppHandle, ids: Vec<i64>) -> Result<(usize, usize), String> {
    use futures::StreamExt;
    let client = checker::fetch_client().map_err(err)?;
    let urls: Vec<(i64, String)> = {
        let db = app.state::<Db>();
        let store = db.lock().unwrap();
        ids.iter().filter_map(|&id| store.get(id).ok().flatten().map(|b| (id, b.url))).collect()
    };
    let client = &client;
    let fetched: Vec<_> = futures::stream::iter(urls)
        .map(|(id, url)| async move { checker::fetch_meta(client, &url).await.map(|m| (id, m)) })
        .buffer_unordered(4)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    let db = app.state::<Db>();
    let out = db.lock().unwrap().refetch(&fetched).map_err(err);
    out
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
    allow_foreground_handover();
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
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let clip = app.state::<SettingsState>().current.lock().unwrap().clipboard_hotkey.clone();
                    if clip.parse::<tauri_plugin_global_shortcut::Shortcut>().is_ok_and(|c| &c == shortcut) {
                        // Nothing is shown: the status bar says what happened next time it's open.
                        let _ = match clipboard_add(app) {
                            Ok(r) => app.emit("clipboard-added", r),
                            Err(e) => app.emit("clipboard-added-error", e),
                        };
                    } else {
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
            if let WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                let app = window.app_handle();
                let _ = match add_files(app, paths) {
                    Ok(r) => app.emit("files-added", r),
                    Err(e) => app.emit("files-added-error", e),
                };
                return;
            }
            let settings = window.state::<SettingsState>();
            let settings = settings.current.lock().unwrap();
            match event {
                WindowEvent::CloseRequested { api, .. } if settings.close_to_tray => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // Minimised to the tray only: off the taskbar until shown again.
                WindowEvent::Resized(_) if settings.hide_when_minimized && window.is_minimized().unwrap_or(false) => {
                    let _ = window.hide();
                }
                _ => {}
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
            for key in [&current.hotkey, &current.clipboard_hotkey] {
                if let Err(e) = desktop::apply_hotkey(app.handle(), "", key) {
                    eprintln!("{e}");
                }
            }
            desktop::build_tray(app.handle())?;
            if !desktop::test_mode() {
                run_scheduler(app.handle().clone());
            }
            #[cfg(not(debug_assertions))]
            register_bundled_host(app.handle());
            // Started with Windows (autostart passes --minimized): stay in the tray if asked.
            let autostarted = std::env::args().any(|a| a == "--minimized");
            // Created here (not from config) so test mode can make it impossible to activate.
            // Normally it starts hidden and show_main shows and focuses it.
            let test = desktop::test_mode();
            // The saved view and search preferences replace the page's on its first load of
            // this run (reloads keep what the page has since changed).
            let seed = if current.ui.is_empty() {
                String::new()
            } else {
                format!(
                    "try {{ if (!sessionStorage.getItem('km-seeded')) {{ localStorage.clear(); Object.assign(localStorage, {}); sessionStorage.setItem('km-seeded', '1'); }} }} catch {{}}",
                    serde_json::to_string(&current.ui)?
                )
            };
            tauri::WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
                .initialization_script(&seed)
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
            save_ui_prefs,
            remove_keywords,
            mark_visited,
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
            pause_check,
            fetch_now,
            browser_sources,
            import_browser,
            import_html_file,
            export_file,
            add_bookmark,
            add_from_clipboard,
            copy_bookmarks,
            saved_items,
            save_item,
            delete_saved_item,
            move_saved_item,
            open_link,
            open_bookmark_new_window,
            add_keywords,
            set_schedule,
            schedule_names
        ])
        .run(tauri::generate_context!())
        .expect("error while running Keymarks");
}

#[cfg(test)]
mod tests {
    use super::{browser_exe, file_bookmarks, normalize_url, reg_sz};

    #[test]
    fn dropped_files_become_file_urls() {
        let dir = std::env::temp_dir();
        let b = file_bookmarks(&[dir.join("My Notes #1.txt"), "relative.txt".into()], "new");
        assert_eq!(b.len(), 1, "a path that isn't absolute is skipped");
        assert!(b[0].url.starts_with("file:///") && b[0].url.ends_with("/My%20Notes%20%231.txt"), "{}", b[0].url);
        assert_eq!((b[0].title.as_str(), b[0].keywords.as_str()), ("My Notes #1.txt", "new"));
    }

    #[test]
    fn default_browser_command_is_parsed() {
        let out = "\r\nHKEY_CLASSES_ROOT\\FirefoxURL\\shell\\open\\command\r\n    (Default)    REG_SZ    \"C:\\Program Files\\Mozilla Firefox\\firefox.exe\" -osint -url \"%1\"\r\n";
        let cmd = reg_sz(out).unwrap();
        assert_eq!(browser_exe(&cmd).as_deref(), Some(r"C:\Program Files\Mozilla Firefox\firefox.exe"));
        assert_eq!(browser_exe(r#""C:\Apps\Unknown\browser.exe" "%1""#), None, "unknown browsers open normally");
        assert_eq!(reg_sz("ERROR: not found"), None);
    }

    #[test]
    fn urls_are_trimmed_completed_and_checked() {
        assert_eq!(normalize_url("  example.com/a ").unwrap(), "https://example.com/a");
        assert_eq!(normalize_url("http://x.example").unwrap(), "http://x.example/");
        assert_eq!(normalize_url("mailto:me@example.com").unwrap(), "mailto:me@example.com");
        assert!(normalize_url("   ").is_err());
        assert!(normalize_url("http://exa mple.com").is_err());
    }
}
