//! SQLite bookmark store with full-text keyword search.
//! Imports Powermarks 3.5 databases (field meanings: docs/pm35-format.md).

mod history;
mod query;

use pm35::{Database, field};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

pub use query::{FIELDS, Mode, SearchOptions, SortKey};
pub use rusqlite::Result;

/// A search that couldn't run: bad query syntax (show it to the user) or a database error.
#[derive(Debug)]
pub enum SearchError {
    Syntax(String),
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for SearchError {
    fn from(e: rusqlite::Error) -> Self {
        SearchError::Db(e)
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SearchError::Syntax(m) => write!(f, "{m}"),
            SearchError::Db(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SearchError {}

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
const COLS: &str = "b.id, b.url, b.title, b.keywords, b.description, b.notes, b.rating, b.added, \
                    b.last_visited, b.visits, b.last_modified, b.last_checked, b.status";

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
    #[serde(default)]
    pub last_modified: Option<i64>,
    #[serde(default)]
    pub last_checked: Option<i64>,
    #[serde(default)]
    pub status: i64,
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
        last_modified: r.get(10)?,
        last_checked: r.get(11)?,
        status: r.get(12)?,
    })
}

pub struct Store {
    conn: Connection,
    history: history::History,
}

impl Store {
    /// Open or create a store. Use ":memory:" for a throwaway one.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        // The app and the browser's native-messaging host share this file.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Store { conn, history: Default::default() };
        store.migrate()?;
        Ok(store)
    }

    /// Bring the schema and data up to date (also after restoring an older backup).
    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        let store = self;
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
        // v2: repair text that was already double/triple UTF-8-encoded in the source file.
        if version < 2 {
            let tx = store.conn.transaction()?;
            let rows: Vec<[String; 5]> = tx
                .prepare("SELECT id, title, keywords, description, notes FROM bookmark")?
                .query_map([], |r| Ok([r.get::<_, i64>(0)?.to_string(), r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))?
                .collect::<Result<_>>()?;
            for [id, title, keywords, description, notes] in rows {
                let fixed = [&title, &keywords, &description, &notes].map(|t| fix_mojibake(t));
                if fixed != [&title, &keywords, &description, &notes].map(|t| t.to_string()) {
                    tx.execute(
                        "UPDATE bookmark SET title = ?2, keywords = ?3, description = ?4, notes = ?5 WHERE id = ?1",
                        params![id, fixed[0], fixed[1], fixed[2], fixed[3]],
                    )?;
                    index_keywords(&tx, id.parse().unwrap(), &fixed[1])?;
                }
            }
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        Ok(())
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

    /// Search with default options (all fields, prefix matching, keyword hits first).
    pub fn search(&self, query: &str, limit: usize) -> std::result::Result<Vec<Bookmark>, SearchError> {
        self.search_with(query, &SearchOptions::default(), limit)
    }

    /// Powermarks-style search (syntax: see `query`). Empty query lists newest first.
    pub fn search_with(
        &self,
        query: &str,
        opts: &SearchOptions,
        limit: usize,
    ) -> std::result::Result<Vec<Bookmark>, SearchError> {
        let (sql, params) = self.search_sql(COLS, query, opts, limit)?;
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt.query_map(params_from_iter(params), bookmark)?.collect::<Result<_>>()?)
    }

    /// Every matching bookmark id, in display order (for virtual scrolling).
    pub fn search_ids(&self, query: &str, opts: &SearchOptions) -> std::result::Result<Vec<i64>, SearchError> {
        let (sql, params) = self.search_sql("b.id", query, opts, usize::MAX)?;
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt.query_map(params_from_iter(params), |r| r.get(0))?.collect::<Result<_>>()?)
    }

    /// Bookmarks by id, in the order given; ids that no longer exist are skipped.
    pub fn get_many(&self, ids: &[i64]) -> Result<Vec<Bookmark>> {
        let json = serde_json::to_string(ids).expect("ids serialize");
        let sql = format!("SELECT {COLS} FROM bookmark b WHERE b.id IN (SELECT value FROM json_each(?1))");
        let mut found: std::collections::HashMap<i64, Bookmark> = self
            .conn
            .prepare(&sql)?
            .query_map([json], bookmark)?
            .map(|r| r.map(|b| (b.id, b)))
            .collect::<Result<_>>()?;
        Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
    }

    fn search_sql(
        &self,
        cols: &str,
        query: &str,
        opts: &SearchOptions,
        limit: usize,
    ) -> std::result::Result<(String, Vec<Value>), SearchError> {
        let filter = query::compile(query, opts).map_err(SearchError::Syntax)?;
        let mut params: Vec<Value> = Vec::new();
        let (with, join, cond, default_order) = match &filter {
            None => (String::new(), "", "1".to_string(), "b.added DESC"),
            Some(f) => {
                // MATERIALIZED: rank the matches once, not once per bookmark row.
                let (with, join) = match &f.rank {
                    Some(m) => {
                        params.push(m.clone().into());
                        (
                            format!(
                                "WITH ranked AS MATERIALIZED (SELECT rowid AS id, {RANK} AS r
                                 FROM bookmark_fts WHERE bookmark_fts MATCH ?)"
                            ),
                            "LEFT JOIN ranked f ON f.id = b.id",
                        )
                    }
                    None => (String::new(), ""),
                };
                params.extend(f.params.iter().cloned().map(Value::from));
                let order = if f.rank.is_some() { "f.r IS NULL, f.r, b.added DESC" } else { "b.added DESC" };
                (with, join, f.sql.clone(), order)
            }
        };
        params.push(Value::Integer(limit.min(i64::MAX as usize) as i64));
        // Chosen column sorts first; relevance / newest break the remaining ties.
        let mut order: Vec<String> = opts.sort.iter().filter_map(SortKey::sql).collect();
        order.push(default_order.to_string());
        let order = order.join(", ");
        let sql = format!("{with} SELECT {cols} FROM bookmark b {join} WHERE {cond} ORDER BY {order} LIMIT ?");
        Ok((sql, params))
    }

    /// The whole keyword dictionary, alphabetical, with use counts.
    pub fn keywords(&self, query: &str) -> std::result::Result<Vec<KeywordCount>, SearchError> {
        self.keywords_with(query, &SearchOptions::default())
    }

    /// The keyword dictionary narrowed to bookmarks matching `query` (all when empty),
    /// alphabetical, with how many of those bookmarks use each keyword.
    pub fn keywords_with(
        &self,
        query: &str,
        opts: &SearchOptions,
    ) -> std::result::Result<Vec<KeywordCount>, SearchError> {
        let filter = query::compile(query, opts).map_err(SearchError::Syntax)?;
        Ok(self.keywords_where(filter.as_ref())?)
    }

    fn keywords_where(&self, filter: Option<&query::Filter>) -> Result<Vec<KeywordCount>> {
        let (cond, params) = match filter {
            Some(f) => (
                format!("WHERE k.bookmark_id IN (SELECT b.id FROM bookmark b WHERE {})", f.sql),
                f.params.clone(),
            ),
            None => (String::new(), Vec::new()),
        };
        let sql = format!(
            "SELECT k.keyword, count(*), k.keyword IN (SELECT keyword FROM tagged_keyword)
             FROM bookmark_keyword k {cond} GROUP BY k.keyword ORDER BY k.keyword"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |r| {
            Ok(KeywordCount { keyword: r.get(0)?, count: r.get(1)?, tagged: r.get(2)? })
        })?;
        rows.collect()
    }

    pub fn set_tagged(&mut self, keyword: &str, tagged: bool) -> Result<()> {
        let name = format!("{} \"{keyword}\"", if tagged { "Tag" } else { "Untag" });
        self.record(name, &[], true, |s| {
            let sql = if tagged {
                "INSERT OR IGNORE INTO tagged_keyword (keyword) VALUES (?1)"
            } else {
                "DELETE FROM tagged_keyword WHERE keyword = ?1"
            };
            s.conn.execute(sql, [keyword])?;
            Ok(())
        })
    }

    /// Rename a keyword on every bookmark (case-insensitive). Renaming onto an existing
    /// keyword merges them; an empty `new` deletes the keyword. Returns bookmarks changed.
    pub fn rename_keyword(&mut self, old: &str, new: &str) -> Result<usize> {
        let ids = self.ids_with_keywords(&[old])?;
        let name = if new.trim().is_empty() {
            format!("Delete keyword \"{old}\"")
        } else {
            format!("Rename \"{old}\" to \"{}\"", new.trim())
        };
        self.record(name, &ids, true, |s| {
            let tx = s.conn.transaction()?;
            let n = rename_in(&tx, old, new)?;
            tx.commit()?;
            Ok(n)
        })
    }

    fn ids_with_keywords(&self, keywords: &[&str]) -> Result<Vec<i64>> {
        let json = serde_json::to_string(keywords).expect("keywords serialize");
        self.conn
            .prepare("SELECT DISTINCT bookmark_id FROM bookmark_keyword WHERE keyword IN (SELECT value FROM json_each(?1))")?
            .query_map([json], |r| r.get(0))?
            .collect()
    }

    /// Propose a dictionary cleanup (Powermarks' Dictionary Cleanup, plus stop words).
    /// Tagged keywords are never included. Nothing changes until `apply_cleanup`.
    pub fn cleanup_plan(&self) -> Result<Vec<CleanupItem>> {
        let dict = self.keywords_where(None)?;
        let by_lower: std::collections::HashMap<String, &KeywordCount> =
            dict.iter().map(|k| (k.keyword.to_lowercase(), k)).collect();
        let mut plan = Vec::new();
        for k in dict.iter().filter(|k| !k.tagged) {
            let (reason, into) = match cleanup_rule(&k.keyword, &by_lower) {
                Some(r) => r,
                None if k.count == 1 => ("used once", None),
                None => continue,
            };
            plan.push(CleanupItem { keyword: k.keyword.clone(), count: k.count, reason, into });
        }
        Ok(plan)
    }

    /// Apply chosen cleanup changes as `(keyword, replacement)`; an empty replacement removes
    /// the keyword. All or nothing. Returns how many keywords were changed.
    pub fn apply_cleanup(&mut self, changes: &[(String, String)]) -> Result<usize> {
        let olds: Vec<&str> = changes.iter().map(|(o, _)| o.as_str()).collect();
        let ids = self.ids_with_keywords(&olds)?;
        self.record(format!("Keyword cleanup ({} keywords)", changes.len()), &ids, true, |s| {
            let tx = s.conn.transaction()?;
            for (old, new) in changes {
                rename_in(&tx, old, new)?;
            }
            tx.commit()?;
            Ok(changes.len())
        })
    }

    /// Consistent full copy of the database, safe while other connections write.
    pub fn backup_to(&self, path: &str) -> Result<()> {
        self.conn.execute("VACUUM INTO ?1", [path])?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Bookmark>> {
        let sql = format!("SELECT {COLS} FROM bookmark b WHERE b.id = ?1");
        self.conn.query_row(&sql, [id], bookmark).optional()
    }

    /// Save the user-editable fields. Returns false if the bookmark no longer exists.
    pub fn update(&mut self, b: &Bookmark) -> Result<bool> {
        let keywords = normalize_keywords(&b.keywords);
        self.record(format!("Edit \"{}\"", b.title), &[b.id], false, |s| {
            let tx = s.conn.transaction()?;
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
        })
    }

    pub fn delete(&mut self, ids: &[i64]) -> Result<usize> {
        let name = format!("Delete {} bookmark{}", ids.len(), if ids.len() == 1 { "" } else { "s" });
        self.record(name, ids, false, |s| {
            let tx = s.conn.transaction()?;
            let mut n = 0;
            for id in ids {
                n += tx.execute("DELETE FROM bookmark WHERE id = ?1", [id])?;
            }
            tx.commit()?;
            Ok(n)
        })
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

/// Keywords as stored: split on whitespace/commas, duplicates dropped (case-insensitive,
/// first spelling wins), order kept, single-space separated.
pub fn normalize_keywords(s: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|k| !k.is_empty() && seen.insert(k.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rename `old` (case-insensitive) on every bookmark using it; see `Store::rename_keyword`.
fn rename_in(tx: &Connection, old: &str, new: &str) -> Result<usize> {
    let new = normalize_keywords(new);
    let old_lower = old.to_lowercase();
    let rows: Vec<(i64, String)> = tx
        .prepare_cached(
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
        index_keywords(tx, *id, &keywords)?;
    }
    let was_tagged = tx.execute("DELETE FROM tagged_keyword WHERE keyword = ?1", [old])? > 0;
    if was_tagged && !new.is_empty() && !new.contains(' ') {
        tx.execute("INSERT OR IGNORE INTO tagged_keyword (keyword) VALUES (?1)", [&new])?;
    }
    Ok(rows.len())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CleanupItem {
    pub keyword: String,
    pub count: i64,
    pub reason: &'static str,
    /// Merge/rename target; None means remove.
    pub into: Option<String>,
}

/// Common words that carry no meaning as keywords (English plus web boilerplate).
const STOP_WORDS: &[&str] = &[
    "a", "about", "after", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be",
    "been", "but", "by", "can", "com", "could", "did", "do", "does", "for", "from", "had", "has",
    "have", "he", "her", "here", "his", "how", "htm", "html", "http", "https", "i", "if", "in",
    "index", "into", "is", "it", "its", "just", "me", "more", "my", "net", "new", "no", "not",
    "of", "on", "one", "or", "org", "our", "out", "php", "she", "so", "some", "than", "that",
    "the", "their", "them", "then", "there", "these", "they", "this", "to", "up", "us", "was",
    "we", "were", "what", "when", "where", "which", "who", "why", "will", "with", "www", "you",
    "your",
];

/// Why a keyword should go (and where to), or None to keep it.
fn cleanup_rule(
    keyword: &str,
    dict: &std::collections::HashMap<String, &KeywordCount>,
) -> Option<(&'static str, Option<String>)> {
    let lower = keyword.to_lowercase();
    if !keyword.chars().any(char::is_alphanumeric) {
        return Some(("no letters or digits", None));
    }
    if keyword.chars().all(|c| c.is_ascii_digit() || "._-:/#".contains(c)) {
        return Some(("number only", None));
    }
    if STOP_WORDS.contains(&lower.as_str()) {
        return Some(("common word", None));
    }
    let trimmed = keyword.trim_matches(|c: char| "\"'()[]{}<>.,;:!?*".contains(c));
    if trimmed != keyword && !trimmed.is_empty() {
        // "the." goes entirely; "rust," becomes "rust".
        return match cleanup_rule(trimmed, dict) {
            Some((reason, None)) => Some((reason, None)),
            _ => Some(("stray punctuation", Some(trimmed.to_string()))),
        };
    }
    // Plurals merge with their singular, into whichever spelling is used more.
    if lower.len() > 3 && lower.ends_with('s') && !["ss", "us", "is"].iter().any(|e| lower.ends_with(e)) {
        let me = dict.get(&lower)?;
        let singulars = [
            lower.strip_suffix("ies").map(|s| format!("{s}y")),
            lower.strip_suffix("es").map(String::from),
            lower.strip_suffix('s').map(String::from),
        ];
        let other = singulars
            .into_iter()
            .flatten()
            .filter(|s| !STOP_WORDS.contains(&s.as_str())) // "news" is not the plural of "new"
            .find_map(|s| dict.get(&s))?;
        if other.count >= me.count {
            return Some(("plural of another keyword", Some(other.keyword.clone())));
        }
    }
    None
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
    fix_mojibake(&html_escape::decode_html_entities(&s))
}

/// Undo UTF-8 text that was mis-decoded as Windows-1252 and re-encoded, possibly several
/// times ("â€™" → "’", "Ãƒâ€" chains). Text that doesn't round-trip cleanly is left alone.
pub fn fix_mojibake(text: &str) -> String {
    let mut s = text.to_string();
    for _ in 0..3 {
        if !(s.contains('Ã') || s.contains('â') || s.contains('Â')) {
            break;
        }
        let bytes: Option<Vec<u8>> = s.chars().map(cp1252_byte).collect();
        match bytes.and_then(|b| String::from_utf8(b).ok()) {
            Some(fixed) if fixed != s => s = fixed,
            _ => break,
        }
    }
    s
}

fn cp1252_byte(c: char) -> Option<u8> {
    match c as u32 {
        0..=0x7f | 0xa0..=0xff => Some(c as u8),
        _ => CP1252_80.iter().position(|&x| x == c).map(|i| 0x80 + i as u8),
    }
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
    fn repairs_multiply_encoded_text() {
        assert_eq!(fix_mojibake("It\u{e2}\u{20ac}\u{2122}s"), "It\u{2019}s"); // â€™
        assert_eq!(fix_mojibake("Caf\u{c3}\u{a9}"), "Caf\u{e9}");
        let twice = fix_mojibake(&fix_mojibake("x")); // idempotent on clean text
        assert_eq!(twice, "x");
        // Triple-encoded em dash survives two extra rounds.
        let mut s = "\u{2014}".to_string();
        for _ in 0..3 {
            s = s.bytes().map(cp1252).collect::<String>();
        }
        assert_eq!(fix_mojibake(&s), "\u{2014}");
        // Legitimate text containing these letters is untouched.
        assert_eq!(fix_mojibake("\u{c2}a va? \u{c3}rvore \u{e2}me"), "\u{c2}a va? \u{c3}rvore \u{e2}me");
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

    #[test]
    fn cleanup_plans_and_applies_with_backup() {
        let mut s = Store::open(":memory:").unwrap();
        s.add("https://1.example/", "1", "rust, The 2024 news --- languages", "").unwrap();
        s.add("https://2.example/", "2", "language Rust new", "").unwrap();
        s.add("https://3.example/", "3", "language keepme keepme2", "").unwrap();
        s.add("https://4.example/", "4", "keepme keepme2 the.", "").unwrap();
        s.set_tagged("keepme2", true).unwrap();
        let plan: Vec<_> = s
            .cleanup_plan()
            .unwrap()
            .into_iter()
            .map(|i| (i.keyword, i.reason, i.into))
            .collect();
        let find = |k: &str| plan.iter().find(|(kw, ..)| kw == k).map(|(_, r, i)| (*r, i.clone()));
        assert_eq!(find("The"), Some(("common word", None)));
        assert_eq!(find("the."), Some(("common word", None)));
        assert_eq!(find("2024"), Some(("number only", None)));
        assert_eq!(find("---"), Some(("no letters or digits", None)));
        assert_eq!(find("languages"), Some(("plural of another keyword", Some("language".into()))));
        assert_eq!(find("news"), Some(("used once", None))); // not merged into "new"
        assert_eq!(find("keepme"), None); // used twice
        assert_eq!(find("keepme2"), None); // tagged

        let path = std::env::temp_dir().join(format!("store-bak-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        s.backup_to(path.to_str().unwrap()).unwrap();
        let changes: Vec<(String, String)> = plan
            .iter()
            .filter(|(_, reason, _)| *reason != "used once")
            .map(|(k, _, into)| (k.clone(), into.clone().unwrap_or_default()))
            .collect();
        s.apply_cleanup(&changes).unwrap();
        let mut kws: Vec<_> = s.search("", 10).unwrap().into_iter().map(|b| b.keywords).collect();
        kws.sort();
        assert_eq!(kws, ["keepme keepme2", "language Rust", "language keepme keepme2", "rust news language"]);
        // The backup still has the original keywords.
        let backup = Store::open(path.to_str().unwrap()).unwrap();
        assert!(backup.keywords("").unwrap().iter().any(|k| k.keyword == "languages"));
        drop(backup);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn search_syntax_modes_and_errors() {
        let s = store(); // A: "food paris" desc Nice; B: "paris museum" title "It's UTF-8"
        let titles = |q: &str, o: &SearchOptions| {
            let mut t: Vec<_> = s.search_with(q, o, 10).unwrap().into_iter().map(|b| b.title).collect();
            t.sort();
            t
        };
        let all = SearchOptions::default();
        let a = "Caf\u{e9} & Bar \u{2019}s".to_string();
        let b = "It\u{2019}s UTF-8".to_string();
        assert_eq!(titles("paris ! food", &all), [b.clone()]);
        assert_eq!(titles("!food", &all), [b.clone()]);
        assert_eq!(titles("food | museum", &all), [a.clone(), b.clone()]);
        assert_eq!(titles("(food | museum) & nice", &all), [a.clone()]);
        assert_eq!(titles("muse", &all), [b.clone()]);
        assert!(titles("muse.", &all).is_empty()); // whole word only
        assert_eq!(titles("museum.", &all), [b.clone()]);

        let kw = SearchOptions { mode: Mode::Keywords, ..all.clone() };
        assert!(titles("utf", &kw).is_empty()); // only in the title
        assert_eq!(titles("utf", &all), [b.clone()]);

        let url = SearchOptions { mode: Mode::Exact, fields: vec!["url".into()], ..all.clone() };
        assert_eq!(titles("COM/?A=1&b", &url), [a.clone()]); // substring, case-insensitive
        let cased = SearchOptions { match_case: true, ..url.clone() };
        assert!(titles("COM/?", &cased).is_empty());

        assert!(matches!(s.search_with("(food", &all, 10), Err(SearchError::Syntax(_))));

        // Column sort with a tiebreak: rating desc, then title asc.
        s.add("https://c.example/", "Aardvark", "paris", "").unwrap();
        let sorted = SearchOptions {
            sort: vec![SortKey { column: "rating".into(), desc: true }, SortKey { column: "title".into(), desc: false }],
            ..all.clone()
        };
        let order: Vec<_> = s.search_with("paris", &sorted, 10).unwrap().into_iter().map(|b| b.title).collect();
        assert_eq!(order, [a.clone(), "Aardvark".to_string(), b.clone()]);

        // Ids come back in the same order; get_many keeps the requested order.
        let ids = s.search_ids("paris", &sorted).unwrap();
        let mut reversed = ids.clone();
        reversed.reverse();
        reversed.push(9999); // gone
        let back: Vec<_> = s.get_many(&reversed).unwrap().into_iter().map(|b| b.title).collect();
        assert_eq!(back, [b.clone(), "Aardvark".to_string(), a.clone()]);
        let kws = s.keywords_with("paris ! food", &all).unwrap();
        assert_eq!(kws.iter().map(|k| k.keyword.as_str()).collect::<Vec<_>>(), ["museum", "paris"]);
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
        let mojibake = s.search("", usize::MAX).unwrap().iter().filter(|b| b.title.contains('\u{fffd}') || ["\u{e2}\u{20ac}", "\u{c3}\u{192}", "\u{c3}\u{a2}"].iter().any(|m| b.title.contains(m))).count();
        eprintln!("titles with leftover mojibake: {mojibake}");
        for q in ["", "a"] {
            let t = std::time::Instant::now();
            let n = s.keywords(q).unwrap().len();
            eprintln!("keywords({q:?}): {n} in {:?}", t.elapsed());
        }
        for (q, o) in [
            ("a", SearchOptions::default()),
            ("(a | e) ! the", SearchOptions::default()),
            ("!a", SearchOptions::default()),
            ("com/", SearchOptions { mode: Mode::Exact, fields: vec!["url".into()], ..Default::default() }),
        ] {
            let t = std::time::Instant::now();
            let n = s.search_with(q, &o, 500).unwrap().len();
            let k = s.keywords_with(q, &o).unwrap().len();
            eprintln!("search {q:?} {:?}: {n} hits + {k} keywords in {:?}", o.mode, t.elapsed());
            assert!(t.elapsed().as_secs() < 1, "search {q:?} too slow: {:?}", t.elapsed());
        }
        for q in ["", "a", "(a | e) ! the"] {
            let t = std::time::Instant::now();
            let ids = s.search_ids(q, &SearchOptions::default()).unwrap();
            let page = s.get_many(&ids[..ids.len().min(100)]).unwrap().len();
            eprintln!("search_ids {q:?}: {} ids + page of {page} in {:?}", ids.len(), t.elapsed());
            assert!(t.elapsed().as_secs() < 1);
        }
        let dict_before: std::collections::HashMap<String, i64> = s.keywords("").unwrap().into_iter().map(|k| (k.keyword, k.count)).collect();
        let rows_before: std::collections::HashMap<i64, String> = s.search("", usize::MAX).unwrap().into_iter().map(|b| (b.id, b.keywords)).collect();
        let t = std::time::Instant::now();
        let plan = s.cleanup_plan().unwrap();
        let mut by: std::collections::BTreeMap<&str, (usize, i64)> = Default::default();
        for i in &plan {
            let e = by.entry(i.reason).or_default();
            e.0 += 1;
            e.1 += i.count;
        }
        eprintln!("cleanup plan in {:?}: {by:?} (keywords, uses)", t.elapsed());
        let changes: Vec<_> = plan
            .iter()
            .filter(|i| i.reason != "used once")
            .map(|i| (i.keyword.clone(), i.into.clone().unwrap_or_default()))
            .collect();
        let t = std::time::Instant::now();
        s.apply_cleanup(&changes).unwrap();
        eprintln!("applied {} in {:?}; dictionary now {}", changes.len(), t.elapsed(), s.keywords("").unwrap().len());
        let t = std::time::Instant::now();
        s.undo().unwrap();
        {
            let after: std::collections::HashMap<String, i64> = s.keywords("").unwrap().into_iter().map(|k| (k.keyword, k.count)).collect();
            let missing: Vec<_> = dict_before.iter().filter(|(k, _)| !after.contains_key(*k)).collect();
            let changed = dict_before.iter().filter(|(k, c)| after.get(*k).is_some_and(|a| a != *c)).count();
            let lower_dupes = missing.iter().filter(|(k, _)| after.keys().any(|a| a.to_lowercase() == k.to_lowercase())).count();
            let rows_after: Vec<(i64, String)> = s.search("", usize::MAX).unwrap().into_iter().map(|b| (b.id, b.keywords)).collect();
            let rows_diff = rows_after.iter().filter(|(id, k)| rows_before.get(id) != Some(k)).count();
            eprintln!("undo check: {} missing ({} only differ in case), {} counts changed, {} rows' keywords differ", missing.len(), lower_dupes, changed, rows_diff);
            assert!(missing.is_empty() && changed == 0 && rows_diff == 0, "undo did not restore the dictionary exactly");
        }
        eprintln!("undid cleanup in {:?}; dictionary back to {}", t.elapsed(), s.keywords("").unwrap().len());
    }
}
