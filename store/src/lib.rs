//! SQLite bookmark store with full-text keyword search.
//! Imports Powermarks 3.5 databases (field meanings: docs/pm35-format.md).

use pm35::{Database, field};
use rusqlite::{Connection, params};

pub use rusqlite::Result;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS bookmark (
    id INTEGER PRIMARY KEY,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    keywords TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '',
    user_field TEXT NOT NULL DEFAULT '',
    added INTEGER,
    last_visited INTEGER,
    last_modified INTEGER,
    last_checked INTEGER,
    visits INTEGER NOT NULL DEFAULT 0,
    rating INTEGER NOT NULL DEFAULT 0,
    status INTEGER NOT NULL DEFAULT 0,
    pm_mdata TEXT -- raw Powermarks mdata fields joined by '][', kept for lossless export
);
CREATE VIRTUAL TABLE IF NOT EXISTS bookmark_fts USING fts5(
    title, keywords, description, notes, url,
    content='bookmark', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS bookmark_ai AFTER INSERT ON bookmark BEGIN
    INSERT INTO bookmark_fts(rowid, title, keywords, description, notes, url)
    VALUES (new.id, new.title, new.keywords, new.description, new.notes, new.url);
END;
CREATE TRIGGER IF NOT EXISTS bookmark_ad AFTER DELETE ON bookmark BEGIN
    INSERT INTO bookmark_fts(bookmark_fts, rowid, title, keywords, description, notes, url)
    VALUES ('delete', old.id, old.title, old.keywords, old.description, old.notes, old.url);
END;
CREATE TRIGGER IF NOT EXISTS bookmark_au AFTER UPDATE ON bookmark BEGIN
    INSERT INTO bookmark_fts(bookmark_fts, rowid, title, keywords, description, notes, url)
    VALUES ('delete', old.id, old.title, old.keywords, old.description, old.notes, old.url);
    INSERT INTO bookmark_fts(rowid, title, keywords, description, notes, url)
    VALUES (new.id, new.title, new.keywords, new.description, new.notes, new.url);
END;
";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub keywords: String,
    pub description: String,
    pub rating: i64,
    pub added: Option<i64>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open or create a store. Use ":memory:" for a throwaway one.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn count(&self) -> Result<i64> {
        self.conn.query_row("SELECT count(*) FROM bookmark", [], |r| r.get(0))
    }

    /// Import every Powermarks entry in one transaction. Returns the number imported.
    pub fn import_pm35(&mut self, db: &Database) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO bookmark (url, title, keywords, description, notes, user_field,
                    added, last_visited, last_modified, last_checked, visits, rating, status, pm_mdata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for e in &db.entries {
                let text = |name| e.extra(name).map(decode_text).unwrap_or_default();
                let time = |i| e.num(i).filter(|&t| t != 0);
                let num = |i| e.num(i).unwrap_or(0);
                insert.execute(params![
                    decode_text(&e.url),
                    decode_text(&e.title),
                    text("keywords"),
                    text("desc"),
                    text("notes"),
                    text("user_field"),
                    time(field::ADDED),
                    time(field::LAST_VISITED),
                    time(field::LAST_MODIFIED),
                    time(field::LAST_CHECKED),
                    num(field::VISITS),
                    num(field::RATING),
                    num(field::STATUS),
                    e.mdata.join("]["),
                ])?;
            }
        }
        tx.commit()?;
        Ok(db.entries.len())
    }

    /// Powermarks-style search: every word must prefix-match somewhere (title, keywords,
    /// description, notes or URL). Empty query lists newest first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Bookmark>> {
        let cols = "b.id, b.url, b.title, b.keywords, b.description, b.rating, b.added";
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
            .collect();
        let (sql, arg) = if words.is_empty() {
            (format!("SELECT {cols} FROM bookmark b ORDER BY b.added DESC LIMIT ?1"), None)
        } else {
            (
                format!(
                    "SELECT {cols} FROM bookmark_fts f JOIN bookmark b ON b.id = f.rowid
                     WHERE bookmark_fts MATCH ?2 ORDER BY f.rank LIMIT ?1"
                ),
                Some(words.join(" ")),
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let row = |r: &rusqlite::Row| {
            Ok(Bookmark {
                id: r.get(0)?,
                url: r.get(1)?,
                title: r.get(2)?,
                keywords: r.get(3)?,
                description: r.get(4)?,
                rating: r.get(5)?,
                added: r.get(6)?,
            })
        };
        let rows = match arg {
            None => stmt.query_map(params![limit as i64], row)?,
            Some(m) => stmt.query_map(params![limit as i64, m], row)?,
        };
        rows.collect()
    }
}

/// Raw pm35 text (one char per file byte, HTML entities) to display text.
/// Powermarks files mix encodings per field: newer entries are UTF-8, older ones Windows-1252.
pub fn decode_text(raw: &str) -> String {
    let bytes: Vec<u8> = raw.chars().map(|c| c as u32 as u8).collect();
    let s = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().into_iter().map(cp1252).collect(),
    };
    html_escape::decode_html_entities(&s).into_owned()
}

const CP1252_80: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

fn cp1252(b: u8) -> char {
    match b {
        0x80..=0x9f => CP1252_80[(b - 0x80) as usize],
        _ => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"<HTML><BODY><DL>\r\n\
<DT><A HREF=\"http://example.com/?a=1&amp;b=2\">Caf\xe9 &amp; Bar \x92s</A>\r\n<DD><!--keywords-->food paris\r\n<!--desc-->Nice\r\n<!--mdata=[Weekly][634DF1F5][0][0][0][65DAAC26][0][0][1][][0][A][][][0]-->\r\n<BR>\r\n\
<DT><A HREF=\"http://example.com/u\">It\xe2\x80\x99s UTF-8</A>\r\n<DD><!--keywords-->paris museum\r\n<!--mdata=[Weekly][634DF3B9][0][0][0][65DAAC26][0][0][1][][0][0][][][0]-->\r\n<BR>\r\n\
</DL><p>\r\n</BODY>\r\n</HTML>";

    fn store() -> Store {
        let mut s = Store::open(":memory:").unwrap();
        assert_eq!(s.import_pm35(&pm35::parse(SAMPLE).unwrap()).unwrap(), 2);
        s
    }

    #[test]
    fn decodes_mixed_encodings_and_entities() {
        let all = store().search("", 10).unwrap();
        assert_eq!(all[0].title, "It\u{2019}s UTF-8"); // newest first
        assert_eq!(all[1].title, "Caf\u{e9} & Bar \u{2019}s");
        assert_eq!(all[1].url, "http://example.com/?a=1&b=2");
        assert_eq!(all[1].rating, 10);
        assert_eq!(all[1].added, Some(0x634DF1F5));
    }

    #[test]
    fn search_is_prefix_and_all_words() {
        let s = store();
        let titles = |q| s.search(q, 10).unwrap().into_iter().map(|b| b.title).collect::<Vec<_>>();
        assert_eq!(titles("par").len(), 2);
        assert_eq!(titles("paris food"), vec!["Caf\u{e9} & Bar \u{2019}s"]);
        assert_eq!(titles("cafe"), vec!["Caf\u{e9} & Bar \u{2019}s"]); // diacritics folded
        assert!(titles("paris zzz").is_empty());
        assert_eq!(titles("\"quoted").len(), 0); // no FTS syntax error
    }

    /// PM35_REAL=/path/to/pm35.htm cargo test -p store -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_file_import() {
        let bytes = std::fs::read(std::env::var("PM35_REAL").expect("set PM35_REAL")).unwrap();
        let db = pm35::parse(&bytes).unwrap();
        let mut s = Store::open(":memory:").unwrap();
        let t = std::time::Instant::now();
        let n = s.import_pm35(&db).unwrap();
        eprintln!("imported {n} in {:?}", t.elapsed());
        assert_eq!(n, db.entries.len());
        let t = std::time::Instant::now();
        let hits = s.search("a", 50).unwrap().len();
        eprintln!("search 'a': {hits} hits in {:?}", t.elapsed());
        let mojibake = s.search("", usize::MAX).unwrap().iter().filter(|b| b.title.contains('\u{fffd}') || b.title.contains("\u{e2}\u{80}")).count();
        eprintln!("titles with leftover mojibake: {mojibake}");
    }
}
