//! SQLite bookmark store with full-text keyword search.
//! Imports Powermarks 3.5 databases (field meanings: docs/pm35-format.md).

pub mod browsers;
pub mod export;
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
-- Saved searches and links (Powermarks' macros), in the user's order.
CREATE TABLE IF NOT EXISTS saved_item (
    id INTEGER PRIMARY KEY,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('search', 'link')),
    query TEXT NOT NULL DEFAULT '',   -- search: the query
    options TEXT NOT NULL DEFAULT '{}', -- search: SearchOptions as JSON (mode, fields, case)
    url TEXT NOT NULL DEFAULT ''      -- link: where it goes
);
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
                    b.last_visited, b.visits, b.last_modified, b.last_checked, b.status, \
                    b.check_enabled, b.check_status, b.http_status, b.changed";

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
    #[serde(default = "yes")]
    pub check_enabled: bool,
    /// Link-check result ("ok", "not_found", …); None if never checked.
    #[serde(default)]
    pub check_status: Option<String>,
    #[serde(default)]
    pub http_status: Option<i64>,
    /// Content changed since it was last opened.
    #[serde(default)]
    pub changed: bool,
}

fn yes() -> bool {
    true
}

/// A saved search or link (what Powermarks called a macro).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedItem {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    /// "search" or "link".
    pub kind: String,
    #[serde(default)]
    pub query: String,
    /// For searches: SearchOptions as JSON (mode, fields, match case). Opaque to the store.
    #[serde(default = "empty_json")]
    pub options: String,
    #[serde(default)]
    pub url: String,
}

fn empty_json() -> String {
    "{}".into()
}

/// A bookmark coming from another browser or a file.
#[derive(Debug, Clone, PartialEq)]
pub struct NewBookmark {
    pub url: String,
    pub title: String,
    pub keywords: String,
    pub description: String,
    /// Unix seconds; None means "now".
    pub added: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct ImportReport {
    pub added: usize,
    /// Already in the collection (same URL), or repeated within the import.
    pub duplicates: usize,
    /// Ids of the bookmarks added.
    pub ids: Vec<i64>,
}

/// What a page says about itself (title, meta description and keywords).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageMeta {
    pub title: String,
    pub description: String,
    pub keywords: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckTarget {
    pub id: i64,
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckRecord {
    pub status: String,
    pub http_status: Option<i64>,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
    pub content_hash: Option<String>,
    pub changed: bool,
    pub error: Option<String>,
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
        check_enabled: r.get(13)?,
        check_status: r.get(14)?,
        http_status: r.get(15)?,
        changed: r.get(16)?,
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
        // v2: repair text that was already UTF-8-encoded more than once in the source file.
        if version < 2 {
            let tx = store.conn.transaction()?;
            repair_text(&tx)?;
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        // v3: link checking. Powermarks' per-bookmark "status checking enabled" (mdata f8)
        // carries over; everything else starts as never checked.
        if version < 3 {
            let tx = store.conn.transaction()?;
            let have: Vec<String> =
                tx.prepare("SELECT name FROM pragma_table_info('bookmark')")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
            for (column, decl) in [
                ("check_enabled", "INTEGER NOT NULL DEFAULT 1"),
                ("check_status", "TEXT"), // NULL: never checked
                ("http_status", "INTEGER"),
                ("etag", "TEXT"),
                ("content_hash", "TEXT"),
                ("changed", "INTEGER NOT NULL DEFAULT 0"),
                ("check_error", "TEXT"),
            ] {
                // Idempotent, so re-running migrations (e.g. after a restore) is safe.
                if !have.iter().any(|h| h == column) {
                    tx.execute_batch(&format!("ALTER TABLE bookmark ADD COLUMN {column} {decl}"))?;
                }
            }
            let mdata: Vec<(i64, String)> = tx
                .prepare("SELECT id, pm_mdata FROM bookmark WHERE pm_mdata IS NOT NULL")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_>>()?;
            for (id, raw) in mdata {
                if raw.split("][").nth(field::CHECK_ENABLED) == Some("0") {
                    tx.execute("UPDATE bookmark SET check_enabled = 0 WHERE id = ?1", [id])?;
                }
            }
            tx.pragma_update(None, "user_version", 3)?;
            tx.commit()?;
        }
        // v4: the repair learned non-Latin scripts and fields mixing encodings. Databases
        // repaired by the older v2 get it again; newer ones already had it at v2.
        if version < 4 {
            let tx = store.conn.transaction()?;
            if version >= 2 {
                repair_text(&tx)?;
            }
            tx.pragma_update(None, "user_version", 4)?;
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

    /// Merge bookmarks into the collection, skipping URLs it already has (exact match, as
    /// Powermarks did). One undoable step called `step` (e.g. "Import from Chrome").
    pub fn import_bookmarks(&mut self, step: &str, items: &[NewBookmark]) -> Result<ImportReport> {
        self.record_created(step.to_string(), |s| {
            let tx = s.conn.transaction()?;
            let mut report = ImportReport::default();
            let mut ids = Vec::new();
            for b in items {
                let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bookmark WHERE url = ?1)", [&b.url], |r| r.get(0))?;
                if exists || b.url.trim().is_empty() {
                    report.duplicates += 1;
                    continue;
                }
                let keywords = normalize_keywords(&b.keywords);
                tx.execute(
                    "INSERT INTO bookmark (url, title, keywords, description, added) VALUES (?1, ?2, ?3, ?4, coalesce(?5, unixepoch()))",
                    params![b.url, b.title, keywords, b.description, b.added],
                )?;
                let id = tx.last_insert_rowid();
                index_keywords(&tx, id, &keywords)?;
                ids.push(id);
                report.added += 1;
            }
            tx.commit()?;
            report.ids = ids.clone();
            Ok((report, ids))
        })
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
                let id = tx.last_insert_rowid();
                if e.mdata.get(field::CHECK_ENABLED).map(String::as_str) == Some("0") {
                    tx.execute("UPDATE bookmark SET check_enabled = 0 WHERE id = ?1", [id])?;
                }
                index_keywords(&tx, id, &keywords)?;
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
    /// Opening a page also clears "changed" (it was seen), as in Powermarks.
    pub fn mark_visited(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE bookmark SET visits = visits + 1, last_visited = unixepoch(), changed = 0 WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Fill in what a just-added bookmark lacks from its page (Powermarks' Fetch): the title
    /// only if it is still the URL or empty, the description only if empty; keywords merge.
    pub fn apply_fetched(&self, id: i64, meta: &PageMeta) -> Result<()> {
        let Some(b) = self.get(id)? else { return Ok(()) };
        let title = if b.title.trim().is_empty() || b.title == b.url { meta.title.trim() } else { b.title.as_str() };
        let title = if title.is_empty() { b.title.as_str() } else { title };
        let description = if b.description.is_empty() { meta.description.trim() } else { b.description.as_str() };
        let keywords = normalize_keywords(&format!("{} {}", b.keywords, meta.keywords.replace(',', " ")));
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE bookmark SET title = ?2, description = ?3, keywords = ?4 WHERE id = ?1",
            params![id, title, description, keywords],
        )?;
        index_keywords(&tx, id, &keywords)?;
        tx.commit()
    }

    /// Powermarks' Fetch (F7): the page's title and description replace the bookmark's (when
    /// the page has them) and its keywords are added, as one undoable step.
    /// Returns (bookmarks changed, keywords added).
    pub fn refetch(&mut self, fetched: &[(i64, PageMeta)]) -> Result<(usize, usize)> {
        let mut updates = Vec::new(); // (id, title, description, keywords)
        let mut added = 0;
        for (id, meta) in fetched {
            let Some(b) = self.get(*id)? else { continue };
            let pick = |new: &str, old: &str| if new.trim().is_empty() { old.to_string() } else { new.trim().to_string() };
            let (title, description) = (pick(&meta.title, &b.title), pick(&meta.description, &b.description));
            let keywords = normalize_keywords(&format!("{} {}", b.keywords, meta.keywords.replace(',', " ")));
            if (&title, &description, &keywords) != (&b.title, &b.description, &b.keywords) {
                added += keywords.split_whitespace().count().saturating_sub(normalize_keywords(&b.keywords).split_whitespace().count());
                updates.push((*id, title, description, keywords));
            }
        }
        if updates.is_empty() {
            return Ok((0, 0)); // no empty undo step
        }
        let ids: Vec<i64> = updates.iter().map(|u| u.0).collect();
        let name = format!("Fetch {} bookmark{}", ids.len(), if ids.len() == 1 { "" } else { "s" });
        self.record(name, &ids, false, |s| {
            let tx = s.conn.unchecked_transaction()?;
            for (id, title, description, keywords) in &updates {
                tx.execute("UPDATE bookmark SET title = ?2, description = ?3, keywords = ?4 WHERE id = ?1", params![id, title, description, keywords])?;
                index_keywords(&tx, *id, keywords)?;
            }
            tx.commit()?;
            Ok((updates.len(), added))
        })
    }

    /// Saved searches and links in the user's order.
    pub fn saved_items(&self) -> Result<Vec<SavedItem>> {
        self.conn
            .prepare("SELECT id, name, kind, query, options, url FROM saved_item ORDER BY position, id")?
            .query_map([], |r| {
                Ok(SavedItem { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, query: r.get(3)?, options: r.get(4)?, url: r.get(5)? })
            })?
            .collect()
    }

    /// Add at the end of the list (id ignored); returns the new id.
    pub fn save_item(&self, item: &SavedItem) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO saved_item (position, name, kind, query, options, url)
             VALUES ((SELECT coalesce(max(position), 0) + 1 FROM saved_item), ?1, ?2, ?3, ?4, ?5)",
            params![item.name.trim(), item.kind, item.query, item.options, item.url.trim()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_saved_item(&self, item: &SavedItem) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE saved_item SET name = ?2, query = ?3, options = ?4, url = ?5 WHERE id = ?1",
            params![item.id, item.name.trim(), item.query, item.options, item.url.trim()],
        )?;
        Ok(n == 1)
    }

    pub fn delete_saved_item(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM saved_item WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Move an item one place up (-1) or down (+1).
    pub fn move_saved_item(&mut self, id: i64, by: i64) -> Result<()> {
        let mut order: Vec<i64> = self.saved_items()?.into_iter().map(|i| i.id).collect();
        let Some(at) = order.iter().position(|&x| x == id) else { return Ok(()) };
        let to = (at as i64 + by).clamp(0, order.len() as i64 - 1) as usize;
        let moved = order.remove(at);
        order.insert(to, moved);
        let tx = self.conn.transaction()?;
        for (pos, id) in order.iter().enumerate() {
            tx.execute("UPDATE saved_item SET position = ?2 WHERE id = ?1", params![id, pos as i64 + 1])?;
        }
        tx.commit()
    }

    /// The bookmark with exactly this URL, if any.
    pub fn find_url(&self, url: &str) -> Result<Option<Bookmark>> {
        let sql = format!("SELECT {COLS} FROM bookmark b WHERE b.url = ?1");
        self.conn.query_row(&sql, [url], bookmark).optional()
    }

    /// What the link checker needs for each enabled bookmark among `ids`.
    /// Bookmarks to check: the given ones, or (None) every one with checking on.
    pub fn check_targets(&self, ids: Option<&[i64]>) -> Result<Vec<CheckTarget>> {
        let json = ids.map(|ids| serde_json::to_string(ids).expect("ids serialize"));
        self.conn
            .prepare(
                "SELECT id, url, etag, last_modified, content_hash FROM bookmark
                 WHERE check_enabled = 1 AND (?1 IS NULL OR id IN (SELECT value FROM json_each(?1)))",
            )?
            .query_map([json], |r| {
                Ok(CheckTarget { id: r.get(0)?, url: r.get(1)?, etag: r.get(2)?, last_modified: r.get(3)?, content_hash: r.get(4)? })
            })?
            .collect()
    }

    /// Store one check result. Validators and hash are only replaced when the check produced
    /// them; "changed" stays set until the bookmark is opened.
    pub fn record_check(&self, id: i64, r: &CheckRecord) -> Result<()> {
        self.conn.execute(
            "UPDATE bookmark SET check_status = ?2, http_status = ?3, etag = coalesce(?4, etag),
                last_modified = coalesce(?5, last_modified), content_hash = coalesce(?6, content_hash),
                changed = max(changed, ?7), check_error = ?8, last_checked = unixepoch()
             WHERE id = ?1",
            params![id, r.status, r.http_status, r.etag, r.last_modified, r.content_hash, r.changed, r.error],
        )?;
        Ok(())
    }

    pub fn set_check_enabled(&mut self, ids: &[i64], enabled: bool) -> Result<usize> {
        let name = format!("{} link checking for {} bookmark{}", if enabled { "Enable" } else { "Disable" }, ids.len(), if ids.len() == 1 { "" } else { "s" });
        self.record(name, ids, false, |s| {
            let json = serde_json::to_string(ids).expect("ids serialize");
            s.conn.execute(
                "UPDATE bookmark SET check_enabled = ?2 WHERE id IN (SELECT value FROM json_each(?1))",
                params![json, enabled],
            )
        })
    }

    /// Raise or lower the rating (0-10) of several bookmarks, as one undoable step.
    pub fn change_rating(&mut self, ids: &[i64], by: i64) -> Result<usize> {
        let name = format!("{} rating of {} bookmark{}", if by > 0 { "Raise" } else { "Lower" }, ids.len(), if ids.len() == 1 { "" } else { "s" });
        self.record(name, ids, false, |s| {
            let json = serde_json::to_string(ids).expect("ids serialize");
            s.conn.execute(
                "UPDATE bookmark SET rating = max(0, min(10, rating + ?2)) WHERE id IN (SELECT value FROM json_each(?1))",
                params![json, by],
            )
        })
    }

    /// Point bookmarks at the URLs they permanently moved to, as one undoable step.
    pub fn repair_urls(&mut self, repairs: &[(i64, String)]) -> Result<usize> {
        if repairs.is_empty() {
            return Ok(0);
        }
        let ids: Vec<i64> = repairs.iter().map(|(id, _)| *id).collect();
        let name = format!("Fix {} moved URL{}", repairs.len(), if repairs.len() == 1 { "" } else { "s" });
        self.record(name, &ids, false, |s| {
            let tx = s.conn.transaction()?;
            for (id, url) in repairs {
                tx.execute("UPDATE bookmark SET url = ?2 WHERE id = ?1", params![id, url])?;
            }
            tx.commit()?;
            Ok(repairs.len())
        })
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

fn repair_text(tx: &Connection) -> Result<()> {
    let rows: Vec<(i64, [String; 4])> = tx
        .prepare("SELECT id, title, keywords, description, notes FROM bookmark")?
        .query_map([], |r| Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?])))?
        .collect::<Result<_>>()?;
    for (id, text) in rows {
        let fixed = text.clone().map(|t| fix_mojibake(&t));
        if fixed != text {
            tx.execute(
                "UPDATE bookmark SET title = ?2, keywords = ?3, description = ?4, notes = ?5 WHERE id = ?1",
                params![id, fixed[0], fixed[1], fixed[2], fixed[3]],
            )?;
            index_keywords(tx, id, &fixed[1])?;
        }
    }
    Ok(())
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
/// Powermarks files mix encodings, even within one field: UTF-8 sequences next to
/// Windows-1252 bytes. Each is decoded on its own terms.
pub fn decode_text(raw: &str) -> String {
    let bytes: Vec<u8> = raw.chars().map(|c| c as u32 as u8).collect();
    fix_mojibake(&html_escape::decode_html_entities(&decode_mixed(&bytes)))
}

/// Every valid UTF-8 sequence as UTF-8, every other byte as Windows-1252.
fn decode_mixed(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let len = match bytes[i] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        if let Some(seq) = bytes.get(i..i + len).filter(|_| len > 1).and_then(|s| std::str::from_utf8(s).ok()) {
            out.push_str(seq);
            i += len;
        } else {
            out.push(cp1252(bytes[i]));
            i += 1;
        }
    }
    out
}

/// Undo UTF-8 that was mis-decoded as Windows-1252 (possibly several times: "â€™" → "’",
/// "Ãƒâ€" chains, "å­\u{90}" → "字"). Works run by run, so text mixing real Latin-1
/// characters with mojibake is repaired too; anything that isn't valid UTF-8 is left alone.
pub fn fix_mojibake(text: &str) -> String {
    let mut s = text.to_string();
    for _ in 0..3 {
        let mut fixed = String::with_capacity(s.len());
        let mut run = Vec::new();
        for c in s.chars() {
            match cp1252_byte(c) {
                Some(b) => run.push(b),
                None => {
                    fixed.push_str(&decode_mixed(&run));
                    run.clear();
                    fixed.push(c);
                }
            }
        }
        fixed.push_str(&decode_mixed(&run));
        if fixed == s {
            break;
        }
        s = fixed;
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
        // Mixed in one field: a real Latin-1 é next to UTF-8 CJK read as Windows-1252.
        let mixed: String = "caf\u{e9} ".chars().chain("\u{5b57}".bytes().map(cp1252)).collect();
        assert_eq!(fix_mojibake(&mixed), "caf\u{e9} \u{5b57}");
        assert_eq!(decode_mixed(b"caf\xe9 \xe5\xad\x97"), "caf\u{e9} \u{5b57}");
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

    #[test]
    fn refetch_replaces_title_merges_keywords_and_undoes() {
        let mut s = store();
        let id = s.search("", 1).unwrap()[0].id;
        let before = s.get(id).unwrap().unwrap();
        let meta = PageMeta { title: "Fresh title".into(), description: String::new(), keywords: "zebra, yak".into() };
        assert_eq!(s.refetch(&[(id, meta.clone())]).unwrap(), (1, 2));
        let after = s.get(id).unwrap().unwrap();
        assert_eq!(after.title, "Fresh title");
        assert_eq!(after.description, before.description, "empty page description keeps the old one");
        assert!(after.keywords.ends_with("zebra yak"));
        assert_eq!(s.refetch(&[(id, meta)]).unwrap(), (0, 0), "nothing new the second time");
        s.undo().unwrap();
        assert_eq!(s.get(id).unwrap().unwrap(), before);
    }

    #[test]
    fn rating_steps_clamp_and_undo() {
        let mut s = store();
        let ids: Vec<i64> = s.search("", 10).unwrap().into_iter().map(|b| b.id).collect();
        let rating = |s: &Store| ids.iter().map(|&id| s.get(id).unwrap().unwrap().rating).collect::<Vec<_>>();
        let before = rating(&s);
        for _ in 0..12 {
            s.change_rating(&ids, 1).unwrap();
        }
        assert_eq!(rating(&s), vec![10; ids.len()]);
        s.change_rating(&ids, -1).unwrap();
        assert_eq!(rating(&s), vec![9; ids.len()]);
        assert_eq!(s.undo().unwrap().as_deref(), Some(format!("Lower rating of {} bookmarks", ids.len()).as_str()));
        for _ in 0..12 {
            s.undo().unwrap();
        }
        assert_eq!(rating(&s), before);
    }

    #[test]
    fn link_check_records_and_repairs() {
        let mut s = store();
        let ids: Vec<i64> = s.search("", 10).unwrap().into_iter().map(|b| b.id).collect();
        assert_eq!(s.check_targets(Some(&ids)).unwrap().len(), 2, "imported with checking enabled");
        let (a, b) = (ids[0], ids[1]);

        s.record_check(a, &CheckRecord { status: "ok".into(), http_status: Some(200), etag: Some("\"v1\"".into()), content_hash: Some("h1".into()), ..Default::default() }).unwrap();
        // A later check without validators keeps the old ones; "changed" is sticky.
        s.record_check(a, &CheckRecord { status: "changed".into(), http_status: Some(200), changed: true, ..Default::default() }).unwrap();
        s.record_check(a, &CheckRecord { status: "ok".into(), http_status: Some(304), ..Default::default() }).unwrap();
        let t = &s.check_targets(Some(&[a])).unwrap()[0];
        assert_eq!((t.etag.as_deref(), t.content_hash.as_deref()), (Some("\"v1\""), Some("h1")));
        let bm = s.get(a).unwrap().unwrap();
        assert!(bm.changed && bm.check_status.as_deref() == Some("ok") && bm.last_checked.is_some());
        s.mark_visited(a).unwrap();
        assert!(!s.get(a).unwrap().unwrap().changed, "opening clears changed");

        s.set_check_enabled(&[b], false).unwrap();
        assert_eq!(s.check_targets(Some(&ids)).unwrap().len(), 1);
        assert_eq!(s.check_targets(None).unwrap().len(), 1, "None = all with checking on");
        s.undo().unwrap();
        assert_eq!(s.check_targets(Some(&ids)).unwrap().len(), 2);

        s.repair_urls(&[(b, "https://moved.example/".into())]).unwrap();
        assert_eq!(s.get(b).unwrap().unwrap().url, "https://moved.example/");
        assert!(s.search("moved", 5).unwrap().iter().any(|x| x.id == b), "URL index updated");
        assert_eq!(s.undo().unwrap().as_deref(), Some("Fix 1 moved URL"));
        assert_ne!(s.get(b).unwrap().unwrap().url, "https://moved.example/");
    }

    #[test]
    fn browser_import_merges_skips_duplicates_and_undoes() {
        let mut s = store(); // has http://example.com/?a=1&b=2 and http://example.com/u
        let nb = |url: &str, kw: &str| NewBookmark { url: url.into(), title: url.into(), keywords: kw.into(), description: String::new(), added: None };
        let items = [nb("http://example.com/u", "dup"), nb("https://new.example/", "Dev Tools"), nb("https://new.example/", "again"), nb("https://two.example/", "")];
        let r = s.import_bookmarks("Import from Chrome (Default)", &items).unwrap();
        assert_eq!((r.added, r.duplicates, r.ids.len()), (2, 2, 2));
        // Fetch fills a placeholder title and empty description, merges keywords.
        s.apply_fetched(r.ids[1], &PageMeta { title: " Two ".into(), description: "About two".into(), keywords: "x, Dev".into() }).unwrap();
        let two = s.get(r.ids[1]).unwrap().unwrap();
        assert_eq!((two.title.as_str(), two.description.as_str(), two.keywords.as_str()), ("Two", "About two", "x Dev"));
        s.apply_fetched(r.ids[1], &PageMeta { title: "Other".into(), description: "Other".into(), keywords: String::new() }).unwrap();
        assert_eq!(s.get(r.ids[1]).unwrap().unwrap().title, "Two", "a real title is kept");
        assert_eq!(s.find_url("https://two.example/").unwrap().map(|b| b.id), Some(r.ids[1]));
        assert_eq!(s.count().unwrap(), 4);
        assert_eq!(s.search("tools", 5).unwrap().len(), 1, "folder keywords searchable");
        assert_eq!(s.undo().unwrap().as_deref(), Some("Import from Chrome (Default)"));
        assert_eq!(s.count().unwrap(), 2);
        assert!(s.search("tools", 5).unwrap().is_empty());
        s.redo().unwrap();
        assert_eq!(s.count().unwrap(), 4);
    }

    #[test]
    fn saved_searches_and_links() {
        let mut s = Store::open(":memory:").unwrap();
        let search = |name: &str, q: &str| SavedItem { id: 0, name: name.into(), kind: "search".into(), query: q.into(), options: r#"{"mode":"keywords"}"#.into(), url: String::new() };
        let a = s.save_item(&search(" Rust ", "rust ! book")).unwrap();
        let b = s.save_item(&SavedItem { id: 0, name: "Docs".into(), kind: "link".into(), query: String::new(), options: "{}".into(), url: " https://docs.rs/ ".into() }).unwrap();
        let c = s.save_item(&search("Recipes", "recipes")).unwrap();
        let names = |s: &Store| s.saved_items().unwrap().into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert_eq!(names(&s), ["Rust", "Docs", "Recipes"]);
        assert_eq!(s.saved_items().unwrap()[1].url, "https://docs.rs/");
        s.move_saved_item(c, -1).unwrap();
        assert_eq!(names(&s), ["Rust", "Recipes", "Docs"]);
        s.move_saved_item(a, -1).unwrap(); // already first
        s.move_saved_item(b, 5).unwrap(); // already last
        assert_eq!(names(&s), ["Rust", "Recipes", "Docs"]);
        let mut item = s.saved_items().unwrap().remove(0);
        item.name = "Rust, not the book".into();
        assert!(s.update_saved_item(&item).unwrap());
        s.delete_saved_item(c).unwrap();
        assert_eq!(names(&s), ["Rust, not the book", "Docs"]);
        assert_eq!(s.saved_items().unwrap()[0].options, r#"{"mode":"keywords"}"#);
        assert!(s.save_item(&SavedItem { kind: "bogus".into(), ..search("x", "") }).is_err(), "kind is checked");
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
        {
            // Export round trips: pm35 and Netscape HTML back into fresh stores, field by field.
            let t = std::time::Instant::now();
            let rows = export::export_rows_all(&s);
            let pm = export::to_pm35(&rows);
            let html = export::to_netscape(&rows);
            eprintln!("exported {} rows ({} KB pm35, {} KB html) in {:?}", rows.len(), pm.len() / 1024, html.len() / 1024, t.elapsed());
            let mut a = Store::open(":memory:").unwrap();
            a.import_pm35(&pm35::parse(&pm).expect("exported pm35 parses")).unwrap();
            let mut b = Store::open(":memory:").unwrap();
            b.import_bookmarks("Import from html", &browsers::read_netscape_html(&html)).unwrap();
            let key = |st: &Store| {
                let mut v: Vec<(String, String, String)> =
                    st.search("", usize::MAX).unwrap().into_iter().map(|x| (x.url, x.title, normalize_keywords(&x.keywords))).collect();
                v.sort();
                v
            };
            let (orig, via_pm, via_html) = (key(&s), key(&a), key(&b));
            let diff = |v: &Vec<(String, String, String)>| orig.iter().zip(v).filter(|(x, y)| x != y).count() + orig.len().abs_diff(v.len());
            eprintln!("pm35 round trip: {} differences; html round trip: {} differences (html drops duplicate URLs)", diff(&via_pm), diff(&via_html));
            for (x, y) in orig.iter().zip(&via_pm).filter(|(x, y)| x != y) {
                // Describe the difference by field and character codes only (data is private).
                let codes = |a: &str, b: &str| {
                    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
                    let i = a.iter().zip(&b).position(|(p, q)| p != q).unwrap_or(a.len().min(b.len()));
                    format!("at {i}: {:?} vs {:?}", a.get(i..i + 3).map(|c| c.iter().map(|c| *c as u32).collect::<Vec<_>>()), b.get(i..i + 3).map(|c| c.iter().map(|c| *c as u32).collect::<Vec<_>>()))
                };
                for (name, p, q) in [("url", &x.0, &y.0), ("title", &x.1, &y.1), ("keywords", &x.2, &y.2)] {
                    if p != q {
                        eprintln!("  differs in {name}: len {} vs {}, {}", p.chars().count(), q.chars().count(), codes(p, q));
                    }
                }
            }
            assert_eq!(diff(&via_pm), 0, "pm35 export must round-trip");
        }
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
