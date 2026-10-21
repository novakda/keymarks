//! SQLite bookmark store with full-text keyword search.
//! Imports Powermarks 3.5 databases (field meanings: docs/pm35-format.md).

use pm35::{Database, field};
use rusqlite::{Connection, OptionalExtension, params};

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
CREATE INDEX IF NOT EXISTS bookmark_url ON bookmark(url);
-- Derived from bookmark.keywords (the source of truth) by Store::index_keywords.
CREATE TABLE IF NOT EXISTS bookmark_keyword (
    bookmark_id INTEGER NOT NULL,
    keyword TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY (bookmark_id, keyword)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS bookmark_keyword_kw ON bookmark_keyword(keyword);
CREATE TRIGGER IF NOT EXISTS bookmark_kw_ad AFTER DELETE ON bookmark BEGIN
    DELETE FROM bookmark_keyword WHERE bookmark_id = old.id;
END;
-- Powermarks 'tagged keywords': a curated favourite vocabulary.
CREATE TABLE IF NOT EXISTS tagged_keyword (keyword TEXT PRIMARY KEY COLLATE NOCASE);
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

/// FTS5 bm25 weights per column (title, keywords, description, notes, url):
/// keyword hits rank first, as Powermarks searched keywords by default.
const RANK: &str = "bm25(bookmark_fts, 2.0, 10.0, 1.0, 1.0, 0.5)";
const COLS: &str = "b.id, b.url, b.title, b.keywords, b.description, b.notes, b.rating, b.added, b.last_visited, b.visits";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub keywords: String,
    pub description: String,
    pub notes: String,
    pub rating: i64,
    pub added: Option<i64>,
    pub last_visited: Option<i64>,
    pub visits: i64,
}

fn bookmark(r: &rusqlite::Row) -> Result<Bookmark> {
    Ok(Bookmark {
        id: r.get(0)?,
        url: r.get(1)?,
        title: r.get(2)?,
        keywords: r.get(3)?,
        description: r.get(4)?,
        notes: r.get(5)?,
        rating: r.get(6)?,
        added: r.get(7)?,
        last_visited: r.get(8)?,
        visits: r.get(9)?,
    })
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open or create a store. Use ":memory:" for a throwaway one.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        // The app and the browser's native-messaging host share this file.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        let mut store = Store { conn };
        // v1: keyword index. Backfill databases created before it existed.
        let version: i64 = store.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            let tx = store.conn.transaction()?;
            let rows: Vec<(i64, String)> = tx
                .prepare("SELECT id, keywords FROM bookmark")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_>>()?;
            for (id, keywords) in rows {
                index_keywords(&tx, id, &keywords)?;
            }
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        Ok(store)
    }

    pub fn count(&self) -> Result<i64> {
        self.conn.query_row("SELECT count(*) FROM bookmark", [], |r| r.get(0))
    }

    /// Add a bookmark unless its URL is already stored. Returns (id, already_existed).
    pub fn add(&self, url: &str, title: &str, keywords: &str, description: &str) -> Result<(i64, bool)> {
        let existing = self
            .conn
            .query_row("SELECT id FROM bookmark WHERE url = ?1", [url], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            return Ok((id, true));
        }
        let keywords = normalize_keywords(keywords);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO bookmark (url, title, keywords, description, added)
             VALUES (?1, ?2, ?3, ?4, unixepoch())",
            params![url, title, keywords, description],
        )?;
        let id = tx.last_insert_rowid();
        index_keywords(&tx, id, &keywords)?;
        tx.commit()?;
        Ok((id, false))
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
                let keywords = text("keywords");
                insert.execute(params![
                    decode_text(&e.url),
                    decode_text(&e.title),
                    keywords,
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
                index_keywords(&tx, tx.last_insert_rowid(), &keywords)?;
            }
        }
        tx.commit()?;
        Ok(db.entries.len())
    }

    /// Powermarks-style search: every word must prefix-match somewhere (title, keywords,
    /// description, notes or URL), keyword matches ranked first. Empty query lists newest first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Bookmark>> {
        let Some(m) = fts_query(query) else {
            let sql = format!("SELECT {COLS} FROM bookmark b ORDER BY b.added DESC LIMIT ?1");
            return self.conn.prepare(&sql)?.query_map(params![limit as i64], bookmark)?.collect();
        };
        let sql = format!(
            "SELECT {COLS} FROM bookmark_fts f JOIN bookmark b ON b.id = f.rowid
             WHERE bookmark_fts MATCH ?2 ORDER BY {RANK} LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.query_map(params![limit as i64, m], bookmark)?.collect()
    }

    /// The keyword dictionary, narrowed to bookmarks matching `query` (all when empty),
    /// alphabetical, with how many of those bookmarks use each keyword.
    pub fn keywords(&self, query: &str) -> Result<Vec<KeywordCount>> {
        let filter = if fts_query(query).is_some() {
            "WHERE k.bookmark_id IN (SELECT rowid FROM bookmark_fts WHERE bookmark_fts MATCH ?1)"
        } else {
            "WHERE ?1 IS NULL"
        };
        let sql = format!(
            "SELECT k.keyword, count(*), k.keyword IN (SELECT keyword FROM tagged_keyword)
             FROM bookmark_keyword k {filter} GROUP BY k.keyword ORDER BY k.keyword"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([fts_query(query)], |r| {
            Ok(KeywordCount { keyword: r.get(0)?, count: r.get(1)?, tagged: r.get(2)? })
        })?;
        rows.collect()
    }

    pub fn set_tagged(&self, keyword: &str, tagged: bool) -> Result<()> {
        let sql = if tagged {
            "INSERT OR IGNORE INTO tagged_keyword (keyword) VALUES (?1)"
        } else {
            "DELETE FROM tagged_keyword WHERE keyword = ?1"
        };
        self.conn.execute(sql, [keyword])?;
        Ok(())
    }

    /// Rename a keyword on every bookmark (case-insensitive). Renaming onto an existing
    /// keyword merges them; an empty `new` deletes the keyword. Returns bookmarks changed.
    pub fn rename_keyword(&mut self, old: &str, new: &str) -> Result<usize> {
        let new = normalize_keywords(new);
        let old_lower = old.to_lowercase();
        let tx = self.conn.transaction()?;
        let rows: Vec<(i64, String)> = tx
            .prepare(
                "SELECT b.id, b.keywords FROM bookmark b
                 JOIN bookmark_keyword k ON k.bookmark_id = b.id WHERE k.keyword = ?1",
            )?
            .query_map([old], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;
        for (id, keywords) in &rows {
            let replaced: Vec<&str> = keywords
                .split(|c: char| c.is_whitespace() || c == ',')
                .map(|k| if k.to_lowercase() == old_lower { new.as_str() } else { k })
                .collect();
            let keywords = normalize_keywords(&replaced.join(" "));
            tx.execute("UPDATE bookmark SET keywords = ?2 WHERE id = ?1", params![id, keywords])?;
            index_keywords(&tx, *id, &keywords)?;
        }
        let was_tagged = tx.execute("DELETE FROM tagged_keyword WHERE keyword = ?1", [old])? > 0;
        if was_tagged && !new.is_empty() && !new.contains(' ') {
            tx.execute("INSERT OR IGNORE INTO tagged_keyword (keyword) VALUES (?1)", [&new])?;
        }
        tx.commit()?;
        Ok(rows.len())
    }

    pub fn get(&self, id: i64) -> Result<Option<Bookmark>> {
        let sql = format!("SELECT {COLS} FROM bookmark b WHERE b.id = ?1");
        self.conn.query_row(&sql, [id], bookmark).optional()
    }

    /// Save the user-editable fields. Returns false if the bookmark no longer exists.
    pub fn update(&self, b: &Bookmark) -> Result<bool> {
        let keywords = normalize_keywords(&b.keywords);
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE bookmark SET url = ?2, title = ?3, keywords = ?4, description = ?5,
                notes = ?6, rating = ?7 WHERE id = ?1",
            params![b.id, b.url, b.title, keywords, b.description, b.notes, b.rating.clamp(0, 10)],
        )?;
        if n == 1 {
            index_keywords(&tx, b.id, &keywords)?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    pub fn delete(&mut self, ids: &[i64]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for id in ids {
            n += tx.execute("DELETE FROM bookmark WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Record that a bookmark was opened (usage count and last-visited, as Powermarks did).
    pub fn mark_visited(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE bookmark SET visits = visits + 1, last_visited = unixepoch() WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Changes whenever another connection (e.g. the browser host) commits to the database.
    pub fn data_version(&self) -> Result<i64> {
        self.conn.pragma_query_value(None, "data_version", |r| r.get(0))
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct KeywordCount {
    pub keyword: String,
    pub count: i64,
    pub tagged: bool,
}

/// FTS5 query for Powermarks-style search: every word prefix-matches. None for an empty query.
fn fts_query(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// Keywords as stored: split on whitespace/commas, duplicates dropped (case-insensitive,
/// first spelling wins), order kept, single-space separated.
pub fn normalize_keywords(s: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|k| !k.is_empty() && seen.insert(k.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn index_keywords(conn: &Connection, id: i64, keywords: &str) -> Result<()> {
    conn.execute("DELETE FROM bookmark_keyword WHERE bookmark_id = ?1", [id])?;
    let mut insert = conn.prepare_cached("INSERT OR IGNORE INTO bookmark_keyword VALUES (?1, ?2)")?;
    for k in keywords.split(|c: char| c.is_whitespace() || c == ',').filter(|k| !k.is_empty()) {
        insert.execute(params![id, k])?;
    }
    Ok(())
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

    #[test]
    fn add_is_searchable_and_deduplicated() {
        let s = store();
        let (id, existed) = s.add("https://rust-lang.org/", "Rust", "programming language", "").unwrap();
        assert!(!existed);
        assert_eq!(s.add("https://rust-lang.org/", "Again", "", "").unwrap(), (id, true));
        let hits = s.search("program", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].added.is_some());
        assert_eq!(s.count().unwrap(), 3);
    }

    #[test]
    fn keyword_hits_rank_first() {
        let s = store();
        s.add("https://x.example/", "Museum guide", "travel", "").unwrap(); // "museum" in title only
        let hits = s.search("museum", 10).unwrap();
        assert_eq!(hits[0].title, "It\u{2019}s UTF-8"); // "museum" in keywords
    }

    #[test]
    fn edit_delete_visit_and_change_detection() {
        let path = std::env::temp_dir().join(format!("store-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut a = Store::open(path.to_str().unwrap()).unwrap();
        let b = Store::open(path.to_str().unwrap()).unwrap(); // second connection, like the host
        let (id, _) = a.add("https://a.example/", "A", "old", "").unwrap();

        let mut bm = a.get(id).unwrap().unwrap();
        bm.keywords = "new words".into();
        bm.rating = 42;
        assert!(a.update(&bm).unwrap());
        assert_eq!(a.get(id).unwrap().unwrap().rating, 10);
        assert!(a.search("old", 5).unwrap().is_empty());
        assert_eq!(a.search("words", 5).unwrap().len(), 1);

        a.mark_visited(id).unwrap();
        assert_eq!(a.get(id).unwrap().unwrap().visits, 1);

        let v = a.data_version().unwrap();
        b.add("https://b.example/", "B", "", "").unwrap();
        assert_ne!(a.data_version().unwrap(), v, "other connection's commit not detected");

        assert_eq!(a.delete(&[id, 9999]).unwrap(), 1);
        assert!(a.get(id).unwrap().is_none());
        assert!(a.search("words", 5).unwrap().is_empty());
        drop((a, b));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }

    #[test]
    fn keyword_dictionary_narrows_renames_and_tags() {
        let mut s = store(); // "food paris" and "paris museum"
        let kw = |s: &Store, q| s.keywords(q).unwrap().into_iter().map(|k| (k.keyword, k.count)).collect::<Vec<_>>();
        assert_eq!(kw(&s, ""), [("food".into(), 1), ("museum".into(), 1), ("paris".into(), 2)]);
        assert_eq!(kw(&s, "museum"), [("museum".into(), 1), ("paris".into(), 1)]);

        s.set_tagged("Paris", true).unwrap();
        assert!(s.keywords("").unwrap().iter().any(|k| k.keyword == "paris" && k.tagged));

        // Rename is case-insensitive and the tag follows.
        assert_eq!(s.rename_keyword("PARIS", "France").unwrap(), 2);
        assert_eq!(kw(&s, ""), [("food".into(), 1), ("France".into(), 2), ("museum".into(), 1)]);
        assert!(s.keywords("").unwrap().iter().any(|k| k.keyword == "France" && k.tagged));

        // Renaming onto an existing keyword merges and dedupes.
        assert_eq!(s.rename_keyword("france", "museum").unwrap(), 2);
        assert_eq!(kw(&s, ""), [("food".into(), 1), ("museum".into(), 2)]);
        let mut kws: Vec<_> = s.search("museum", 5).unwrap().into_iter().map(|b| b.keywords).collect();
        kws.sort();
        assert_eq!(kws, ["food museum", "museum"]);

        assert_eq!(s.rename_keyword("food", "").unwrap(), 1); // delete
        assert!(kw(&s, "").iter().all(|(k, _)| k != "food"));
        assert!(s.search("food", 5).unwrap().is_empty());

        let (id, _) = s.add("https://k.example/", "K", "a, b a  B c", "").unwrap();
        assert_eq!(s.get(id).unwrap().unwrap().keywords, "a b c");
        s.delete(&[id]).unwrap();
        assert!(kw(&s, "").iter().all(|(k, _)| k != "c"));
    }

    #[test]
    fn keyword_index_backfills_old_databases() {
        let path = std::env::temp_dir().join(format!("store-mig-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let p = path.to_str().unwrap();
        Store::open(p).unwrap().add("https://m.example/", "M", "x y", "").unwrap();
        {
            // Simulate a database from before the keyword index.
            let c = Connection::open(p).unwrap();
            c.execute_batch("DELETE FROM bookmark_keyword; PRAGMA user_version = 0;").unwrap();
        }
        assert_eq!(Store::open(p).unwrap().keywords("").unwrap().len(), 2);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{ext}"));
        }
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
        for q in ["", "a"] {
            let t = std::time::Instant::now();
            let n = s.keywords(q).unwrap().len();
            eprintln!("keywords({q:?}): {n} in {:?}", t.elapsed());
        }
    }
}
