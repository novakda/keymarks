//! Writing bookmarks out: Netscape bookmark HTML (any browser imports it), Powermarks 3.5
//! `pm35.htm` (the original program can open it again), CSV, a printable HTML table and
//! full-fidelity JSON (which Keymarks imports back).

use crate::{Entry, Folder, Result, SavedItem, Store};
use pm35::{Database, field};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// Everything an export needs about one bookmark.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExportRow {
    pub url: String,
    pub title: String,
    pub keywords: String,
    pub description: String,
    pub notes: String,
    pub user_field: String,
    pub added: Option<i64>,
    pub last_visited: Option<i64>,
    pub last_modified: Option<i64>,
    pub last_checked: Option<i64>,
    pub visits: i64,
    pub rating: i64,
    pub check_enabled: bool,
    pub schedule: Option<String>,
    /// The record imported from Powermarks, if any (kept so unknown fields survive).
    pub pm_mdata: Option<String>,
    /// "bookmark" (also when empty, for files written before folders), or "note" (no URL).
    pub kind: String,
    /// The folder it sits in (0: unknown, the top level) and its place there.
    pub folder_id: i64,
    pub position: i64,
}

/// What an export covers: rows, and for a folder (or everything) the folders below it, so
/// the tree can be written out. `top` is the folder whose contents become the top level;
/// None is a flat list (a selection or search), kept in its given order.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Tree {
    pub top: Option<i64>,
    pub folders: Vec<Folder>,
    #[serde(rename = "bookmarks")]
    pub rows: Vec<ExportRow>,
}

impl Tree {
    pub fn flat(rows: Vec<ExportRow>) -> Tree {
        Tree { top: None, folders: Vec::new(), rows }
    }

    /// The tree in stored order as import entries (what `Store::import_tree` takes). Rows
    /// whose folder isn't in the tree come last at the top level, in their given order.
    pub fn entries(&self) -> Vec<Entry> {
        // Children per folder: (position, folders before items, index into folders or rows).
        let mut kids: HashMap<i64, Vec<(i64, bool, usize)>> = HashMap::new();
        for (i, f) in self.folders.iter().enumerate() {
            kids.entry(f.parent_id.unwrap_or(0)).or_default().push((f.position, false, i));
        }
        let known: HashSet<i64> = self.top.into_iter().chain(self.folders.iter().map(|f| f.id)).collect();
        let mut loose = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            if known.contains(&r.folder_id) {
                kids.entry(r.folder_id).or_default().push((r.position, true, i));
            } else {
                loose.push(i);
            }
        }
        for v in kids.values_mut() {
            v.sort(); // stable on ties: folders first, then source order (as `siblings` does)
        }
        fn walk(t: &Tree, kids: &HashMap<i64, Vec<(i64, bool, usize)>>, folder: i64, out: &mut Vec<Entry>) {
            for &(_, item, i) in kids.get(&folder).into_iter().flatten() {
                if item {
                    out.push(Entry::Row(Box::new(t.rows[i].clone())));
                } else {
                    let f = &t.folders[i];
                    out.push(Entry::Folder { name: f.name.clone(), note: f.note.clone(), added: Some(f.added) });
                    walk(t, kids, f.id, out);
                    out.push(Entry::Close);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(top) = self.top {
            walk(self, &kids, top, &mut out);
        }
        out.extend(loose.into_iter().map(|i| Entry::Row(Box::new(self.rows[i].clone()))));
        out
    }

    /// The rows in tree order, for the formats without folders (CSV, printable, pm35).
    pub fn rows_in_order(&self) -> Vec<ExportRow> {
        self.entries().into_iter().filter_map(|e| if let Entry::Row(r) = e { Some(*r) } else { None }).collect()
    }
}

const ROW_COLS: &str = "url, title, keywords, description, notes, user_field, added, last_visited, last_modified, \
                        last_checked, visits, rating, check_enabled, pm_mdata, schedule, kind, folder_id, position";

fn export_row(r: &rusqlite::Row) -> Result<ExportRow> {
    Ok(ExportRow {
        url: r.get(0)?,
        title: r.get(1)?,
        keywords: r.get(2)?,
        description: r.get(3)?,
        notes: r.get(4)?,
        user_field: r.get(5)?,
        added: r.get(6)?,
        last_visited: r.get(7)?,
        last_modified: r.get(8)?,
        last_checked: r.get(9)?,
        visits: r.get(10)?,
        rating: r.get(11)?,
        check_enabled: r.get(12)?,
        pm_mdata: r.get(13)?,
        schedule: r.get(14)?,
        kind: r.get(15)?,
        folder_id: r.get(16)?,
        position: r.get(17)?,
    })
}

impl Store {
    /// Rows to export: the given ids in that order, or every bookmark (oldest first).
    pub fn export_rows(&self, ids: Option<&[i64]>) -> Result<Vec<ExportRow>> {
        let cols = ROW_COLS;
        match ids {
            None => self.conn.prepare(&format!("SELECT {cols} FROM bookmark ORDER BY coalesce(added, 0), id"))?.query_map([], export_row)?.collect(),
            Some(ids) => {
                let json = serde_json::to_string(ids).expect("ids serialize");
                // json_each keeps the requested order via its key.
                self.conn
                    .prepare(&format!(
                        "SELECT {cols} FROM json_each(?1) j JOIN bookmark b ON b.id = j.value ORDER BY j.key"
                    ))?
                    .query_map(params![json], export_row)?
                    .collect()
            }
        }
    }

    /// Folder `top` and everything below it (XFER-16 range "a folder"; ROOT is everything).
    /// A missing folder gives an empty tree.
    pub fn export_tree(&self, top: i64) -> Result<Tree> {
        let sub = "WITH RECURSIVE sub(id) AS (SELECT id FROM folder WHERE id = ?1
                   UNION ALL SELECT f.id FROM folder f JOIN sub ON f.parent_id = sub.id)";
        let folders = self
            .conn
            .prepare(&format!("{sub} SELECT {} FROM folder WHERE id IN sub AND id <> ?1 ORDER BY parent_id, position, id", crate::folders::FOLDER_COLS))?
            .query_map([top], crate::folders::folder_row)?
            .collect::<Result<_>>()?;
        let rows = self
            .conn
            .prepare(&format!("{sub} SELECT {ROW_COLS} FROM bookmark WHERE folder_id IN sub ORDER BY folder_id, position, id"))?
            .query_map([top], export_row)?
            .collect::<Result<_>>()?;
        Ok(Tree { top: Some(top), folders, rows })
    }
}

impl Store {
    /// A Powermarks file of `rows`, with Saved items as macros and tagged keywords, so
    /// Powermarks 3.5 gets those back too.
    pub fn export_pm35(&self, rows: &[ExportRow]) -> Result<Vec<u8>> {
        let tagged: Vec<String> = self.conn.prepare("SELECT keyword FROM tagged_keyword ORDER BY keyword")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
        Ok(to_pm35_with(rows, &self.saved_items()?, &tagged))
    }
}

/// Fields CSV and printable exports can carry, in this order, with their headings.
pub const EXPORT_FIELDS: [(&str, &str); 13] = [
    ("title", "Title"),
    ("url", "URL"),
    ("keywords", "Keywords"),
    ("description", "Description"),
    ("notes", "Notes"),
    ("user_field", "User field"),
    ("rating", "Rating"),
    ("visits", "Visits"),
    ("added", "Added"),
    ("last_visited", "Last visited"),
    ("last_modified", "Last modified"),
    ("last_checked", "Last checked"),
    ("schedule", "Schedule"),
];

/// Unix seconds as "YYYY-MM-DD HH:MM" (UTC), which spreadsheets read as a date.
fn datetime(t: Option<i64>) -> String {
    let Some(t) = t else { return String::new() };
    // Days to civil date: Howard Hinnant's algorithm.
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

impl ExportRow {
    fn field(&self, name: &str) -> String {
        match name {
            "title" => self.title.clone(),
            "url" => self.url.clone(),
            "keywords" => self.keywords.clone(),
            "description" => self.description.clone(),
            "notes" => self.notes.clone(),
            "user_field" => self.user_field.clone(),
            "rating" if self.rating > 0 => self.rating.to_string(),
            "visits" => self.visits.to_string(),
            "added" => datetime(self.added),
            "last_visited" => datetime(self.last_visited),
            "last_modified" => datetime(self.last_modified),
            "last_checked" => datetime(self.last_checked),
            "schedule" => self.schedule.clone().unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// The chosen fields (unknown names dropped) in EXPORT_FIELDS order; none = title and URL.
fn chosen(fields: &[String]) -> Vec<(&'static str, &'static str)> {
    let picked: Vec<_> = EXPORT_FIELDS.into_iter().filter(|(f, _)| fields.iter().any(|x| x == f)).collect();
    if picked.is_empty() { EXPORT_FIELDS[..2].to_vec() } else { picked }
}

/// CSV (RFC 4180) with a heading row, UTF-8 with a byte-order mark so Excel reads it right.
pub fn to_csv(rows: &[ExportRow], fields: &[String]) -> String {
    let fields = chosen(fields);
    let cell = |s: &str| if s.contains([',', '"', '\r', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let line = |cells: Vec<String>| cells.iter().map(|c| cell(c)).collect::<Vec<_>>().join(",") + "\r\n";
    let mut out = String::from("\u{feff}");
    out += &line(fields.iter().map(|(_, h)| h.to_string()).collect());
    for r in rows {
        out += &line(fields.iter().map(|(f, _)| r.field(f)).collect());
    }
    out
}

/// A table to read or print (Powermarks' printable export), titled `title`.
pub fn to_printable(rows: &[ExportRow], fields: &[String], title: &str) -> String {
    let fields = chosen(fields);
    let title = html(if title.trim().is_empty() { "Bookmarks" } else { title.trim() });
    let mut out = format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title>\n<style>\n\
         body {{ font: 10pt system-ui, sans-serif; margin: 1.5em; color: #000; background: #fff; }}\n\
         table {{ border-collapse: collapse; width: 100%; }}\n\
         th, td {{ border-bottom: 1px solid #ccc; padding: 3px 6px; text-align: left; vertical-align: top; overflow-wrap: anywhere; }}\n\
         th {{ border-bottom: 2px solid #000; }}\n\
         thead {{ display: table-header-group; }} tr {{ break-inside: avoid; }}\n\
         </style></head><body>\n<h1>{title}</h1>\n<p>{} bookmark{}</p>\n<table>\n<thead><tr>",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
    for (_, h) in &fields {
        out += &format!("<th>{h}</th>");
    }
    out += "</tr></thead>\n<tbody>\n";
    for r in rows {
        out += "<tr>";
        for (f, _) in &fields {
            out += &format!("<td>{}</td>", html(&r.field(f)));
        }
        out += "</tr>\n";
    }
    out + "</tbody></table>\n</body></html>\n"
}

/// Every field of every row, and the folders, for scripts and for importing back into Keymarks.
pub fn to_json(tree: &Tree) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "format": "keymarks-1", "top": tree.top, "folders": tree.folders, "bookmarks": tree.rows }))
        .expect("rows serialize")
}

/// The tree in a Keymarks JSON export (files from before folders read as a flat list).
pub fn read_json(text: &str) -> std::result::Result<Tree, String> {
    #[derive(serde::Deserialize)]
    struct File {
        format: String,
        #[serde(flatten)]
        tree: Tree,
    }
    let file: File = serde_json::from_str(text).map_err(|e| format!("not a Keymarks JSON export: {e}"))?;
    if file.format != "keymarks-1" {
        return Err(format!("unknown Keymarks JSON format {}", file.format));
    }
    Ok(file.tree)
}

#[cfg(test)]
pub(crate) fn export_rows_all(s: &Store) -> Vec<ExportRow> {
    s.export_rows(None).unwrap()
}

fn html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Text for a `<DD>`: escaped, line breaks as `<BR>`.
fn dd_text(s: &str) -> String {
    html(s).replace("\r\n", "\n").replace('\n', "<BR>")
}

/// Netscape bookmark HTML, the tree as `<H3>`/`<DL>` (XFER-16). Keywords travel as
/// Firefox-style TAGS; a `<DD>` holds the notes, then the description in Compass's `<meta>`
/// part, so Keymarks (and Compass) read both back; a folder's `<DD>` is its note.
/// URL-less notes aren't links and are left out.
pub fn to_netscape(tree: &Tree) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <!-- This is an automatically generated file. Exported by Keymarks. -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    let mut depth = 1;
    for e in tree.entries() {
        let pad = "    ".repeat(depth);
        match e {
            Entry::Folder { name, note, added } => {
                let date = added.map(|t| format!(" ADD_DATE=\"{t}\"")).unwrap_or_default();
                out += &format!("{pad}<DT><H3{date}>{}</H3>\n", html(&name));
                if !note.is_empty() {
                    out += &format!("{pad}<DD>{}\n", dd_text(&note));
                }
                out += &format!("{pad}<DL><p>\n");
                depth += 1;
            }
            Entry::Close => {
                depth -= 1;
                out += &format!("{}</DL><p>\n", "    ".repeat(depth));
            }
            Entry::Row(r) if r.url.is_empty() => {}
            Entry::Row(r) => {
                let mut attrs = format!("HREF=\"{}\"", html(&r.url));
                if let Some(t) = r.added {
                    attrs += &format!(" ADD_DATE=\"{t}\"");
                }
                if let Some(t) = r.last_visited {
                    attrs += &format!(" LAST_VISIT=\"{t}\"");
                }
                if !r.keywords.is_empty() {
                    attrs += &format!(" TAGS=\"{}\"", html(&r.keywords.split_whitespace().collect::<Vec<_>>().join(",")));
                }
                out += &format!("{pad}<DT><A {attrs}>{}</A>\n", html(&r.title));
                let meta = if r.description.is_empty() { String::new() } else { format!("<meta><BR>{}</meta>", dd_text(&r.description)) };
                if !r.notes.is_empty() || !meta.is_empty() {
                    out += &format!("{pad}<DD>{}{meta}\n", dd_text(&r.notes));
                }
            }
            Entry::Bookmark(_) => unreachable!("trees from the store hold rows"),
        }
    }
    out + "</DL><p>\n"
}

/// Text as Powermarks stores it: Latin-1 characters as-is (the writer turns them into bytes),
/// markup characters and anything beyond Latin-1 as HTML entities, no line breaks.
fn pm_text(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\r' | '\n' => " ".into(),
            c if (c as u32) > 0xff => format!("&#{};", c as u32),
            c => c.to_string(),
        })
        .collect()
}

const PM35_HEAD: &str = "<!DOCTYPE HTML PUBLIC \"-//IETF//DTD HTML 3.2//EN\">\r\n<!--Powermarks Format 3.5-->\r\n\
<!-- This is an automatically generated file.\r\nIt will be read and overwritten.\r\nDo Not Edit! -->\r\n\r\n\
<HTML>\r\n<HEAD>\r\n<!--scs=[Weekly][1][93A80][37D0B99E]-->\r\n</HEAD>\r\n<BODY>\r\n<DL>\r\n";
const PM35_TAIL: &str = "</DL><p>\r\n</BODY>\r\n</HTML>";

/// Powermarks 3.5 file bytes. The imported 15-field record is reused (so fields Keymarks
/// doesn't understand survive) with the fields Keymarks tracks brought up to date.
pub fn to_pm35(rows: &[ExportRow]) -> Vec<u8> {
    to_pm35_with(rows, &[], &[])
}

/// A Saved item as a macro record (layouts in `pm35::records`). Powermarks has no
/// "all fields" word search, so those become keyword searches.
fn macro_record(item: &SavedItem) -> String {
    let o: serde_json::Value = serde_json::from_str(&item.options).unwrap_or_default();
    let flag = |k: &str| if o[k].as_bool().unwrap_or(false) { "1" } else { "0" };
    let name = pm_text(&item.name);
    match (item.kind.as_str(), o["mode"].as_str()) {
        ("link", _) => format!("[0][{name}][0][0][{}]", pm_text(&item.url)),
        (_, Some("exact")) => {
            let on = |f: &str| o["fields"].as_array().is_none_or(|a| a.iter().any(|v| v == f));
            let mask: u32 = crate::PM_FIELD_BITS.iter().enumerate().filter(|(_, f)| on(f)).map(|(i, _)| 1 << i).sum();
            format!("[2][{name}][0][0][{}][{}][{mask:X}]", pm_text(&item.query), flag("match_case"))
        }
        _ => format!("[1][{name}][0][0][{}][{}][0]", pm_text(&item.query), flag("partial")),
    }
}

pub fn to_pm35_with(rows: &[ExportRow], saved: &[SavedItem], tagged: &[String]) -> Vec<u8> {
    let hex = |t: Option<i64>| format!("{:X}", t.unwrap_or(0).clamp(0, u32::MAX as i64));
    let entries = rows
        .iter()
        .filter(|r| !r.url.is_empty() || r.pm_mdata.is_some()) // notes, but not Powermarks' own records
        .map(|r| {
            let mut m: Vec<String> = r
                .pm_mdata
                .as_deref()
                .map(|raw| raw.split("][").map(String::from).collect())
                .filter(|m: &Vec<String>| m.len() == 15)
                .unwrap_or_else(|| {
                    // A fresh record: weekly schedule, nothing checked yet.
                    let mut m = vec![String::new(); 15];
                    m[field::SCHEDULE] = "Weekly".into();
                    m[field::STATUS_TIME] = hex(r.added);
                    for i in [field::STATUS, field::HASH_MODE, 14] {
                        m[i] = "0".into();
                    }
                    m
                });
            m[field::ADDED] = hex(r.added);
            m[field::LAST_VISITED] = hex(r.last_visited);
            m[field::LAST_MODIFIED] = hex(r.last_modified);
            m[field::LAST_CHECKED] = hex(r.last_checked);
            m[field::VISITS] = format!("{:X}", r.visits.max(0));
            m[field::RATING] = format!("{:X}", r.rating.clamp(0, 10));
            m[field::CHECK_ENABLED] = if r.check_enabled { "1" } else { "0" }.into();
            if let Some(name) = &r.schedule {
                m[field::SCHEDULE] = pm_text(name);
            }
            let extras = [("keywords", &r.keywords), ("notes", &r.notes), ("desc", &r.description), ("user_field", &r.user_field)]
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| (k.to_string(), pm_text(v)))
                .collect();
            pm35::Entry { url: pm_text(&r.url), title: pm_text(&r.title), extras, mdata: m }
        })
        .collect();
    let macros: String = saved.iter().map(|i| format!("<!--macro={}-->\r\n", macro_record(i))).collect();
    let kwords: String = tagged.iter().map(|k| format!("<!--kword=[{}][1][0]-->\r\n", pm_text(k))).collect();
    let head = PM35_HEAD.replacen("</HEAD>", &format!("{macros}</HEAD>"), 1);
    let tail = PM35_TAIL.replacen("</DL><p>\r\n", &format!("</DL><p>\r\n{kwords}"), 1);
    pm35::write(&Database { head, entries, tail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browsers::read_netscape_html;

    use crate::{Entry, NewBookmark, ROOT};

    /// The tree under `folder`, depth-first, one line per folder or item with every field the
    /// round trips must keep. URL-less notes only `with_notes` (Netscape HTML has no place for them).
    fn tree_lines(s: &Store, folder: i64, with_notes: bool) -> Vec<String> {
        fn walk(s: &Store, folder: i64, depth: usize, with_notes: bool, out: &mut Vec<String>) {
            let pad = "  ".repeat(depth);
            for n in s.children(folder).unwrap() {
                match n {
                    crate::Node::Folder(f) => {
                        out.push(format!("{pad}[{}|{:?}|{}", f.name, f.note, f.added));
                        walk(s, f.id, depth + 1, with_notes, out);
                    }
                    crate::Node::Bookmark(b) if b.kind == "note" && !with_notes => {}
                    crate::Node::Bookmark(b) => out.push(format!(
                        "{pad}{}|{}|{}|{}|{:?}|{:?}|{:?}",
                        b.kind, b.url, b.title, b.keywords, b.description, b.notes, b.added
                    )),
                }
            }
        }
        let mut out = Vec::new();
        walk(s, folder, 0, with_notes, &mut out);
        out
    }

    /// Nested and empty folders, folder notes, interleaved order, notes, descriptions,
    /// keywords, markup and non-Latin text, and a URL-less note.
    fn tree_store() -> (Store, i64) {
        let mut s = Store::open(":memory:").unwrap();
        let b = |url: &str, title: &str, keywords: &str, description: &str, notes: &str, added: i64| {
            Entry::Bookmark(NewBookmark {
                url: url.into(),
                title: title.into(),
                keywords: keywords.into(),
                description: description.into(),
                notes: notes.into(),
                added: Some(added),
            })
        };
        let f = |name: &str, note: &str, added: i64| Entry::Folder { name: name.into(), note: note.into(), added: Some(added) };
        let entries = [
            b("https://top.example/", "Top & <first>", "alpha beta", "", "", 1_600_000_000),
            f("Dev", "dev stuff\nsecond line", 1_600_000_100),
            f("Rust", "", 1_600_000_200),
            b("https://rust-lang.org/", "Rust", "rust lang", "The Rust language", "my note\nline two", 1_600_000_300),
            Entry::Close,
            f("Empty", "", 1_600_000_400),
            Entry::Close,
            b("http://www.example.com/a?x=1&y=\"2\"", "Caf\u{e9} \u{1f600}", "", "only a description", "", 1_600_000_500),
            Entry::Close,
            b("https://after.example/", "After", "", "", "only notes", 1_600_000_600),
            f("Empty top", "a note on an empty folder", 1_600_000_700),
            Entry::Close,
        ];
        s.import_tree("fixture", &entries, ROOT, None, false).unwrap();
        let dev = s.children(ROOT).unwrap().into_iter().find_map(|n| match n {
            crate::Node::Folder(f) if f.name == "Dev" => Some(f.id),
            _ => None,
        });
        let dev = dev.unwrap();
        s.add_note(dev, "Shopping", "eggs and flour").unwrap().unwrap();
        (s, dev)
    }

    #[test]
    fn netscape_round_trips_the_tree() {
        let (s, _) = tree_store();
        let html = to_netscape(&s.export_tree(ROOT).unwrap());
        assert!(!html.contains("Shopping"), "URL-less notes are not links");
        let mut fresh = Store::open(":memory:").unwrap();
        fresh.import_tree("Import", &read_netscape_html(&html), ROOT, None, false).unwrap();
        let want = tree_lines(&s, ROOT, false);
        assert_eq!(want.len(), 8, "{want:#?}");
        assert_eq!(tree_lines(&fresh, ROOT, false), want, "same tree, order and fields");
    }

    #[test]
    fn json_round_trips_the_tree() {
        let (mut s, _) = tree_store();
        let id = s.search("Rust", 1).unwrap()[0].id;
        let mut b = s.get(id).unwrap().unwrap();
        (b.rating, b.user_field, b.visits) = (7, "uf".into(), 4);
        s.update(&b).unwrap();
        let tree = s.export_tree(ROOT).unwrap();
        let back = read_json(&to_json(&tree)).unwrap();
        assert_eq!(back, tree);
        let mut fresh = Store::open(":memory:").unwrap();
        fresh.import_tree("Import JSON", &back.entries(), ROOT, None, false).unwrap();
        assert_eq!(tree_lines(&fresh, ROOT, true), tree_lines(&s, ROOT, true), "notes too");
        let fields = |t: &Tree| t.rows_in_order().into_iter().map(|r| ExportRow { folder_id: 0, position: 0, ..r }).collect::<Vec<_>>();
        assert_eq!(fields(&fresh.export_tree(ROOT).unwrap()), fields(&tree), "every field");
    }

    #[test]
    fn folder_export_is_that_subtree() {
        let (s, dev) = tree_store();
        let tree = s.export_tree(dev).unwrap();
        assert_eq!(tree.rows.len(), 3, "Rust, the www link and the note");
        let mut fresh = Store::open(":memory:").unwrap();
        fresh.import_tree("Import", &read_netscape_html(&to_netscape(&tree)), ROOT, None, false).unwrap();
        assert_eq!(tree_lines(&fresh, ROOT, false), tree_lines(&s, dev, false));
        assert!(s.export_tree(9999).unwrap().rows.is_empty(), "missing folder: nothing");
    }

    #[test]
    fn browser_file_through_keymarks_and_back() {
        // What Chrome and Firefox write, folders nested and empty, a description as <DD>.
        let file = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1600000000" LAST_MODIFIED="1600000001" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
    <DL><p>
        <DT><A HREF="https://news.example/" ADD_DATE="1600000002" ICON="data:image/png;base64,AAAA">News</A>
        <DT><H3 ADD_DATE="1600000003">Work</H3>
        <DL><p>
            <DT><H3 ADD_DATE="1600000004">Empty</H3>
            <DL><p>
            </DL><p>
            <DT><A HREF="https://wiki.example/page?a=1&amp;b=2" ADD_DATE="1600000005" TAGS="docs,wiki">Wiki &amp; docs</A>
            <DD>Team wiki
        </DL><p>
    </DL><p>
    <DT><A HREF="https://loose.example/" ADD_DATE="1600000006">Loose</A>
</DL><p>
"#;
        let read = read_netscape_html(file);
        let mut s = Store::open(":memory:").unwrap();
        s.import_tree("Import", &read, ROOT, None, false).unwrap();
        let ours = to_netscape(&s.export_tree(ROOT).unwrap());
        assert_eq!(read_netscape_html(&ours), read, "nothing lost or changed through Keymarks");
        // The shape browsers expect: the doctype first, every <H3> opens a <DL>, lists balance.
        assert!(ours.starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>\n"));
        let lines: Vec<&str> = ours.lines().map(str::trim).filter(|l| !l.starts_with("<DD>")).collect();
        for (i, _) in lines.iter().enumerate().filter(|(_, l)| l.starts_with("<DT><H3")) {
            assert_eq!(lines[i + 1], "<DL><p>", "folder at line {i} opens its list");
        }
        assert_eq!(ours.matches("<DL><p>").count(), ours.matches("</DL><p>").count());
        assert_eq!(ours.matches("<DT><H3").count(), 3);
    }


    fn store() -> Store {
        let mut s = Store::open(":memory:").unwrap();
        let sample: &[u8] = b"<HTML><BODY><DL>\r\n\
<DT><A HREF=\"http://example.com/?a=1&amp;b=2\">Caf\xe9 &amp; Bar</A>\r\n<DD><!--keywords-->food paris\r\n<!--notes-->n1\r\n<!--mdata=[Monthly][634DF1F5][0][0][0][65DAAC26][C][1][1][ABCD][2][A][x][y][1]-->\r\n<BR>\r\n\
</DL><p>\r\n</BODY>\r\n</HTML>";
        s.import_pm35(&pm35::parse(sample).unwrap(), None).unwrap();
        s.add("https://new.example/\u{4e2d}", "Emoji \u{1f600} title", "tag1 tag2", "a \"quoted\" <desc>").unwrap();
        s
    }

    #[test]
    fn csv_quotes_and_picks_fields() {
        let s = store();
        let csv = to_csv(&export_rows_all(&s), &["description".into(), "url".into(), "added".into(), "bogus".into()]);
        let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').split("\r\n").collect();
        assert_eq!(lines[0], "URL,Description,Added", "EXPORT_FIELDS order, unknown dropped");
        assert!(lines[2].starts_with("https://new.example/\u{4e2d},\"a \"\"quoted\"\" <desc>\","), "RFC 4180 quoting");
        assert!(to_csv(&[], &[]).ends_with("Title,URL\r\n"), "no fields = title and URL");
        assert_eq!(datetime(Some(1_700_000_000)), "2023-11-14 22:13");
        assert_eq!(datetime(Some(0)), "1970-01-01 00:00");
    }

    #[test]
    fn printable_escapes() {
        let s = store();
        let page = to_printable(&export_rows_all(&s), &["title".into(), "description".into()], "My <list>");
        assert!(page.contains("<title>My &lt;list&gt;</title>") && page.contains("<p>2 bookmarks</p>"));
        assert!(page.contains("<td>a &quot;quoted&quot; &lt;desc&gt;</td>"));
    }

    #[test]
    fn json_round_trips_every_field() {
        let mut s = store();
        let id = s.search("Emoji", 1).unwrap()[0].id;
        let mut b = s.get(id).unwrap().unwrap();
        (b.notes, b.rating, b.user_field) = ("note".into(), 7, "uf".into());
        s.update(&b).unwrap();
        let rows = export_rows_all(&s);
        let back = read_json(&to_json(&Tree::flat(rows.clone()))).unwrap();
        assert_eq!(back.rows, rows);
        let mut fresh = Store::open(":memory:").unwrap();
        let report = fresh.import_tree("Import JSON", &back.entries(), ROOT, None, false).unwrap();
        assert_eq!((report.added, report.skipped), (2, 0));
        assert_eq!(export_rows_all(&fresh), rows, "every field, Powermarks record included");
        assert_eq!(fresh.import_tree("again", &back.entries(), ROOT, None, false).unwrap().skipped, 2);
        fresh.undo().unwrap();
        fresh.undo().unwrap();
        assert_eq!(fresh.count().unwrap(), 0);
        assert!(read_json("[]").is_err() && read_json(r#"{"format":"other","bookmarks":[]}"#).is_err());
    }

    #[test]
    fn netscape_round_trips_keywords() {
        let s = store();
        let rows = s.export_rows(None).unwrap();
        let html = to_netscape(&Tree::flat(rows));
        assert!(!html.contains("<H3"), "a flat list has no folders");
        let back = read_netscape_html(&html);
        let back = crate::bookmarks(&back);
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].title.as_str(), back[0].keywords.as_str()), ("Caf\u{e9} & Bar", "food paris"));
        // The description travels in Compass's <meta> part of the <DD>, so it comes back a description.
        assert_eq!((back[1].description.as_str(), back[1].notes.as_str()), ("a \"quoted\" <desc>", ""));
        assert_eq!(back[1].keywords, "tag1 tag2");
        assert_eq!(back[0].added, Some(0x634DF1F5));
    }

    #[test]
    fn pm35_export_keeps_unknown_fields_and_updates_known_ones() {
        let mut s = store();
        let id = s.search("food", 1).unwrap()[0].id;
        let mut b = s.get(id).unwrap().unwrap();
        b.rating = 3;
        s.update(&b).unwrap();
        s.mark_visited(id).unwrap();

        let bytes = to_pm35(&s.export_rows(None).unwrap());
        let db = pm35::parse(&bytes).expect("Powermarks can parse it");
        assert_eq!(db.entries.len(), 2);
        let e = &db.entries[0];
        assert_eq!(e.title, "Caf\u{e9} &amp; Bar"); // Latin-1 é stays a single byte
        assert_eq!(e.extra("keywords"), Some("food paris"));
        assert_eq!(e.extra("notes"), Some("n1"));
        assert_eq!(e.mdata[field::SCHEDULE], "Monthly", "unknown/kept fields survive");
        assert_eq!(e.mdata[field::PAGE_HASH], "ABCD");
        assert_eq!((e.mdata[12].as_str(), e.mdata[13].as_str()), ("x", "y"));
        assert_eq!(e.num(field::RATING), Some(3), "edited rating written");
        assert_eq!(e.num(field::VISITS), Some(3), "2 imported + 1 opened");
        assert!(e.num(field::LAST_VISITED).unwrap() > 0);

        let fresh = &db.entries[1];
        assert_eq!(fresh.url, "https://new.example/&#20013;");
        assert_eq!(fresh.title, "Emoji &#128512; title");
        assert_eq!(fresh.mdata.len(), 15);
        assert_eq!(fresh.mdata[field::SCHEDULE], "Weekly");
        assert_eq!(fresh.extra("desc"), Some("a &quot;quoted&quot; &lt;desc&gt;"));

        // And it imports back into an empty Keymarks with the same text.
        let mut again = Store::open(":memory:").unwrap();
        again.import_pm35(&db, None).unwrap();
        let titles: Vec<_> = again.search("", 10).unwrap().into_iter().map(|b| b.title).collect();
        assert!(titles.contains(&"Emoji \u{1f600} title".to_string()), "{titles:?}");
    }

    #[test]
    fn pm35_export_carries_saved_items_and_tags_back() {
        let mut s = store();
        s.set_tagged("food", true).unwrap();
        for (kind, name, query, options, url) in [
            ("link", "Docs", "", "{}", "https://docs.example/"),
            ("search", "Rusty", "rust ! old", r#"{"mode":"keywords","partial":true}"#, ""),
            ("search", "Exact", "Caf\u{e9}", r#"{"mode":"exact","match_case":true,"fields":["notes","url"]}"#, ""),
        ] {
            s.save_item(&SavedItem { id: 0, name: name.into(), kind: kind.into(), query: query.into(), options: options.into(), url: url.into() }).unwrap();
        }
        let db = pm35::parse(&s.export_pm35(&s.export_rows(None).unwrap()).unwrap()).unwrap();
        let mut again = Store::open(":memory:").unwrap();
        again.import_pm35(&db, None).unwrap();
        let back = again.saved_items().unwrap();
        assert_eq!(back.iter().map(|i| (i.name.as_str(), i.kind.as_str(), i.query.as_str(), i.url.as_str())).collect::<Vec<_>>(),
            [("Docs", "link", "", "https://docs.example/"), ("Rusty", "search", "rust ! old", ""), ("Exact", "search", "Caf\u{e9}", "")]);
        assert!(back[2].options.contains("\"match_case\":true"));
        assert!(back[2].options.contains(r#""fields":["url","notes"]"#), "field mask round-trips");
        assert!(again.keywords("").unwrap().iter().any(|k| k.keyword == "food" && k.tagged));
    }

    #[test]
    fn export_selected_keeps_order() {
        let s = store();
        let ids: Vec<i64> = s.search("", 10).unwrap().into_iter().map(|b| b.id).collect();
        let rows = s.export_rows(Some(&[ids[1], ids[0]])).unwrap();
        assert_eq!(rows.iter().map(|r| r.url.clone()).collect::<Vec<_>>(), [s.get(ids[1]).unwrap().unwrap().url, s.get(ids[0]).unwrap().unwrap().url]);
    }
}
