//! Writing bookmarks out: Netscape bookmark HTML (any browser imports it) and Powermarks 3.5
//! `pm35.htm` (the original program can open it again).

use crate::{Result, SavedItem, Store};
use pm35::{Database, Entry, field};
use rusqlite::params;

/// Everything an export needs about one bookmark.
#[derive(Debug, Clone, PartialEq, Default)]
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
}

impl Store {
    /// Rows to export: the given ids in that order, or every bookmark (oldest first).
    pub fn export_rows(&self, ids: Option<&[i64]>) -> Result<Vec<ExportRow>> {
        let cols = "url, title, keywords, description, notes, user_field, added, last_visited, last_modified, \
                    last_checked, visits, rating, check_enabled, pm_mdata, schedule";
        let map = |r: &rusqlite::Row| {
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
            })
        };
        match ids {
            None => self.conn.prepare(&format!("SELECT {cols} FROM bookmark ORDER BY coalesce(added, 0), id"))?.query_map([], map)?.collect(),
            Some(ids) => {
                let json = serde_json::to_string(ids).expect("ids serialize");
                // json_each keeps the requested order via its key.
                self.conn
                    .prepare(&format!(
                        "SELECT {cols} FROM json_each(?1) j JOIN bookmark b ON b.id = j.value ORDER BY j.key"
                    ))?
                    .query_map(params![json], map)?
                    .collect()
            }
        }
    }
}

impl Store {
    /// A Powermarks file of `ids` (or everything), with Saved items as macros and tagged
    /// keywords, so Powermarks 3.5 gets those back too.
    pub fn export_pm35(&self, ids: Option<&[i64]>) -> Result<Vec<u8>> {
        let tagged: Vec<String> = self.conn.prepare("SELECT keyword FROM tagged_keyword ORDER BY keyword")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
        Ok(to_pm35_with(&self.export_rows(ids)?, &self.saved_items()?, &tagged))
    }
}

#[cfg(test)]
pub(crate) fn export_rows_all(s: &Store) -> Vec<ExportRow> {
    s.export_rows(None).unwrap()
}

fn html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Netscape bookmark HTML. No folders: keywords travel as Firefox-style TAGS, so importing
/// the file back into Keymarks gives the same keywords.
pub fn to_netscape(rows: &[ExportRow]) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <!-- This is an automatically generated file. Exported by Keymarks. -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    for r in rows {
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
        out += &format!("    <DT><A {attrs}>{}</A>\n", html(&r.title));
        if !r.description.is_empty() {
            out += &format!("    <DD>{}\n", html(&r.description));
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
        (_, Some("exact")) => format!("[2][{name}][0][0][{}][{}][1F]", pm_text(&item.query), flag("match_case")),
        _ => format!("[1][{name}][0][0][{}][{}][0]", pm_text(&item.query), flag("partial")),
    }
}

pub fn to_pm35_with(rows: &[ExportRow], saved: &[SavedItem], tagged: &[String]) -> Vec<u8> {
    let hex = |t: Option<i64>| format!("{:X}", t.unwrap_or(0).clamp(0, u32::MAX as i64));
    let entries = rows
        .iter()
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
            Entry { url: pm_text(&r.url), title: pm_text(&r.title), extras, mdata: m }
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

    fn store() -> Store {
        let mut s = Store::open(":memory:").unwrap();
        let sample: &[u8] = b"<HTML><BODY><DL>\r\n\
<DT><A HREF=\"http://example.com/?a=1&amp;b=2\">Caf\xe9 &amp; Bar</A>\r\n<DD><!--keywords-->food paris\r\n<!--notes-->n1\r\n<!--mdata=[Monthly][634DF1F5][0][0][0][65DAAC26][C][1][1][ABCD][2][A][x][y][1]-->\r\n<BR>\r\n\
</DL><p>\r\n</BODY>\r\n</HTML>";
        s.import_pm35(&pm35::parse(sample).unwrap()).unwrap();
        s.add("https://new.example/\u{4e2d}", "Emoji \u{1f600} title", "tag1 tag2", "a \"quoted\" <desc>").unwrap();
        s
    }

    #[test]
    fn netscape_round_trips_keywords() {
        let s = store();
        let rows = s.export_rows(None).unwrap();
        let back = read_netscape_html(&to_netscape(&rows));
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].title.as_str(), back[0].keywords.as_str()), ("Caf\u{e9} & Bar", "food paris"));
        assert_eq!(back[1].description, "a \"quoted\" <desc>");
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
        again.import_pm35(&db).unwrap();
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
            ("search", "Exact", "Caf\u{e9}", r#"{"mode":"exact","match_case":true}"#, ""),
        ] {
            s.save_item(&SavedItem { id: 0, name: name.into(), kind: kind.into(), query: query.into(), options: options.into(), url: url.into() }).unwrap();
        }
        let db = pm35::parse(&s.export_pm35(None).unwrap()).unwrap();
        let mut again = Store::open(":memory:").unwrap();
        again.import_pm35(&db).unwrap();
        let back = again.saved_items().unwrap();
        assert_eq!(back.iter().map(|i| (i.name.as_str(), i.kind.as_str(), i.query.as_str(), i.url.as_str())).collect::<Vec<_>>(),
            [("Docs", "link", "", "https://docs.example/"), ("Rusty", "search", "rust ! old", ""), ("Exact", "search", "Caf\u{e9}", "")]);
        assert!(back[2].options.contains("\"match_case\":true"));
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
