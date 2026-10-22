//! Reading other browsers' bookmarks: Chrome/Edge JSON, Firefox places.sqlite and the
//! Netscape bookmark HTML that every browser exports. Folder names become keywords (as
//! Powermarks did on import); browser root folders such as "Bookmarks bar" are skipped.

use crate::{NewBookmark, normalize_keywords};
use regex::Regex;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// A browser profile found on this machine.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub browser: &'static str,
    pub profile: String,
    pub path: PathBuf,
}

/// Chrome and Edge profiles under %LOCALAPPDATA%, Firefox profiles under %APPDATA%.
pub fn detect() -> Vec<Source> {
    let mut found = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for (browser, dir) in [("Chrome", "Google\\Chrome\\User Data"), ("Edge", "Microsoft\\Edge\\User Data")] {
            for profile in subdirs(&local.join(dir)) {
                let file = profile.join("Bookmarks");
                if file.is_file() {
                    found.push(Source { browser, profile: file_name(&profile), path: file });
                }
            }
        }
    }
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        for profile in subdirs(&roaming.join("Mozilla\\Firefox\\Profiles")) {
            let file = profile.join("places.sqlite");
            if file.is_file() {
                found.push(Source { browser: "Firefox", profile: file_name(&profile), path: file });
            }
        }
    }
    found
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Read a detected source.
pub fn read(source: &Source) -> Result<Vec<NewBookmark>, String> {
    match source.browser {
        "Firefox" => read_firefox(&source.path),
        _ => read_chromium(&std::fs::read_to_string(&source.path).map_err(|e| e.to_string())?),
    }
}

/// Folder path → keywords: every word of every folder name.
fn folder_keywords(path: &[String], extra: &str) -> String {
    let words: Vec<&str> = path
        .iter()
        .flat_map(|f| f.split(|c: char| c.is_whitespace() || "&|!(),:;/\\".contains(c)))
        .chain(extra.split_whitespace())
        .collect();
    normalize_keywords(&words.join(" "))
}

const CHROMIUM_ROOTS: [&str; 3] = ["bookmark_bar", "other", "synced"];

/// Chrome/Edge `Bookmarks` JSON.
pub fn read_chromium(json: &str) -> Result<Vec<NewBookmark>, String> {
    let doc: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("not a Chromium bookmarks file: {e}"))?;
    fn walk(node: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<NewBookmark>) {
        let name = node["name"].as_str().unwrap_or_default();
        match node["type"].as_str() {
            Some("url") => {
                let url = node["url"].as_str().unwrap_or_default().to_string();
                // Chromium time: microseconds since 1601-01-01.
                let added = node["date_added"].as_str().and_then(|d| d.parse::<i64>().ok()).filter(|&d| d > 0).map(|d| d / 1_000_000 - 11_644_473_600);
                out.push(NewBookmark { title: if name.is_empty() { url.clone() } else { name.to_string() }, url, keywords: folder_keywords(path, ""), description: String::new(), added });
            }
            Some("folder") => {
                path.push(name.to_string());
                for child in node["children"].as_array().into_iter().flatten() {
                    walk(child, path, out);
                }
                path.pop();
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for root in CHROMIUM_ROOTS {
        // The roots' own names ("Bookmarks bar", …) are not keywords.
        for child in doc["roots"][root]["children"].as_array().into_iter().flatten() {
            walk(child, &mut Vec::new(), &mut out);
        }
    }
    Ok(out)
}

/// Firefox `places.sqlite`. Firefox keeps it locked while running, so read a temporary copy.
pub fn read_firefox(places: &Path) -> Result<Vec<NewBookmark>, String> {
    let tmp = std::env::temp_dir().join(format!("keymarks-places-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let copy = tmp.join("places.sqlite");
    let result = (|| {
        std::fs::copy(places, &copy).map_err(|e| e.to_string())?;
        let wal = places.with_extension("sqlite-wal");
        if wal.is_file() {
            std::fs::copy(&wal, tmp.join("places.sqlite-wal")).map_err(|e| e.to_string())?;
        }
        read_places(&Connection::open(&copy).map_err(|e| e.to_string())?)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn read_places(db: &Connection) -> Result<Vec<NewBookmark>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    // Folders (type 2) by id: (parent, title, root guid if it is one of Firefox's roots).
    let mut folders: HashMap<i64, (i64, String, String)> = HashMap::new();
    for row in db
        .prepare("SELECT id, parent, coalesce(title, ''), guid FROM moz_bookmarks WHERE type = 2")
        .map_err(e)?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))
        .map_err(e)?
    {
        let (id, f) = row.map_err(e)?;
        folders.insert(id, f);
    }
    let is_root = |guid: &str| guid.ends_with("____") || guid == "root________";
    let tags_root = folders.iter().find(|(_, f)| f.2 == "tags________").map(|(id, _)| *id);
    let mut bookmarks = Vec::new();
    let mut tags: HashMap<i64, Vec<String>> = HashMap::new(); // place id → tag names
    for row in db
        .prepare(
            "SELECT b.parent, b.fk, coalesce(b.title, ''), p.url, b.dateAdded FROM moz_bookmarks b
             JOIN moz_places p ON p.id = b.fk WHERE b.type = 1 ORDER BY b.id",
        )
        .map_err(e)?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<i64>>(4)?)))
        .map_err(e)?
    {
        let (parent, place, title, url, added) = row.map_err(e)?;
        // A tag is a folder under the tags root; the "bookmarks" in it just mark a place.
        if let Some(tag_folder) = folders.get(&parent).filter(|f| Some(f.0) == tags_root) {
            tags.entry(place).or_default().push(tag_folder.1.clone());
            continue;
        }
        if url.starts_with("place:") {
            continue; // smart folders / saved queries
        }
        let mut path = Vec::new();
        let mut at = parent;
        while let Some((up, name, guid)) = folders.get(&at) {
            if !is_root(guid) {
                path.push(name.clone());
            }
            at = *up;
        }
        path.reverse();
        bookmarks.push((place, path, title, url, added.map(|us| us / 1_000_000)));
    }
    Ok(bookmarks
        .into_iter()
        .map(|(place, path, title, url, added)| NewBookmark {
            title: if title.is_empty() { url.clone() } else { title },
            keywords: folder_keywords(&path, &tags.get(&place).map(|t| t.join(" ")).unwrap_or_default()),
            url,
            description: String::new(),
            added,
        })
        .collect())
}

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<h3[^>]*>(.*?)</h3>|<a\s([^>]*)>(.*?)</a>|<dd>([^<]*)|<dl\b[^>]*>|</dl>"#).unwrap()
});
static ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)([a-z_]+)\s*=\s*"([^"]*)""#).unwrap());

/// Netscape bookmark HTML (what every browser's "Export bookmarks" writes).
pub fn read_netscape_html(html: &str) -> Vec<NewBookmark> {
    let text = |s: &str| html_escape::decode_html_entities(s.trim()).into_owned();
    let mut out: Vec<NewBookmark> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut pending: Option<String> = None; // folder named by <H3>, opened by the next <DL>
    let mut depth = 0usize; // <DL> nesting, to skip the document's own outer list
    let mut folder_depths: Vec<usize> = Vec::new();
    for cap in TOKENS.captures_iter(html) {
        let token = cap.get(0).unwrap().as_str().to_ascii_lowercase();
        if let Some(name) = cap.get(1) {
            pending = Some(text(name.as_str()));
        } else if let (Some(attrs), Some(title)) = (cap.get(2), cap.get(3)) {
            let attrs: HashMap<String, String> =
                ATTR.captures_iter(attrs.as_str()).map(|a| (a[1].to_ascii_lowercase(), text(&a[2]))).collect();
            let Some(url) = attrs.get("href").filter(|u| !u.is_empty()).cloned() else { continue };
            let title = text(title.as_str());
            let tags = attrs.get("tags").map(|t| t.replace(',', " ")).unwrap_or_default();
            out.push(NewBookmark {
                title: if title.is_empty() { url.clone() } else { title },
                keywords: folder_keywords(&path, &tags),
                url,
                description: String::new(),
                added: attrs.get("add_date").and_then(|d| d.parse().ok()),
            });
        } else if let Some(desc) = cap.get(4) {
            if let Some(last) = out.last_mut() {
                last.description = text(desc.as_str());
            }
        } else if token.starts_with("<dl") {
            depth += 1;
            if let Some(name) = pending.take() {
                path.push(name);
                folder_depths.push(depth);
            }
        } else if token == "</dl>" {
            if folder_depths.last() == Some(&depth) {
                folder_depths.pop();
                path.pop();
            }
            depth = depth.saturating_sub(1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_json() {
        let json = r#"{"roots":{
            "bookmark_bar":{"name":"Bookmarks bar","type":"folder","children":[
                {"type":"url","name":"Rust","url":"https://rust-lang.org/","date_added":"13300000000000000"},
                {"type":"folder","name":"Dev Tools","children":[
                    {"type":"folder","name":"Async/IO","children":[{"type":"url","name":"","url":"https://tokio.rs/"}]}]}]},
            "other":{"name":"Other bookmarks","type":"folder","children":[{"type":"url","name":"News","url":"https://news.example/"}]},
            "synced":{"name":"Mobile bookmarks","type":"folder","children":[]}}}"#;
        let b = read_chromium(json).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].keywords.as_str(), b[0].added), ("", Some(1_655_526_400)));
        assert_eq!(b[1].keywords, "Dev Tools Async IO");
        assert_eq!(b[1].title, "https://tokio.rs/", "untitled falls back to the URL");
        assert!(read_chromium("not json").is_err());
    }

    #[test]
    fn netscape_html() {
        let html = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<TITLE>Bookmarks</TITLE><H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1">Search &amp; Engines</H3>
    <DL><p>
        <DT><A HREF="https://duck.example/" ADD_DATE="1700000000" TAGS="privacy,web">Duck &amp; Go</A>
        <DD>A search engine
        <DT><H3>Old</H3>
        <DL><p>
            <DT><A HREF="https://old.example/">Old one</A>
        </DL><p>
    </DL><p>
    <DT><A HREF="https://top.example/">Top</A>
    <DT><A HREF="">no url</A>
</DL><p>"#;
        let b = read_netscape_html(html);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].title, "Duck & Go");
        assert_eq!(b[0].keywords, "Search Engines privacy web");
        assert_eq!(b[0].description, "A search engine");
        assert_eq!(b[0].added, Some(1_700_000_000));
        assert_eq!(b[1].keywords, "Search Engines Old");
        assert_eq!(b[2].keywords, "", "back at the top level");
    }

    #[test]
    fn firefox_places() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT);
             CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER, parent INTEGER, title TEXT, dateAdded INTEGER, guid TEXT);
             INSERT INTO moz_places VALUES (1, 'https://mdn.example/'), (2, 'place:sort=8'), (3, 'https://blog.example/');
             INSERT INTO moz_bookmarks VALUES
               (1, 2, NULL, 0, '', NULL, 'root________'),
               (2, 2, NULL, 1, 'menu', NULL, 'menu________'),
               (3, 2, NULL, 1, 'tags', NULL, 'tags________'),
               (10, 2, NULL, 2, 'Web Docs', NULL, 'aaaaaaaaaaaa'),
               (11, 1, 1, 10, 'MDN', 1700000000000000, 'b1'),
               (12, 1, 2, 2, 'Recent', NULL, 'b2'),
               (13, 1, 3, 2, '', NULL, 'b3'),
               (20, 2, NULL, 3, 'reference', NULL, 't1'),
               (21, 1, 1, 20, NULL, NULL, 't2');",
        )
        .unwrap();
        let b = read_places(&db).unwrap();
        assert_eq!(b.len(), 2, "smart folders and tag entries are not bookmarks");
        assert_eq!((b[0].keywords.as_str(), b[0].added), ("Web Docs reference", Some(1_700_000_000)));
        assert_eq!((b[1].title.as_str(), b[1].keywords.as_str()), ("https://blog.example/", ""));
    }
}
