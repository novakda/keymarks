//! Folders: one tree with a single root; every bookmark (or URL-less note) sits in exactly
//! one folder. Folders are not tags: keywords are untouched by anything here.
//! Folders and bookmarks share one `position` order among siblings.

use crate::{Bookmark, COLS, Result, Store, bookmark};
use rusqlite::{Connection, OptionalExtension, params};

/// The root folder's id. The schema allows exactly one folder without a parent: this one.
pub const ROOT: i64 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Folder {
    pub id: i64,
    /// None only for the root.
    pub parent_id: Option<i64>,
    pub name: String,
    pub position: i64,
    pub note: String,
    pub added: i64,
    pub modified: i64,
}

/// One child of a folder, in display order.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Node {
    Folder(Folder),
    Bookmark(Bookmark),
}

/// v6 migration step. Idempotent: columns are added only if missing, the root only once,
/// and only rows without a position get one. Runs inside the caller's transaction.
pub(crate) fn migrate(tx: &Connection) -> Result<()> {
    let have: Vec<String> =
        tx.prepare("SELECT name FROM pragma_table_info('bookmark')")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?;
    for (column, decl) in [
        ("folder_id", "INTEGER NOT NULL DEFAULT 1"),
        ("position", "INTEGER"), // NULL only until bookmark_place runs
        ("kind", "TEXT NOT NULL DEFAULT 'bookmark' CHECK (kind IN ('bookmark', 'note'))"), // note: url is ''
    ] {
        if !have.iter().any(|h| h == column) {
            tx.execute_batch(&format!("ALTER TABLE bookmark ADD COLUMN {column} {decl}"))?;
        }
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS folder (
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES folder(id),
            name TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            note TEXT NOT NULL DEFAULT '',
            added INTEGER NOT NULL DEFAULT (unixepoch()),
            modified INTEGER NOT NULL DEFAULT (unixepoch()),
            CHECK ((parent_id IS NULL) = (id = 1)) -- exactly one root
        );
        CREATE INDEX IF NOT EXISTS folder_parent ON folder(parent_id, position);
        INSERT OR IGNORE INTO folder (id, parent_id, name) VALUES (1, NULL, 'Bookmarks');
        CREATE INDEX IF NOT EXISTS bookmark_folder ON bookmark(folder_id, position);
        -- Re-index full text only when indexed text changes, not on moves.
        DROP TRIGGER IF EXISTS bookmark_au;
        CREATE TRIGGER bookmark_au AFTER UPDATE OF title, keywords, description, notes, url ON bookmark BEGIN
            INSERT INTO bookmark_fts(bookmark_fts, rowid, title, keywords, description, notes, url)
            VALUES ('delete', old.id, old.title, old.keywords, old.description, old.notes, old.url);
            INSERT INTO bookmark_fts(rowid, title, keywords, description, notes, url)
            VALUES (new.id, new.title, new.keywords, new.description, new.notes, new.url);
        END;
        -- New rows go to the end of their folder; rows whose folder is gone (e.g. undo after
        -- the folder was deleted) go to the end of the root.
        CREATE TRIGGER IF NOT EXISTS bookmark_place AFTER INSERT ON bookmark
        WHEN new.position IS NULL OR NOT EXISTS (SELECT 1 FROM folder WHERE id = new.folder_id) BEGIN
            UPDATE bookmark SET
                folder_id = coalesce((SELECT id FROM folder WHERE id = new.folder_id), 1),
                position = 1 + coalesce((SELECT max(p) FROM (
                    SELECT position AS p FROM bookmark
                    WHERE folder_id = coalesce((SELECT id FROM folder WHERE id = new.folder_id), 1) AND id <> new.id
                    UNION ALL SELECT position FROM folder
                    WHERE parent_id = coalesce((SELECT id FROM folder WHERE id = new.folder_id), 1))), 0)
            WHERE id = new.id;
        END;
        -- The existing collection: into the root, oldest first (FLD-18).
        UPDATE bookmark SET position = o.n
        FROM (SELECT id, row_number() OVER (ORDER BY coalesce(added, 0), id)
                  + (SELECT coalesce(max(position), 0) FROM bookmark WHERE folder_id = 1) AS n
              FROM bookmark WHERE position IS NULL) o
        WHERE o.id = bookmark.id;",
    )?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Folder(i64),
    Bookmark(i64),
}

fn siblings(tx: &Connection, folder: i64) -> Result<Vec<Item>> {
    tx.prepare(
        "SELECT 1, id, position FROM folder WHERE parent_id = ?1
         UNION ALL SELECT 0, id, position FROM bookmark WHERE folder_id = ?1
         ORDER BY 3, 1 DESC, 2",
    )?
    .query_map([folder], |r| Ok(if r.get::<_, i64>(0)? == 1 { Item::Folder(r.get(1)?) } else { Item::Bookmark(r.get(1)?) }))?
    .collect()
}

/// Make `order` the contents of `folder`, positions 1..n.
// ponytail: renumbers the whole sibling list per move, writing only rows whose place
// changed; switch to gapped/fractional positions if big folders make drags slow.
fn set_order(tx: &Connection, folder: i64, order: &[Item]) -> Result<()> {
    for (i, item) in order.iter().enumerate() {
        let pos = i as i64 + 1;
        match item {
            Item::Folder(id) => tx.execute(
                "UPDATE folder SET parent_id = ?2, position = ?3 WHERE id = ?1 AND (parent_id IS NOT ?2 OR position IS NOT ?3)",
                params![id, folder, pos],
            )?,
            Item::Bookmark(id) => tx.execute(
                "UPDATE bookmark SET folder_id = ?2, position = ?3 WHERE id = ?1 AND (folder_id IS NOT ?2 OR position IS NOT ?3)",
                params![id, folder, pos],
            )?,
        };
    }
    Ok(())
}

/// Put `item` into `folder` at index `at` (None or past the end: last).
fn place(tx: &Connection, item: Item, folder: i64, at: Option<i64>) -> Result<()> {
    let mut order: Vec<Item> = siblings(tx, folder)?.into_iter().filter(|i| *i != item).collect();
    let at = at.map_or(order.len(), |a| (a.max(0) as usize).min(order.len()));
    order.insert(at, item);
    set_order(tx, folder, &order)
}

fn folder_row(r: &rusqlite::Row) -> Result<Folder> {
    Ok(Folder { id: r.get(0)?, parent_id: r.get(1)?, name: r.get(2)?, position: r.get(3)?, note: r.get(4)?, added: r.get(5)?, modified: r.get(6)? })
}

fn exists(tx: &Connection, folder: i64) -> Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM folder WHERE id = ?1)", [folder], |r| r.get(0))
}

// Hooks for later steps: none of these are undoable yet (history snapshots bookmark rows
// only), and the tree UI, import and export don't call them.
impl Store {
    pub fn folder(&self, id: i64) -> Result<Option<Folder>> {
        self.conn
            .query_row("SELECT id, parent_id, name, position, note, added, modified FROM folder WHERE id = ?1", [id], folder_row)
            .optional()
    }

    /// A folder's subfolders and bookmarks in stored order.
    pub fn children(&self, id: i64) -> Result<Vec<Node>> {
        siblings(&self.conn, id)?
            .into_iter()
            .map(|item| {
                Ok(match item {
                    Item::Folder(id) => Node::Folder(self.folder(id)?.expect("listed folder exists")),
                    Item::Bookmark(id) => Node::Bookmark(
                        self.conn.query_row(&format!("SELECT {COLS} FROM bookmark b WHERE b.id = ?1"), [id], bookmark)?,
                    ),
                })
            })
            .collect()
    }

    /// New folder in `parent` at index `at` (None: last). None if `parent` doesn't exist.
    pub fn create_folder(&mut self, parent: i64, name: &str, at: Option<i64>) -> Result<Option<i64>> {
        let tx = self.conn.transaction()?;
        if !exists(&tx, parent)? {
            return Ok(None);
        }
        tx.execute("INSERT INTO folder (parent_id, name, position) VALUES (?1, ?2, 0)", params![parent, name.trim()])?;
        let id = tx.last_insert_rowid();
        place(&tx, Item::Folder(id), parent, at)?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn rename_folder(&self, id: i64, name: &str) -> Result<bool> {
        let n = self.conn.execute("UPDATE folder SET name = ?2, modified = unixepoch() WHERE id = ?1", params![id, name.trim()])?;
        Ok(n == 1)
    }

    /// Move a folder (with everything in it) into `parent` at index `at`. Refused (false)
    /// for the root, a missing folder or parent, or a parent inside the folder itself.
    pub fn move_folder(&mut self, id: i64, parent: i64, at: Option<i64>) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let inside: bool = tx.query_row(
            "WITH RECURSIVE up(id) AS (SELECT ?2 UNION SELECT f.parent_id FROM folder f JOIN up ON f.id = up.id WHERE f.parent_id IS NOT NULL)
             SELECT EXISTS(SELECT 1 FROM up WHERE id = ?1)",
            params![id, parent],
            |r| r.get(0),
        )?;
        if id == ROOT || inside || !exists(&tx, id)? || !exists(&tx, parent)? {
            return Ok(false);
        }
        place(&tx, Item::Folder(id), parent, at)?;
        tx.execute("UPDATE folder SET modified = unixepoch() WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Delete a folder. Nothing in it is deleted: its contents move up to the parent, in
    /// their order, where the folder was. The root can't be deleted (false).
    pub fn delete_folder(&mut self, id: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let Some(parent) = tx.query_row("SELECT parent_id FROM folder WHERE id = ?1", [id], |r| r.get::<_, Option<i64>>(0)).optional()?.flatten()
        else {
            return Ok(false); // missing, or the root
        };
        let kids = siblings(&tx, id)?;
        let order: Vec<Item> =
            siblings(&tx, parent)?.into_iter().flat_map(|i| if i == Item::Folder(id) { kids.clone() } else { vec![i] }).collect();
        set_order(&tx, parent, &order)?;
        tx.execute("DELETE FROM folder WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Move a bookmark (or note) into `folder` at index `at` (None: last). False if either
    /// is missing.
    pub fn move_bookmark(&mut self, id: i64, folder: i64, at: Option<i64>) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let found: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bookmark WHERE id = ?1)", [id], |r| r.get(0))?;
        if !found || !exists(&tx, folder)? {
            return Ok(false);
        }
        place(&tx, Item::Bookmark(id), folder, at)?;
        tx.commit()?;
        Ok(true)
    }

    /// A note item without a URL (NOTE-03), last in `folder`. None if the folder is missing.
    pub fn add_note(&self, folder: i64, title: &str, notes: &str) -> Result<Option<i64>> {
        if !exists(&self.conn, folder)? {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO bookmark (kind, url, title, notes, folder_id, added) VALUES ('note', '', ?1, ?2, ?3, unixepoch())",
            params![title, notes, folder],
        )?;
        Ok(Some(self.conn.last_insert_rowid()))
    }

    /// NOT run by the migration (Q2): Keymarks' imports kept no record of which keywords
    /// came from browser folders, so the database alone can't say. Given the browser's
    /// folder paths (e.g. read again from its bookmarks file), builds those folders and moves
    /// each root bookmark whose keywords begin with a path's words (as `folder_keywords`
    /// writes them) into the deepest such folder. Keywords are untouched. Typed keywords that
    /// happen to read like a folder path match too. Returns bookmarks moved.
    pub fn assign_folders_from_browser_paths(&mut self, paths: &[Vec<String>]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut targets: Vec<(Vec<String>, i64)> = Vec::new(); // (lowercased words, folder)
        for path in paths.iter().filter(|p| !p.is_empty()) {
            let mut at = ROOT;
            for name in path {
                let found = tx
                    .query_row("SELECT id FROM folder WHERE parent_id = ?1 AND name = ?2", params![at, name.trim()], |r| r.get(0))
                    .optional()?;
                at = match found {
                    Some(id) => id,
                    None => {
                        tx.execute("INSERT INTO folder (parent_id, name, position) VALUES (?1, ?2, 0)", params![at, name.trim()])?;
                        let id = tx.last_insert_rowid();
                        place(&tx, Item::Folder(id), at, None)?;
                        id
                    }
                };
            }
            let words = crate::browsers::folder_keywords(path, "").split_whitespace().map(str::to_lowercase).collect();
            targets.push((words, at));
        }
        targets.sort_by_key(|(w, _)| std::cmp::Reverse(w.len())); // deepest match wins
        let root: Vec<(i64, String)> = tx
            .prepare("SELECT id, keywords FROM bookmark WHERE folder_id = 1 AND kind = 'bookmark' ORDER BY position, id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;
        let mut moved = 0;
        for (id, keywords) in root {
            let kws: Vec<String> = keywords.split_whitespace().map(str::to_lowercase).collect();
            if let Some((_, folder)) = targets.iter().find(|(w, _)| !w.is_empty() && kws.starts_with(w)) {
                place(&tx, Item::Bookmark(id), *folder, None)?;
                moved += 1;
            }
        }
        tx.commit()?;
        Ok(moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NewBookmark;
    use rusqlite::Connection;

    struct TempDb(String);
    impl TempDb {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("store-fld-{name}-{}.db", std::process::id()));
            let t = TempDb(p.to_str().unwrap().to_string());
            t.clean();
            t
        }
        fn clean(&self) {
            for ext in ["", "-wal", "-shm", ".v5.bak"] {
                let _ = std::fs::remove_file(format!("{}{ext}", self.0));
            }
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            self.clean();
        }
    }

    /// A database as Keymarks 0.11 left it (schema v5, no folders), with `rows` of
    /// (url, title, keywords, added).
    fn v5_fixture(path: &str, rows: &[(&str, &str, &str, i64)]) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(crate::SCHEMA).unwrap();
        for col in ["check_enabled INTEGER NOT NULL DEFAULT 1", "check_status TEXT", "http_status INTEGER", "etag TEXT",
            "content_hash TEXT", "changed INTEGER NOT NULL DEFAULT 0", "check_error TEXT", "schedule TEXT"] {
            c.execute_batch(&format!("ALTER TABLE bookmark ADD COLUMN {col}")).unwrap();
        }
        for (url, title, kw, added) in rows {
            c.execute("INSERT INTO bookmark (url, title, keywords, added) VALUES (?1, ?2, ?3, ?4)", rusqlite::params![url, title, kw, added]).unwrap();
            crate::index_keywords(&c, c.last_insert_rowid(), kw).unwrap();
        }
        c.pragma_update(None, "user_version", 5).unwrap();
    }

    fn version(path: &str) -> i64 {
        Connection::open(path).unwrap().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap()
    }

    fn force_version(path: &str, v: i64) {
        Connection::open(path).unwrap().pragma_update(None, "user_version", v).unwrap();
    }

    /// Children as ("f"/"b", name or title) in order.
    fn names(s: &Store, folder: i64) -> Vec<(&'static str, String)> {
        s.children(folder)
            .unwrap()
            .into_iter()
            .map(|n| match n {
                Node::Folder(f) => ("f", f.name),
                Node::Bookmark(b) => ("b", b.title),
            })
            .collect()
    }

    #[test]
    fn empty_database_gets_a_single_root() {
        let s = Store::open(":memory:").unwrap();
        let root = s.folder(ROOT).unwrap().unwrap();
        assert_eq!(root.parent_id, None);
        assert!(names(&s, ROOT).is_empty());
        let v: i64 = s.conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, crate::SCHEMA_VERSION);
        // The schema itself refuses a second root and a root with a parent.
        assert!(s.conn.execute("INSERT INTO folder (parent_id, name) VALUES (NULL, 'x')", []).is_err());
        assert!(s.conn.execute("UPDATE folder SET parent_id = 1 WHERE id = 1", []).is_err());
        // New bookmarks land at the end of the root.
        s.add("https://a.example/", "A", "", "").unwrap();
        let (b, _) = s.add("https://b.example/", "B", "", "").unwrap();
        assert_eq!(names(&s, ROOT), [("b", "A".into()), ("b", "B".into())]);
        let b = s.get(b).unwrap().unwrap();
        assert_eq!((b.kind.as_str(), b.folder_id, b.position), ("bookmark", ROOT, 2));
    }

    #[test]
    fn migration_puts_an_existing_collection_in_the_root_by_added() {
        let db = TempDb::new("mig");
        v5_fixture(&db.0, &[("https://c.example/", "C", "dev tools rust", 300), ("https://a.example/", "A", "news", 100), ("https://b.example/", "B", "", 200)]);
        let s = Store::open(&db.0).unwrap();
        assert_eq!(names(&s, ROOT), [("b", "A".into()), ("b", "B".into()), ("b", "C".into())]);
        assert_eq!(s.search("rust", 5).unwrap()[0].keywords, "dev tools rust", "keywords untouched, still searchable");
        assert_eq!(s.keywords("").unwrap().len(), 4);
        assert_eq!(s.children(ROOT).unwrap().iter().filter(|n| matches!(n, Node::Folder(_))).count(), 0, "no keyword folders");
        // The pre-migration copy sits alongside, untouched.
        let bak = format!("{}.v5.bak", db.0);
        assert_eq!(version(&bak), 5);
        let c = Connection::open(&bak).unwrap();
        assert_eq!(c.query_row("SELECT count(*) FROM bookmark", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        assert!(c.prepare("SELECT 1 FROM folder").is_err(), "backup is the old schema");
        drop((s, c));
        assert_eq!(version(&db.0), crate::SCHEMA_VERSION);

        // Idempotent: running v6 again (e.g. after restoring) changes nothing.
        {
            let mut s = Store::open(&db.0).unwrap();
            let f = s.create_folder(ROOT, "Kept", None).unwrap().unwrap();
            s.move_bookmark(s.find_url("https://a.example/").unwrap().unwrap().id, f, None).unwrap();
        }
        force_version(&db.0, 5);
        let s = Store::open(&db.0).unwrap();
        assert_eq!(names(&s, ROOT), [("b", "B".into()), ("b", "C".into()), ("f", "Kept".into())]);
        let kept = s.children(ROOT).unwrap().into_iter().find_map(|n| if let Node::Folder(f) = n { Some(f.id) } else { None }).unwrap();
        assert_eq!(names(&s, kept), [("b", "A".into())]);
        assert_eq!(s.conn.query_row("SELECT count(*) FROM folder WHERE parent_id IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(s.count().unwrap(), 3);
        assert_eq!(version(&bak), 5, "first backup is never overwritten");
    }

    #[test]
    fn migration_is_one_transaction() {
        let db = TempDb::new("tx");
        v5_fixture(&db.0, &[("https://a.example/", "A", "", 1)]);
        // A stray folder table of the wrong shape makes the v6 step fail after its ALTERs.
        Connection::open(&db.0).unwrap().execute_batch("CREATE TABLE folder (id INTEGER PRIMARY KEY, junk)").unwrap();
        assert!(Store::open(&db.0).is_err());
        assert_eq!(version(&db.0), 5, "nothing committed");
        let c = Connection::open(&db.0).unwrap();
        assert!(c.prepare("SELECT folder_id FROM bookmark").is_err(), "no half-added columns");
        assert_eq!(c.query_row("SELECT title FROM bookmark", [], |r| r.get::<_, String>(0)).unwrap(), "A");
    }

    #[test]
    fn notes_have_no_url_and_skip_url_rules() {
        let db = TempDb::new("note");
        v5_fixture(&db.0, &[("https://a.example/", "A", "", 1)]);
        let mut s = Store::open(&db.0).unwrap();
        let f = s.create_folder(ROOT, "Ideas", None).unwrap().unwrap();
        let n = s.add_note(f, "Shopping list", "eggs and flour").unwrap().unwrap();
        let note = s.get(n).unwrap().unwrap();
        assert_eq!((note.kind.as_str(), note.url.as_str(), note.folder_id), ("note", "", f));
        assert_eq!(s.search("flour", 5).unwrap()[0].id, n, "full-text indexed");
        assert_eq!(s.add_note(9999, "x", "").unwrap(), None, "missing folder");
        // Not a duplicate of anything, never checked, never exported as a link.
        assert_eq!(s.find_url("").unwrap(), None);
        let (empty, existed) = s.add("", "Untitled", "", "").unwrap();
        assert!(!existed && empty != n, "an empty URL is not the note's duplicate");
        s.delete(&[empty]).unwrap();
        assert!(s.check_targets(None).unwrap().iter().all(|t| t.id != n));
        s.set_schedule(&[n], Some("Weekly")).unwrap();
        assert!(s.due_for_check(&[("Weekly".into(), 1)], i64::MAX / 2, 10).unwrap().iter().all(|t| t.id != n));
        let rows = s.export_rows(None).unwrap();
        assert_eq!(rows.len(), 2, "lossless exports keep the note");
        assert!(!crate::export::to_netscape(&rows).contains("Shopping"));
        let pm = String::from_utf8_lossy(&crate::export::to_pm35(&rows)).to_string();
        assert!(pm.contains("a.example") && !pm.contains("Shopping"));
        drop(s);
        // Survives the migration running again.
        force_version(&db.0, 5);
        let s = Store::open(&db.0).unwrap();
        let note = s.get(n).unwrap().unwrap();
        assert_eq!((note.kind.as_str(), note.folder_id), ("note", f));
        assert_eq!(names(&s, f), [("b", "Shopping list".into())]);
    }

    #[test]
    fn create_rename_and_list_in_order() {
        let mut s = Store::open(":memory:").unwrap();
        let (a, _) = s.add("https://a.example/", "A", "", "").unwrap();
        let x = s.create_folder(ROOT, "X", None).unwrap().unwrap();
        let y = s.create_folder(ROOT, "Y", Some(0)).unwrap().unwrap();
        assert_eq!(names(&s, ROOT), [("f", "Y".into()), ("b", "A".into()), ("f", "X".into())]);
        assert_eq!(s.create_folder(9999, "Z", None).unwrap(), None);
        assert!(s.rename_folder(y, "  Why ").unwrap());
        assert_eq!(s.folder(y).unwrap().unwrap().name, "Why");
        assert!(!s.rename_folder(9999, "Z").unwrap());
        assert!(s.move_bookmark(a, x, Some(0)).unwrap());
        assert_eq!(names(&s, ROOT), [("f", "Why".into()), ("f", "X".into())]);
        assert_eq!(names(&s, x), [("b", "A".into())]);
        assert_eq!(s.folder(x).unwrap().unwrap().parent_id, Some(ROOT));
    }

    #[test]
    fn move_bookmarks_and_folders() {
        let mut s = Store::open(":memory:").unwrap();
        let ids: Vec<i64> = ["1", "2", "3"].iter().map(|t| s.add(&format!("https://{t}.example/"), t, "", "").unwrap().0).collect();
        assert!(s.move_bookmark(ids[2], ROOT, Some(0)).unwrap(), "reorder within a folder");
        assert_eq!(names(&s, ROOT), [("b", "3".into()), ("b", "1".into()), ("b", "2".into())]);
        assert!(s.move_bookmark(ids[2], ROOT, Some(99)).unwrap(), "past the end = the end");
        assert_eq!(names(&s, ROOT), [("b", "1".into()), ("b", "2".into()), ("b", "3".into())]);
        assert!(!s.move_bookmark(ids[0], 9999, None).unwrap(), "missing folder");
        assert!(!s.move_bookmark(9999, ROOT, None).unwrap(), "missing bookmark");

        let p = s.create_folder(ROOT, "P", None).unwrap().unwrap();
        let c = s.create_folder(p, "C", None).unwrap().unwrap();
        let g = s.create_folder(c, "G", None).unwrap().unwrap();
        assert!(!s.move_folder(p, g, None).unwrap(), "not into its own subtree");
        assert!(!s.move_folder(p, p, None).unwrap(), "not into itself");
        assert!(!s.move_folder(ROOT, p, None).unwrap(), "root stays put");
        assert!(!s.move_folder(p, 9999, None).unwrap());
        assert!(s.move_folder(g, ROOT, Some(1)).unwrap());
        assert_eq!(names(&s, ROOT), [("b", "1".into()), ("f", "G".into()), ("b", "2".into()), ("b", "3".into()), ("f", "P".into())]);
        assert!(names(&s, c).is_empty());
        assert_eq!(s.folder(g).unwrap().unwrap().parent_id, Some(ROOT));
    }

    #[test]
    fn delete_moves_contents_to_the_parent_in_its_place() {
        let mut s = Store::open(":memory:").unwrap();
        let (a, _) = s.add("https://a.example/", "A", "", "").unwrap();
        let d = s.create_folder(ROOT, "D", None).unwrap().unwrap();
        s.add("https://z.example/", "Z", "", "").unwrap();
        let (b, _) = s.add("https://b.example/", "B", "", "").unwrap();
        let sub = s.create_folder(d, "Sub", None).unwrap().unwrap();
        s.move_bookmark(b, d, Some(0)).unwrap();
        let n = s.add_note(d, "N", "").unwrap().unwrap();
        assert_eq!(names(&s, d), [("b", "B".into()), ("f", "Sub".into()), ("b", "N".into())]);
        assert!(s.delete_folder(d).unwrap());
        assert_eq!(s.folder(d).unwrap(), None);
        assert_eq!(names(&s, ROOT), [("b", "A".into()), ("b", "B".into()), ("f", "Sub".into()), ("b", "N".into()), ("b", "Z".into())]);
        assert_eq!(s.folder(sub).unwrap().unwrap().parent_id, Some(ROOT));
        assert_eq!(s.get(n).unwrap().unwrap().folder_id, ROOT);
        assert_eq!(s.count().unwrap(), 4, "nothing deleted but the folder");
        assert!(!s.delete_folder(ROOT).unwrap(), "the root can't be deleted");
        assert!(!s.delete_folder(9999).unwrap());
        // A bookmark brought back by undo into a folder that's gone lands in the root.
        s.delete(&[a]).unwrap();
        s.undo().unwrap();
        assert_eq!(s.get(a).unwrap().unwrap().folder_id, ROOT);
    }

    #[test]
    fn keyword_pass_builds_folders_from_known_browser_paths() {
        let mut s = Store::open(":memory:").unwrap();
        let kw = |path: &[&str], extra: &str| crate::browsers::folder_keywords(&path.iter().map(|p| p.to_string()).collect::<Vec<_>>(), extra);
        let nb = |url: &str, keywords: String| NewBookmark { url: url.into(), title: url.into(), keywords, description: String::new(), added: None };
        s.import_bookmarks("Import", &[
            nb("https://tokio.rs/", kw(&["Dev Tools", "Async/IO"], "rust")),
            nb("https://crates.io/", kw(&["Dev Tools"], "")),
            nb("https://news.example/", kw(&[], "news")),
            // Typed by hand, but reads exactly like the "Dev Tools" folder: why this pass
            // can't run on its own at migration time.
            nb("https://typed.example/", "dev tools".into()),
        ]).unwrap();
        assert!(names(&s, ROOT).iter().all(|(k, _)| *k == "b"), "opening never builds keyword folders");
        let paths = [vec!["Dev Tools".to_string()], vec!["Dev Tools".to_string(), "Async/IO".to_string()]];
        assert_eq!(s.assign_folders_from_browser_paths(&paths).unwrap(), 3);
        assert_eq!(names(&s, ROOT), [("b", "https://news.example/".into()), ("f", "Dev Tools".into())]);
        let dev = s.children(ROOT).unwrap().into_iter().find_map(|n| if let Node::Folder(f) = n { Some(f.id) } else { None }).unwrap();
        assert_eq!(names(&s, dev), [("f", "Async/IO".into()), ("b", "https://crates.io/".into()), ("b", "https://typed.example/".into())]);
        assert_eq!(s.find_url("https://tokio.rs/").unwrap().unwrap().keywords, "Dev Tools Async IO rust", "keywords untouched");
        assert_eq!(s.assign_folders_from_browser_paths(&paths).unwrap(), 0, "only root bookmarks move; folders reused");
    }
}
