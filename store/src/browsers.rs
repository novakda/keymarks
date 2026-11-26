//! Reading other browsers' bookmarks: Chrome/Edge JSON, Firefox places.sqlite and the
//! Netscape bookmark HTML that every browser exports (and Compass writes, with its own
//! extras). Each reader returns the tree as `Entry`s in the source's order; the browsers'
//! root folders ("Bookmarks bar", "Other bookmarks", …) are flattened into the top level.
//! Folder names are not keywords here: the import decides that (KEY-09).

use crate::{Entry, NewBookmark, normalize_keywords};
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
pub fn read(source: &Source) -> Result<Vec<Entry>, String> {
    match source.browser {
        "Firefox" => read_firefox(&source.path),
        _ => read_chromium(&std::fs::read_to_string(&source.path).map_err(|e| e.to_string())?),
    }
}

/// Folder path → keywords: every word of every folder name, then `extra`.
pub(crate) fn folder_keywords(path: &[String], extra: &str) -> String {
    let words: Vec<&str> = path
        .iter()
        .flat_map(|f| f.split(|c: char| c.is_whitespace() || "&|!(),:;/\\".contains(c)))
        .chain(extra.split_whitespace())
        .collect();
    normalize_keywords(&words.join(" "))
}

fn bookmark(url: String, title: &str, keywords: String, added: Option<i64>) -> Entry {
    Entry::Bookmark(NewBookmark {
        title: if title.is_empty() { url.clone() } else { title.to_string() },
        url,
        keywords,
        description: String::new(),
        notes: String::new(),
        added,
    })
}

const CHROMIUM_ROOTS: [&str; 3] = ["bookmark_bar", "other", "synced"];

/// Chrome/Edge `Bookmarks` JSON.
pub fn read_chromium(json: &str) -> Result<Vec<Entry>, String> {
    let doc: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("not a Chromium bookmarks file: {e}"))?;
    // Chromium time: microseconds since 1601-01-01.
    let time = |node: &serde_json::Value| {
        node["date_added"].as_str().and_then(|d| d.parse::<i64>().ok()).filter(|&d| d > 0).map(|d| d / 1_000_000 - 11_644_473_600)
    };
    fn walk(node: &serde_json::Value, time: &dyn Fn(&serde_json::Value) -> Option<i64>, out: &mut Vec<Entry>) {
        let name = node["name"].as_str().unwrap_or_default();
        match node["type"].as_str() {
            Some("url") => out.push(bookmark(node["url"].as_str().unwrap_or_default().to_string(), name, String::new(), time(node))),
            Some("folder") => {
                out.push(Entry::Folder { name: name.to_string(), note: String::new(), added: time(node) });
                for child in node["children"].as_array().into_iter().flatten() {
                    walk(child, time, out);
                }
                out.push(Entry::Close);
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for root in CHROMIUM_ROOTS {
        for child in doc["roots"][root]["children"].as_array().into_iter().flatten() {
            walk(child, &time, &mut out);
        }
    }
    Ok(out)
}

/// Firefox `places.sqlite`. Firefox keeps it locked while running, so read a temporary copy.
pub fn read_firefox(places: &Path) -> Result<Vec<Entry>, String> {
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

struct Place {
    id: i64,
    kind: i64, // 1 bookmark, 2 folder, 3 separator
    fk: Option<i64>,
    title: String,
    url: Option<String>,
    added: Option<i64>,
    guid: String,
}

fn read_places(db: &Connection) -> Result<Vec<Entry>, String> {
    let e = |e: rusqlite::Error| e.to_string();
    // Children of each item (moz_bookmarks.parent), in Firefox's order.
    let mut kids: HashMap<i64, Vec<Place>> = HashMap::new();
    for row in db
        .prepare(
            "SELECT b.id, b.type, b.fk, b.parent, coalesce(b.title, ''), p.url, b.dateAdded, b.guid
             FROM moz_bookmarks b LEFT JOIN moz_places p ON p.id = b.fk ORDER BY b.parent, b.position, b.id",
        )
        .map_err(e)?
        .query_map([], |r| {
            Ok((r.get::<_, i64>(3)?, Place {
                id: r.get(0)?,
                kind: r.get(1)?,
                fk: r.get(2)?,
                title: r.get(4)?,
                url: r.get(5)?,
                added: r.get::<_, Option<i64>>(6)?.map(|us| us / 1_000_000),
                guid: r.get(7)?,
            }))
        })
        .map_err(e)?
    {
        let (parent, place) = row.map_err(e)?;
        kids.entry(parent).or_default().push(place);
    }
    let none = Vec::new();
    let children = |id: i64| kids.get(&id).unwrap_or(&none);
    let root = kids.values().flatten().find(|p| p.guid == "root________").map(|p| p.id);
    // A tag is a folder under the tags root; the "bookmarks" in it just mark a place.
    let mut tags: HashMap<i64, Vec<&str>> = HashMap::new(); // place id → tag names
    for tags_root in root.into_iter().flat_map(|r| children(r)).filter(|p| p.guid == "tags________") {
        for tag in children(tags_root.id) {
            for mark in children(tag.id) {
                tags.entry(mark.fk.unwrap_or(0)).or_default().push(&tag.title);
            }
        }
    }
    fn walk<'a>(p: &'a Place, children: &dyn Fn(i64) -> &'a Vec<Place>, tags: &HashMap<i64, Vec<&str>>, out: &mut Vec<Entry>) {
        match p.kind {
            1 => {
                let Some(url) = p.url.clone().filter(|u| !u.starts_with("place:")) else { return }; // smart folders
                let keywords = normalize_keywords(&tags.get(&p.fk.unwrap_or(0)).map(|t| t.join(" ")).unwrap_or_default());
                out.push(bookmark(url, &p.title, keywords, p.added));
            }
            2 => {
                out.push(Entry::Folder { name: p.title.clone(), note: String::new(), added: p.added });
                for c in children(p.id) {
                    walk(c, children, tags, out);
                }
                out.push(Entry::Close);
            }
            _ => {} // separators
        }
    }
    let mut out = Vec::new();
    // Firefox's roots (menu, toolbar, unfiled, mobile) flatten into the top level; tags are not bookmarks.
    for top in root.into_iter().flat_map(|r| children(r)).filter(|p| p.guid != "tags________") {
        for c in children(top.id) {
            walk(c, &children, &tags, &mut out);
        }
    }
    Ok(out)
}

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<h3\b([^>]*)>(.*?)</h3>|<a\s([^>]*)>(.*?)</a>|<dd>((?:[^<]|<(?:br|/?meta)\b[^>]*>)*)|<dl\b[^>]*>|</dl>|<base\s([^>]*)>"#).unwrap()
});
static ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)([a-z_-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#).unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
static BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\b[^>]*>").unwrap());

fn attrs(s: &str) -> HashMap<String, String> {
    let text = |s: &str| html_escape::decode_html_entities(s.trim()).into_owned();
    ATTR.captures_iter(s)
        .map(|a| (a[1].to_ascii_lowercase(), text(a.get(2).or(a.get(3)).or(a.get(4)).map_or("", |m| m.as_str()))))
        .collect()
}

/// Text that keeps its `<BR>` line breaks; other tags dropped, spaces collapsed per line.
fn lines(html: &str) -> String {
    let text = TAG.replace_all(&BR.replace_all(html, "\n"), " ").into_owned();
    let text = html_escape::decode_html_entities(&text).into_owned();
    let lines: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    lines.join("\n").trim_matches('\n').to_string()
}

/// A `<DD>`: personal notes, then (Compass 2.83) fetched page meta in `<meta>…</meta>`.
fn dd(raw: &str) -> (String, String) {
    let lower = raw.to_ascii_lowercase();
    match lower.find("<meta>") {
        Some(i) => {
            let end = lower[i..].find("</meta>").map_or(raw.len(), |j| i + j);
            (lines(&raw[..i]), lines(&raw[i + 6..end]))
        }
        None => (lines(raw), String::new()),
    }
}

/// Netscape bookmark HTML (what every browser's "Export bookmarks" writes, and Compass's own
/// file), or any HTML page: its links, relative ones resolved against `<BASE>` (dropped
/// without one). `<DD>` text is the item's (or folder's) note; Compass's `<meta>` part of it
/// is the description. Compass's `STATUS` numbers are unknown, so items start never checked.
pub fn read_netscape_html(html: &str) -> Vec<Entry> {
    let text = |s: &str| html_escape::decode_html_entities(TAG.replace_all(s, " ").split_whitespace().collect::<Vec<_>>().join(" ").as_str()).into_owned();
    let mut base: Option<url::Url> = None;
    let mut out: Vec<Entry> = Vec::new();
    // A folder named by <H3> (and maybe noted by a <DD>), opened by the next <DL>.
    let mut pending: Option<(String, String, Option<i64>)> = None;
    let mut depth = 0usize; // <DL> nesting, to skip the document's own outer list
    let mut folder_depths: Vec<usize> = Vec::new();
    for cap in TOKENS.captures_iter(html) {
        let token = cap.get(0).unwrap().as_str().to_ascii_lowercase();
        if let Some(name) = cap.get(2) {
            let added = attrs(cap.get(1).map_or("", |m| m.as_str())).get("add_date").and_then(|d| d.parse().ok());
            pending = Some((text(name.as_str()), String::new(), added));
        } else if let Some(b) = cap.get(6) {
            base = attrs(b.as_str()).get("href").and_then(|h| url::Url::parse(h).ok());
        } else if let (Some(attrs), Some(title)) = (cap.get(3), cap.get(4)) {
            let attrs = self::attrs(attrs.as_str());
            let Some(href) = attrs.get("href").filter(|u| !u.is_empty() && !u.starts_with('#')) else { continue };
            let url = match url::Url::parse(href) {
                Ok(u) => u,
                Err(_) => match base.as_ref().and_then(|b| b.join(href).ok()) {
                    Some(u) => u,
                    None => continue, // relative, nothing to resolve it against
                },
            };
            if url.scheme() == "javascript" {
                continue;
            }
            // Absolute links stay as written (what browsers exported); resolved ones normalised.
            let url = if href.contains(':') { href.clone() } else { url.into() };
            let tags = normalize_keywords(&attrs.get("tags").map(|t| t.replace(',', " ")).unwrap_or_default());
            out.push(bookmark(url, &text(title.as_str()), tags, attrs.get("add_date").and_then(|d| d.parse().ok())));
        } else if let Some(desc) = cap.get(5) {
            let (notes, meta) = dd(desc.as_str());
            if let Some((_, note, _)) = pending.as_mut() {
                *note = notes;
            } else if let Some(Entry::Bookmark(last)) = out.last_mut() {
                (last.notes, last.description) = (notes, meta);
            }
        } else if token.starts_with("<dl") {
            depth += 1;
            if let Some((name, note, added)) = pending.take() {
                out.push(Entry::Folder { name, note, added });
                folder_depths.push(depth);
            }
        } else if token == "</dl>" {
            if folder_depths.last() == Some(&depth) {
                folder_depths.pop();
                out.push(Entry::Close);
            }
            depth = depth.saturating_sub(1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bookmarks;

    #[test]
    fn chromium_json() {
        let json = r#"{"roots":{
            "bookmark_bar":{"name":"Bookmarks bar","type":"folder","children":[
                {"type":"url","name":"Rust","url":"https://rust-lang.org/","date_added":"13300000000000000"},
                {"type":"folder","name":"Dev Tools","children":[
                    {"type":"folder","name":"Async/IO","children":[{"type":"url","name":"","url":"https://tokio.rs/"}]},
                    {"type":"folder","name":"Empty","children":[]}]}]},
            "other":{"name":"Other bookmarks","type":"folder","children":[{"type":"url","name":"News","url":"https://news.example/"}]},
            "synced":{"name":"Mobile bookmarks","type":"folder","children":[]}}}"#;
        let e = read_chromium(json).unwrap();
        assert_eq!(shape(&e), "Rust [Dev Tools [Async/IO https://tokio.rs/ ] [Empty ] ] News", "roots flattened, tree and order kept");
        let b = bookmarks(&e);
        assert_eq!((b[0].keywords.as_str(), b[0].added), ("", Some(1_655_526_400)));
        assert_eq!(b[1].keywords, "", "folder names are not keywords here; the import decides");
        assert_eq!(b[1].title, "https://tokio.rs/", "untitled falls back to the URL");
        assert!(read_chromium("not json").is_err());
    }

    /// The tree as text: `[name ... ]` for folders, titles for bookmarks.
    fn shape(entries: &[Entry]) -> String {
        entries
            .iter()
            .map(|e| match e {
                Entry::Folder { name, .. } => format!("[{name}"),
                Entry::Close => "]".into(),
                Entry::Bookmark(b) => b.title.clone(),
                Entry::Row(r) => r.title.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn any_html_page() {
        let html = r##"<html><head><base href='https://site.example/docs/'></head><body>
<p>See <a href="intro.html"><b>The</b> intro</a>, <a href=/about>About</a>, <a href="#top">top</a>,
<a href="javascript:void(0)">js</a> and <a class=x href="https://other.example/">other</a>.</p></body></html>"##;
        let e = read_netscape_html(html);
        assert_eq!(bookmarks(&e).iter().map(|b| (b.url.as_str(), b.title.as_str())).collect::<Vec<_>>(), [
            ("https://site.example/docs/intro.html", "The intro"),
            ("https://site.example/about", "About"),
            ("https://other.example/", "other"),
        ]);
        assert_eq!(e.len(), 3, "no folders");
        assert_eq!(read_netscape_html(r#"<a href="page.html">no base</a>"#).len(), 0);
    }

    #[test]
    fn netscape_html() {
        let html = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<TITLE>Bookmarks</TITLE><H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1600000000">Search &amp; Engines</H3>
    <DL><p>
        <DT><A HREF="https://duck.example/" ADD_DATE="1700000000" TAGS="privacy,web">Duck &amp; Go</A>
        <DD>A search engine
        <DT><H3>Old</H3>
        <DL><p>
            <DT><A HREF="https://old.example/">Old one</A>
        </DL><p>
        <DT><H3>Nothing here</H3>
        <DL><p>
        </DL><p>
    </DL><p>
    <DT><A HREF="https://top.example/">Top</A>
    <DT><A HREF="">no url</A>
</DL><p>"#;
        let e = read_netscape_html(html);
        assert_eq!(shape(&e), "[Search & Engines Duck & Go [Old Old one ] [Nothing here ] ] Top");
        assert!(matches!(&e[0], Entry::Folder { added: Some(1_600_000_000), .. }));
        let b = bookmarks(&e);
        assert_eq!(b[0].keywords, "privacy web", "TAGS stay keywords");
        assert_eq!((b[0].notes.as_str(), b[0].description.as_str()), ("A search engine", ""), "<DD> is a note (XFER-03)");
        assert_eq!(b[0].added, Some(1_700_000_000));
        assert_eq!(b[1].keywords, "");
    }

    #[test]
    fn compass_html() {
        // Compass 2.83's own file: STATUS, folder notes, and fetched meta kept apart from notes.
        let html = r#"<!-- Created by Compass v.2.83 -->
<TITLE>Compass Bookmarks</TITLE>
<H1>Compass</H1>
<DL><p>
<DT><H3 FOLDED ADD_DATE="1000000000">Comics</H3><DD>Daily reads<BR>check Mondays
<DL><p>
<DT><A HREF="http://comic.example/" ADD_DATE="1000000001" LAST_VISIT="1000000002" LAST_MODIFIED="" STATUS="3">A comic</A><DD>my own notes<BR>line two<meta><BR>Fetched description &amp; keywords</meta>
<DT><A HREF="http://plain.example/" ADD_DATE="1000000003" LAST_VISIT="0" LAST_MODIFIED="" STATUS="0">Plain</A>
<HR>
<DT><A HREF="http://meta.example/" STATUS="1">Only meta</A><DD><meta><BR>just fetched</meta>
</DL><p>
</DL><p>"#;
        let e = read_netscape_html(html);
        assert_eq!(shape(&e), "[Comics A comic Plain Only meta ]", "separators are skipped");
        assert!(matches!(&e[0], Entry::Folder { note, .. } if note == "Daily reads\ncheck Mondays"), "folder note, <BR> as a line break");
        let b = bookmarks(&e);
        assert_eq!((b[0].notes.as_str(), b[0].description.as_str()), ("my own notes\nline two", "Fetched description & keywords"));
        assert_eq!((b[1].notes.as_str(), b[1].description.as_str()), ("", ""));
        assert_eq!((b[2].notes.as_str(), b[2].description.as_str()), ("", "just fetched"));
        assert_eq!(b[0].added, Some(1_000_000_001));
    }

    #[test]
    fn firefox_places() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT);
             CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER, parent INTEGER, position INTEGER, title TEXT, dateAdded INTEGER, guid TEXT);
             INSERT INTO moz_places VALUES (1, 'https://mdn.example/'), (2, 'place:sort=8'), (3, 'https://blog.example/'), (4, 'https://first.example/');
             INSERT INTO moz_bookmarks VALUES
               (1, 2, NULL, 0, 0, '', NULL, 'root________'),
               (2, 2, NULL, 1, 0, 'menu', NULL, 'menu________'),
               (3, 2, NULL, 1, 1, 'tags', NULL, 'tags________'),
               (4, 2, NULL, 1, 2, 'toolbar', NULL, 'toolbar_____'),
               (10, 2, NULL, 2, 1, 'Web Docs', 1600000000000000, 'aaaaaaaaaaaa'),
               (11, 1, 1, 10, 0, 'MDN', 1700000000000000, 'b1'),
               (12, 1, 2, 2, 2, 'Recent', NULL, 'b2'),
               (13, 1, 3, 2, 3, '', NULL, 'b3'),
               (14, 1, 4, 2, 0, 'First', NULL, 'b4'),
               (15, 2, NULL, 10, 1, 'Empty', NULL, 'cccccccccccc'),
               (16, 3, NULL, 4, 0, '', NULL, 'sep'),
               (20, 2, NULL, 3, 0, 'reference', NULL, 't1'),
               (21, 1, 1, 20, 0, NULL, NULL, 't2');",
        )
        .unwrap();
        let e = read_places(&db).unwrap();
        assert_eq!(shape(&e), "First [Web Docs MDN [Empty ] ] https://blog.example/", "smart folders, separators and tag entries are not bookmarks; order by position");
        assert!(matches!(&e[1], Entry::Folder { added: Some(1_600_000_000), .. }));
        let b = bookmarks(&e);
        assert_eq!((b[1].keywords.as_str(), b[1].added), ("reference", Some(1_700_000_000)), "tags are keywords");
    }
}
