//! Folders: one tree with a single root; every bookmark (or URL-less note) sits in exactly
//! one folder. Folders are not tags: keywords are untouched by anything here.
//! Folders and bookmarks share one `position` order among siblings.

use crate::{Bookmark, COLS, Result, Store, bookmark};
use rusqlite::{Connection, OptionalExtension, params};

/// The root folder's id. The schema allows exactly one folder without a parent: this one.
pub const ROOT: i64 = 1;

/// Stands for the New Item Folder (ITEM-04) where a folder id is asked for: adds from the
/// browser, Fast Add and the clipboard land there. Never a real id (ids start at 1).
pub const NEW_ITEMS: i64 = 0;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Folder {
    pub id: i64,
    /// None only for the root.
    pub parent_id: Option<i64>,
    pub name: String,
    pub position: i64,
    pub note: String,
    pub added: i64,
    pub modified: i64,
    /// Collapsed in the tree (FLD-08).
    pub folded: bool,
    /// The New Item Folder (ITEM-04): role `new_items`.
    #[serde(default)]
    pub new_items: bool,
}

/// v9 migration step: folder roles (`new_items`, ITEM-04). Kept on the row, so a rename
/// keeps it and undo restores it with the folder. Idempotent.
pub(crate) fn migrate_roles(tx: &Connection) -> Result<()> {
    let have: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('folder') WHERE name = 'role')", [], |r| r.get(0))?;
    if !have {
        tx.execute_batch("ALTER TABLE folder ADD COLUMN role TEXT")?;
    }
    Ok(())
}

// ponytail: "one at most" is kept by `set_new_item_folder` and resolved oldest-first here, not
// by a unique index: undo writes whole folder rows back, and a deleted inbox restored after a
// new one was made would break such an index. Two marked folders can then exist; the oldest wins.
fn find_inbox(tx: &Connection) -> Result<Option<i64>> {
    tx.query_row("SELECT id FROM folder WHERE role = 'new_items' ORDER BY id LIMIT 1", [], |r| r.get(0)).optional()
}

/// The folder `folder` stands for: the New Item Folder for `NEW_ITEMS`, made as a root-level
/// "New" if there is none (its id then goes in `created`, so the add's undo removes it too).
pub(crate) fn resolve(tx: &Connection, folder: i64, created: &mut Vec<i64>) -> Result<i64> {
    if folder != NEW_ITEMS {
        return Ok(folder);
    }
    if let Some(id) = find_inbox(tx)? {
        return Ok(id);
    }
    let id = insert_folder(tx, ROOT, "New", "", None)?;
    tx.execute("UPDATE folder SET role = 'new_items' WHERE id = ?1", [id])?;
    created.push(id);
    Ok(id)
}

/// v8 migration step (v10: the note too): folder names and notes in their own full-text
/// index (external content, kept by triggers, so undo, restore and every write path stay in
/// step). Idempotent.
pub(crate) fn migrate_search(tx: &Connection) -> Result<()> {
    tx.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS folder_fts USING fts5(
            name, note, content='folder', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
        CREATE TRIGGER IF NOT EXISTS folder_ai AFTER INSERT ON folder BEGIN
            INSERT INTO folder_fts(rowid, name, note) VALUES (new.id, new.name, new.note);
        END;
        CREATE TRIGGER IF NOT EXISTS folder_ad AFTER DELETE ON folder BEGIN
            INSERT INTO folder_fts(folder_fts, rowid, name, note) VALUES ('delete', old.id, old.name, old.note);
        END;
        CREATE TRIGGER IF NOT EXISTS folder_au AFTER UPDATE OF name, note ON folder BEGIN
            INSERT INTO folder_fts(folder_fts, rowid, name, note) VALUES ('delete', old.id, old.name, old.note);
            INSERT INTO folder_fts(rowid, name, note) VALUES (new.id, new.name, new.note);
        END;
        INSERT INTO folder_fts(folder_fts) VALUES ('rebuild');",
    )
}

/// v10 migration step: rebuild the folder index with notes (NOTE-02).
pub(crate) fn migrate_note_search(tx: &Connection) -> Result<()> {
    tx.execute_batch("DROP TABLE IF EXISTS folder_fts; DROP TRIGGER IF EXISTS folder_ai; DROP TRIGGER IF EXISTS folder_ad; DROP TRIGGER IF EXISTS folder_au;")?;
    migrate_search(tx)
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

/// v7 migration step: the tree's expand state, kept per folder (FLD-08). Collapsed by default.
pub(crate) fn migrate_folded(tx: &Connection) -> Result<()> {
    let have: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('folder') WHERE name = 'folded')", [], |r| r.get(0))?;
    if !have {
        tx.execute_batch("ALTER TABLE folder ADD COLUMN folded INTEGER NOT NULL DEFAULT 1")?;
    }
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

pub(crate) const FOLDER_COLS: &str = "id, parent_id, name, position, note, added, modified, folded, coalesce(role = 'new_items', 0)";

pub(crate) fn folder_row(r: &rusqlite::Row) -> Result<Folder> {
    Ok(Folder {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        position: r.get(3)?,
        note: r.get(4)?,
        added: r.get(5)?,
        modified: r.get(6)?,
        folded: r.get(7)?,
        new_items: r.get(8)?,
    })
}

/// Is `folder` the same as `target` or one of its ancestors (so `target` is in its subtree)?
fn inside(tx: &Connection, folder: i64, target: i64) -> Result<bool> {
    tx.query_row(
        "WITH RECURSIVE up(id) AS (SELECT ?2 UNION SELECT f.parent_id FROM folder f JOIN up ON f.id = up.id WHERE f.parent_id IS NOT NULL)
         SELECT EXISTS(SELECT 1 FROM up WHERE id = ?1)",
        params![folder, target],
        |r| r.get(0),
    )
}

/// Where a move puts its items: next to this child of the target folder.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Beside {
    /// True if `id` is a folder, false for a bookmark or note.
    pub folder: bool,
    pub id: i64,
    /// After it rather than before.
    #[serde(default)]
    pub after: bool,
}

pub(crate) fn exists(tx: &Connection, folder: i64) -> Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM folder WHERE id = ?1)", [folder], |r| r.get(0))
}

/// A new folder last in `parent` (an import's; no undo step of its own).
pub(crate) fn insert_folder(tx: &Connection, parent: i64, name: &str, note: &str, added: Option<i64>) -> Result<i64> {
    tx.execute(
        "INSERT INTO folder (parent_id, name, note, added, position) VALUES (?1, ?2, ?3, coalesce(?4, unixepoch()),
             1 + coalesce((SELECT max(p) FROM (SELECT position AS p FROM bookmark WHERE folder_id = ?1
                                               UNION ALL SELECT position FROM folder WHERE parent_id = ?1)), 0))",
        params![parent, name.trim(), note, added],
    )?;
    Ok(tx.last_insert_rowid())
}

// Changes to the tree are undo steps (DATA-04). Imports build trees in `Store::import_tree`.
impl Store {
    pub fn folder(&self, id: i64) -> Result<Option<Folder>> {
        self.conn
            .query_row(&format!("SELECT {FOLDER_COLS} FROM folder WHERE id = ?1"), [id], folder_row)
            .optional()
    }

    /// Every folder, the root first, then by parent and position: enough to draw the tree.
    // ponytail: the whole tree in one call; page it by parent if folders run to tens of thousands.
    pub fn folders(&self) -> Result<Vec<Folder>> {
        self.conn
            .prepare(&format!("SELECT {FOLDER_COLS} FROM folder ORDER BY parent_id IS NOT NULL, parent_id, position, id"))?
            .query_map([], folder_row)?
            .collect()
    }

    /// Expand (false) or collapse (true) a folder in the tree. False if it doesn't exist.
    pub fn set_folded(&self, id: i64, folded: bool) -> Result<bool> {
        Ok(self.conn.execute("UPDATE folder SET folded = ?2 WHERE id = ?1", params![id, folded])? == 1)
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

    /// New folder in `parent` at index `at` (None: last), one undo step. None if `parent`
    /// doesn't exist.
    pub fn create_folder(&mut self, parent: i64, name: &str, at: Option<i64>) -> Result<Option<i64>> {
        let name = name.trim().to_string();
        self.record_tree(
            &[parent],
            |s| {
                let tx = s.conn.transaction()?;
                if !exists(&tx, parent)? {
                    return Ok(None);
                }
                tx.execute("INSERT INTO folder (parent_id, name, position) VALUES (?1, ?2, 0)", params![parent, name])?;
                let id = tx.last_insert_rowid();
                place(&tx, Item::Folder(id), parent, at)?;
                tx.commit()?;
                Ok(Some(id))
            },
            |_, _| format!("New folder \u{201c}{name}\u{201d}"),
        )
    }

    /// Rename a folder, one undo step. False if it doesn't exist.
    pub fn rename_folder(&mut self, id: i64, name: &str) -> Result<bool> {
        let name = name.trim().to_string();
        self.record_tree(
            &[id],
            |s| Ok(s.conn.execute("UPDATE folder SET name = ?2, modified = unixepoch() WHERE id = ?1", params![id, name])? == 1),
            |_, _| format!("Rename folder \u{201c}{name}\u{201d}"),
        )
    }

    /// Set a folder's note (NOTE-02), one undo step. False if it doesn't exist.
    pub fn set_folder_note(&mut self, id: i64, note: &str) -> Result<bool> {
        let note = note.trim().to_string();
        self.record_tree(
            &[id],
            |s| Ok(s.conn.execute("UPDATE folder SET note = ?2, modified = CASE WHEN note IS ?2 THEN modified ELSE unixepoch() END WHERE id = ?1", params![id, note])? == 1),
            |s, _| format!("Note on \u{201c}{}\u{201d}", s.folder(id).ok().flatten().map(|f| f.name).unwrap_or_default()),
        )
    }

    /// Move a folder (with everything in it) into `parent` at index `at`. Refused (false)
    /// for the root, a missing folder or parent, or a parent inside the folder itself.
    pub fn move_folder(&mut self, id: i64, parent: i64, at: Option<i64>) -> Result<bool> {
        let parents = [self.parent_of(Item::Folder(id))?.unwrap_or(ROOT), parent];
        self.record_tree(
            &parents,
            |s| {
                let tx = s.conn.transaction()?;
                if id == ROOT || inside(&tx, id, parent)? || !exists(&tx, id)? || !exists(&tx, parent)? {
                    return Ok(false);
                }
                place(&tx, Item::Folder(id), parent, at)?;
                tx.execute("UPDATE folder SET modified = unixepoch() WHERE id = ?1", [id])?;
                tx.commit()?;
                Ok(true)
            },
            |s, _| format!("Move folder \u{201c}{}\u{201d}", s.folder(id).ok().flatten().map(|f| f.name).unwrap_or_default()),
        )
    }

    /// Delete a folder, one undo step. Nothing in it is deleted: its contents move up to the
    /// parent, in their order, where the folder was. The root can't be deleted (false).
    pub fn delete_folder(&mut self, id: i64) -> Result<bool> {
        let Some(gone) = self.folder(id)? else { return Ok(false) };
        let Some(parent) = gone.parent_id else { return Ok(false) }; // the root
        self.record_tree(
            &[parent, id],
            |s| {
                let tx = s.conn.transaction()?;
                let kids = siblings(&tx, id)?;
                let order: Vec<Item> =
                    siblings(&tx, parent)?.into_iter().flat_map(|i| if i == Item::Folder(id) { kids.clone() } else { vec![i] }).collect();
                set_order(&tx, parent, &order)?;
                tx.execute("DELETE FROM folder WHERE id = ?1", [id])?;
                tx.commit()?;
                Ok(true)
            },
            |_, _| format!("Delete folder \u{201c}{}\u{201d}", gone.name),
        )
    }

    /// Move a bookmark (or note) into `folder` at index `at` (None: last), one undo step.
    /// False if either is missing.
    pub fn move_bookmark(&mut self, id: i64, folder: i64, at: Option<i64>) -> Result<bool> {
        let parents = [self.parent_of(Item::Bookmark(id))?.unwrap_or(ROOT), folder];
        self.record_tree(
            &parents,
            |s| {
                let tx = s.conn.transaction()?;
                let found: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bookmark WHERE id = ?1)", [id], |r| r.get(0))?;
                if !found || !exists(&tx, folder)? {
                    return Ok(false);
                }
                place(&tx, Item::Bookmark(id), folder, at)?;
                tx.commit()?;
                Ok(true)
            },
            |_, _| "Move 1 item".into(),
        )
    }

    /// Move folders and bookmarks (or notes) into `to`, in the order given (folders first),
    /// at the end or next to `beside` (a child of `to`), as one undo step: Move to folder,
    /// drag and drop, reorder (ITEM-16, FLD-07). Refused (0) if `to` is missing, or a folder
    /// is the root, missing, or would go inside itself. Bookmarks gone meanwhile are skipped.
    /// Returns how many items moved.
    pub fn move_items(&mut self, folders: &[i64], bookmarks: &[i64], to: i64, beside: Option<Beside>) -> Result<usize> {
        let mut parents = vec![to];
        for item in folders.iter().map(|&f| Item::Folder(f)).chain(bookmarks.iter().map(|&b| Item::Bookmark(b))) {
            parents.extend(self.parent_of(item)?);
        }
        parents.sort();
        parents.dedup();
        let moved = self.record_tree(
            &parents,
            |s| {
                let tx = s.conn.transaction()?;
                if !exists(&tx, to)? {
                    return Ok(Vec::new());
                }
                for &f in folders {
                    if f == ROOT || !exists(&tx, f)? || inside(&tx, f, to)? {
                        return Ok(Vec::new());
                    }
                }
                let mut items: Vec<Item> = folders.iter().map(|&f| Item::Folder(f)).collect();
                for &b in bookmarks {
                    if tx.query_row("SELECT EXISTS(SELECT 1 FROM bookmark WHERE id = ?1)", [b], |r| r.get(0))? {
                        items.push(Item::Bookmark(b));
                    }
                }
                items.dedup();
                let mut order: Vec<Item> = siblings(&tx, to)?.into_iter().filter(|i| !items.contains(i)).collect();
                let at = beside
                    .and_then(|b| {
                        let anchor = if b.folder { Item::Folder(b.id) } else { Item::Bookmark(b.id) };
                        order.iter().position(|i| *i == anchor).map(|p| p + b.after as usize)
                    })
                    .unwrap_or(order.len());
                order.splice(at..at, items.iter().copied());
                set_order(&tx, to, &order)?;
                for &f in folders {
                    tx.execute("UPDATE folder SET modified = unixepoch() WHERE id = ?1", [f])?;
                }
                tx.commit()?;
                Ok(items)
            },
            |s, items| match items.as_slice() {
                [Item::Folder(f)] => format!("Move folder \u{201c}{}\u{201d}", s.folder(*f).ok().flatten().map(|f| f.name).unwrap_or_default()),
                [Item::Bookmark(b)] => {
                    let b = s.get(*b).ok().flatten().map(|b| if b.title.is_empty() { b.url } else { b.title }).unwrap_or_default();
                    format!("Move \u{201c}{b}\u{201d}")
                }
                _ => format!("Move {} items to \u{201c}{}\u{201d}", items.len(), s.folder(to).ok().flatten().map(|f| f.name).unwrap_or_default()),
            },
        )?;
        Ok(moved.len())
    }

    /// The folder an item sits in (None if it's missing or is the root).
    fn parent_of(&self, item: Item) -> Result<Option<i64>> {
        let (sql, id) = match item {
            Item::Folder(id) => ("SELECT parent_id FROM folder WHERE id = ?1", id),
            Item::Bookmark(id) => ("SELECT folder_id FROM bookmark WHERE id = ?1", id),
        };
        Ok(self.conn.query_row(sql, [id], |r| r.get::<_, Option<i64>>(0)).optional()?.flatten())
    }

    /// The New Item Folder (ITEM-04), if one is set and still exists.
    pub fn new_item_folder(&self) -> Result<Option<i64>> {
        find_inbox(&self.conn)
    }

    /// Make `id` the New Item Folder (folder context menu), one undo step. False if it's missing.
    pub fn set_new_item_folder(&mut self, id: i64) -> Result<bool> {
        let Some(f) = self.folder(id)? else { return Ok(false) };
        let old = find_inbox(&self.conn)?;
        self.record_tree(
            &[Some(id), old].into_iter().flatten().collect::<Vec<_>>(),
            |s| {
                s.conn.execute(
                    "UPDATE folder SET role = CASE WHEN id = ?1 THEN 'new_items' END WHERE role = 'new_items' OR id = ?1",
                    [id],
                )?;
                Ok(true)
            },
            |_, _| format!("Set New Item Folder \u{201c}{}\u{201d}", f.name),
        )
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

}

#[cfg(test)]
mod tests {
    use super::*;
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
            for ext in ["", "-wal", "-shm", ".v5.bak", ".v6.bak", ".v7.bak", ".v8.bak"] {
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
        assert!(!crate::export::to_netscape(&crate::export::Tree::flat(rows.clone())).contains("Shopping"));
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
    fn move_items_files_reorders_and_refuses_cycles() {
        let mut s = Store::open(":memory:").unwrap();
        let ids: Vec<i64> = ["1", "2", "3", "4"].iter().map(|t| s.add(&format!("https://{t}.example/"), t, "", "").unwrap().0).collect();
        let f = s.create_folder(ROOT, "F", None).unwrap().unwrap();
        // Several at once, in the order given, at the end of the target.
        assert_eq!(s.move_items(&[], &[ids[2], ids[0]], f, None).unwrap(), 2);
        assert_eq!(names(&s, f), [("b", "3".into()), ("b", "1".into())]);
        assert_eq!(s.history_labels().0.as_deref(), Some("Move 2 items to \u{201c}F\u{201d}"));
        // Reorder within a folder: before or after a sibling (folder or bookmark).
        assert_eq!(s.move_items(&[], &[ids[0]], f, Some(Beside { folder: false, id: ids[2], after: false })).unwrap(), 1);
        assert_eq!(names(&s, f), [("b", "1".into()), ("b", "3".into())]);
        assert_eq!(s.move_items(&[], &[ids[3]], ROOT, Some(Beside { folder: true, id: f, after: false })).unwrap(), 1);
        assert_eq!(names(&s, ROOT), [("b", "2".into()), ("b", "4".into()), ("f", "F".into())]);
        assert_eq!(s.move_items(&[], &[ids[1]], ROOT, Some(Beside { folder: true, id: f, after: true })).unwrap(), 1);
        assert_eq!(names(&s, ROOT), [("b", "4".into()), ("f", "F".into()), ("b", "2".into())]);
        assert_eq!(s.history_labels().0.as_deref(), Some("Move \u{201c}2\u{201d}"));
        // Folders: never into their own subtree, the root never moves; refusals add no step.
        let g = s.create_folder(f, "G", None).unwrap().unwrap();
        let before = s.history_labels();
        assert_eq!(s.move_items(&[f], &[], g, None).unwrap(), 0);
        assert_eq!(s.move_items(&[f], &[], f, None).unwrap(), 0);
        assert_eq!(s.move_items(&[ROOT], &[], f, None).unwrap(), 0);
        assert_eq!(s.move_items(&[], &[ids[0]], 9999, None).unwrap(), 0);
        assert_eq!(s.move_items(&[], &[9999], f, None).unwrap(), 0, "a bookmark gone elsewhere is skipped");
        assert_eq!(s.history_labels(), before);
        assert_eq!(s.move_items(&[g], &[], ROOT, Some(Beside { folder: false, id: ids[3], after: false })).unwrap(), 1);
        assert_eq!(names(&s, ROOT)[0], ("f", "G".into()));
        assert_eq!(s.history_labels().0.as_deref(), Some("Move folder \u{201c}G\u{201d}"));
    }

    #[test]
    fn filing_is_undone_and_redone_step_by_step() {
        let mut s = Store::open(":memory:").unwrap();
        let ids: Vec<i64> = ["1", "2", "3"].iter().map(|t| s.add(&format!("https://{t}.example/"), t, "", "").unwrap().0).collect();
        let start = names(&s, ROOT);
        let f = s.create_folder(ROOT, "F", None).unwrap().unwrap();
        assert!(s.rename_folder(f, "Filed").unwrap());
        s.move_items(&[], &[ids[2], ids[0]], f, None).unwrap();
        let g = s.create_folder(ROOT, "G", Some(0)).unwrap().unwrap();
        s.move_items(&[f], &[], g, None).unwrap();
        assert!(s.delete_folder(g).unwrap());
        let end = (names(&s, ROOT), names(&s, f));
        // Changes the history doesn't own survive an undo of a move: a visit, the expand state.
        s.conn.execute("UPDATE bookmark SET visits = 7 WHERE id = ?1", [ids[0]]).unwrap();
        s.set_folded(f, false).unwrap();

        let undo = |s: &mut Store| s.undo().unwrap().unwrap();
        assert_eq!(undo(&mut s), "Delete folder \u{201c}G\u{201d}");
        assert_eq!(s.folder(f).unwrap().unwrap().parent_id, Some(g));
        assert_eq!(names(&s, ROOT), [("f", "G".into()), ("b", "2".into())]);
        assert_eq!(undo(&mut s), "Move folder \u{201c}Filed\u{201d}");
        assert_eq!(names(&s, ROOT), [("f", "G".into()), ("b", "2".into()), ("f", "Filed".into())]);
        assert_eq!(undo(&mut s), "New folder \u{201c}G\u{201d}");
        assert_eq!(s.folder(g).unwrap(), None);
        assert_eq!(undo(&mut s), "Move 2 items to \u{201c}Filed\u{201d}");
        assert!(names(&s, f).is_empty());
        let b = s.get(ids[0]).unwrap().unwrap();
        assert_eq!((b.folder_id, b.visits), (ROOT, 7));
        assert_eq!(undo(&mut s), "Rename folder \u{201c}Filed\u{201d}");
        assert_eq!(s.folder(f).unwrap().unwrap().name, "F");
        assert!(!s.folder(f).unwrap().unwrap().folded, "expand state kept");
        assert_eq!(undo(&mut s), "New folder \u{201c}F\u{201d}");
        assert_eq!(names(&s, ROOT), start);
        assert_eq!(s.undo().unwrap(), None);

        while s.redo().unwrap().is_some() {}
        assert_eq!((names(&s, ROOT), names(&s, f)), end);
    }

    #[test]
    fn undoing_a_new_folder_keeps_what_arrived_in_it_meanwhile() {
        let mut s = Store::open(":memory:").unwrap();
        let f = s.create_folder(ROOT, "F", None).unwrap().unwrap();
        // The extension (not an undo step) adds into it, then the folder's creation is undone.
        let n = s.add_note(f, "Arrived", "").unwrap().unwrap();
        s.undo().unwrap();
        assert_eq!(s.get(n).unwrap().unwrap().folder_id, ROOT, "lands in the root, not lost");
        assert_eq!(names(&s, ROOT), [("b", "Arrived".into())]);
    }

    #[test]
    fn folder_list_and_expand_state_persist() {
        let db = TempDb::new("fold");
        let (p, c) = {
            let mut s = Store::open(&db.0).unwrap();
            let p = s.create_folder(ROOT, "P", None).unwrap().unwrap();
            let a = s.create_folder(ROOT, "A", Some(0)).unwrap().unwrap();
            let c = s.create_folder(p, "C", None).unwrap().unwrap();
            let all = s.folders().unwrap();
            assert_eq!(all.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Bookmarks", "A", "P", "C"], "root, then by parent and position");
            assert!(all.iter().all(|f| f.folded), "new folders start collapsed");
            assert!(s.set_folded(p, false).unwrap());
            assert!(!s.set_folded(9999, false).unwrap());
            let _ = a;
            (p, c)
        };
        let s = Store::open(&db.0).unwrap();
        assert!(!s.folder(p).unwrap().unwrap().folded, "kept across restarts");
        assert!(s.folder(c).unwrap().unwrap().folded);
    }

    #[test]
    fn v6_database_gains_expand_state() {
        let db = TempDb::new("v7");
        {
            let mut s = Store::open(&db.0).unwrap();
            s.create_folder(ROOT, "Old", None).unwrap();
            s.conn.execute_batch("ALTER TABLE folder DROP COLUMN folded; PRAGMA user_version = 6;").unwrap();
        }
        let s = Store::open(&db.0).unwrap();
        assert_eq!(version(&db.0), crate::SCHEMA_VERSION);
        assert!(s.folders().unwrap().iter().all(|f| f.folded));
        assert_eq!(version(&format!("{}.v6.bak", db.0)), 6);
    }

    #[test]
    fn v7_database_gains_the_folder_path_index() {
        let db = TempDb::new("v8");
        let (f, b) = {
            let mut s = Store::open(&db.0).unwrap();
            let f = s.create_folder(ROOT, "Search engines", None).unwrap().unwrap();
            let (b, _) = s.add("https://g.example/", "G", "", "").unwrap();
            s.move_bookmark(b, f, None).unwrap();
            s.conn.execute_batch("DROP TABLE folder_fts; DROP TRIGGER folder_ai; DROP TRIGGER folder_ad; DROP TRIGGER folder_au; PRAGMA user_version = 7;").unwrap();
            (f, b)
        };
        let s = Store::open(&db.0).unwrap();
        assert_eq!(version(&db.0), crate::SCHEMA_VERSION);
        assert_eq!(s.search_ids("sea eng", &Default::default()).unwrap(), [b], "existing folders indexed");
        let _ = f;
    }

    #[test]
    fn empty_search_lists_the_folder_in_folder_order() {
        use crate::query::{SearchOptions, SortKey};
        let mut s = Store::open(":memory:").unwrap();
        let (a, _) = s.add("https://a.example/", "A", "red", "").unwrap();
        let (b, _) = s.add("https://b.example/", "B", "blue", "").unwrap();
        let (z, _) = s.add("https://z.example/", "Z", "red", "").unwrap();
        let f = s.create_folder(ROOT, "F", None).unwrap().unwrap();
        s.create_folder(f, "Sub", None).unwrap();
        let n = s.add_note(f, "Note", "remember").unwrap().unwrap();
        s.move_bookmark(z, f, Some(0)).unwrap();
        s.move_bookmark(b, f, None).unwrap();
        let in_f = SearchOptions { folder: Some(f), ..Default::default() };
        assert_eq!(s.search_ids("", &in_f).unwrap(), [z, n, b], "bookmarks and notes, folders left out, stored order");
        assert_eq!(s.search_ids("", &SearchOptions { folder: Some(ROOT), ..Default::default() }).unwrap(), [a]);
        let by_title = SearchOptions { sort: vec![SortKey { column: "title".into(), desc: false }], ..in_f.clone() };
        assert_eq!(s.search_ids("", &by_title).unwrap(), [b, n, z], "a header sort is only a view");
        assert_eq!(s.search_ids("", &in_f).unwrap(), [z, n, b], "stored order untouched");
        // A search covers the whole collection unless scoped to the folder (FIND-08).
        let mut found = s.search_ids("red", &in_f).unwrap();
        found.sort();
        assert_eq!(found, [a, z]);
        // The keyword pane follows the folder while the search is empty.
        let kws: Vec<String> = s.keywords_with("", &in_f).unwrap().into_iter().map(|k| k.keyword).collect();
        assert_eq!(kws, ["blue", "red"]);
        assert_eq!(s.keywords_with("", &SearchOptions::default()).unwrap().len(), 2);
    }

    /// Search engines/Web/(g), Search engines/(d), Work/Engineering/(e), root/(r).
    fn search_tree(s: &mut Store) -> (i64, i64, [i64; 4]) {
        let se = s.create_folder(ROOT, "Search engines", None).unwrap().unwrap();
        let web = s.create_folder(se, "Web", None).unwrap().unwrap();
        let work = s.create_folder(ROOT, "Work", None).unwrap().unwrap();
        let eng = s.create_folder(work, "Engineering", None).unwrap().unwrap();
        let (g, _) = s.add("https://g.example/", "Google", "red", "").unwrap();
        let (d, _) = s.add("https://d.example/", "DuckDuckGo", "red blue", "").unwrap();
        let (e, _) = s.add("https://e.example/", "Build server", "blue", "").unwrap();
        let (r, _) = s.add("https://r.example/", "Recipes", "red", "").unwrap();
        s.move_bookmark(g, web, None).unwrap();
        s.move_bookmark(d, se, None).unwrap();
        s.move_bookmark(e, eng, None).unwrap();
        let _ = r;
        (se, work, [g, d, e, r])
    }

    fn sorted(mut v: Vec<i64>) -> Vec<i64> {
        v.sort();
        v
    }

    #[test]
    fn folder_path_is_searchable() {
        use crate::query::{Mode, SearchOptions};
        let mut s = Store::open(":memory:").unwrap();
        let (se, work, [g, d, e, r]) = search_tree(&mut s);
        let all = SearchOptions::default();
        let ids = |s: &Store, q: &str, o: &SearchOptions| sorted(s.search_ids(q, o).unwrap());
        // Every word of the path counts, at any depth below the matching folder (FIND-07).
        assert_eq!(ids(&s, "sea eng", &all), [g, d], "\"sea eng\" finds a Search engines folder");
        assert_eq!(ids(&s, "web", &all), [g]);
        assert_eq!(ids(&s, "engineering google", &all), Vec::<i64>::new(), "both words, one item");
        assert_eq!(ids(&s, "work build", &all), [e], "path word and title word together");
        assert_eq!(ids(&s, "bookmarks", &all), Vec::<i64>::new(), "the root's name is not part of a path");
        // Operators and whole words work on the path too.
        assert_eq!(ids(&s, "red ! search", &all), [r]);
        assert_eq!(ids(&s, "web | work", &all), [g, e]);
        assert_eq!(ids(&s, "engine.", &all), Vec::<i64>::new(), "whole word: engine. is not engines");
        assert_eq!(ids(&s, "engines.", &all), [g, d]);
        // Rename and move re-index (rename: the folder's own name; move: the path follows the tree).
        s.rename_folder(work, "Office").unwrap();
        assert_eq!(ids(&s, "work", &all), Vec::<i64>::new());
        assert_eq!(ids(&s, "office", &all), [e]);
        s.move_folder(work, se, None).unwrap();
        assert_eq!(ids(&s, "search build", &all), [e], "moved under Search engines");
        s.undo().unwrap();
        assert_eq!(ids(&s, "search build", &all), Vec::<i64>::new(), "undo of the move");
        s.undo().unwrap();
        assert_eq!(ids(&s, "work", &all), [e], "undo of the rename");
        // Keywords mode searches the keywords field only, not the path.
        let kw = SearchOptions { mode: Mode::Keywords, ..all.clone() };
        assert_eq!(ids(&s, "web", &kw), Vec::<i64>::new());
        // Exact mode: Folder is one of the fields, matched against the path (FIND-06).
        let exact = |fields: &[&str]| SearchOptions { mode: Mode::Exact, fields: fields.iter().map(|f| f.to_string()).collect(), ..all.clone() };
        assert_eq!(ids(&s, "engines › web", &exact(&["folder"])), [g]);
        assert_eq!(ids(&s, "work › eng", &exact(&["folder"])), [e]);
        assert_eq!(ids(&s, "web", &exact(&["title"])), Vec::<i64>::new(), "folder not chosen");
        let cased = SearchOptions { match_case: true, ..exact(&["folder"]) };
        assert_eq!(ids(&s, "web", &cased), Vec::<i64>::new());
        assert_eq!(ids(&s, "Web", &cased), [g]);
        assert!(crate::FIELDS.contains(&"folder"), "exact mode offers the folder by default");
    }

    #[test]
    fn search_scoped_to_a_folder_and_below() {
        use crate::query::{Mode, SearchOptions};
        let mut s = Store::open(":memory:").unwrap();
        let (se, work, [g, d, e, r]) = search_tree(&mut s);
        let ids = |q: &str, o: &SearchOptions| sorted(s.search_ids(q, o).unwrap());
        let everywhere = SearchOptions { folder: Some(se), ..Default::default() };
        let here = SearchOptions { in_folder: true, ..everywhere.clone() };
        assert_eq!(ids("red", &everywhere), [g, d, r], "default: the whole collection");
        assert_eq!(ids("red", &here), [g, d], "the folder and its subfolders");
        assert_eq!(ids("red ! google", &here), [d], "operators combine with the scope");
        assert_eq!(ids("(red | blue) ! duckduckgo.", &here), [g]);
        assert_eq!(ids("(red | blue) ! duck.", &here), [g, d], "whole word: duck. is not DuckDuckGo");
        assert_eq!(ids("blue | build", &SearchOptions { folder: Some(work), ..here.clone() }), [e]);
        assert_eq!(ids("red", &SearchOptions { folder: Some(ROOT), ..here.clone() }), [g, d, r], "the root: everything");
        let exact = SearchOptions { mode: Mode::Exact, fields: vec!["url".into()], ..here.clone() };
        assert_eq!(ids(".example", &exact), [g, d], "exact mode is scoped too");
        // The empty search still lists only the folder itself, in folder order.
        assert_eq!(s.search_ids("", &here).unwrap(), [d]);
        // The keyword pane follows the scope.
        let kws: Vec<(String, i64)> = s.keywords_with("red", &here).unwrap().into_iter().map(|k| (k.keyword, k.count)).collect();
        assert_eq!(kws, [("blue".to_string(), 1), ("red".to_string(), 2)]);
        // Scope is a search setting: it round-trips through the options JSON (FIND-11).
        let o: SearchOptions = serde_json::from_str(r#"{"in_folder": true, "folder": 2}"#).unwrap();
        assert!(o.in_folder && o.folder == Some(2));
        assert!(!serde_json::from_str::<SearchOptions>("{}").unwrap().in_folder, "off unless chosen");
    }

    fn link(url: &str) -> crate::NewBookmark {
        crate::NewBookmark { url: url.into(), title: url.into(), keywords: "new".into(), description: String::new(), notes: String::new(), added: None }
    }

    #[test]
    fn new_item_folder_is_made_on_first_use_and_survives_rename() {
        let mut s = Store::open(":memory:").unwrap();
        assert_eq!(s.new_item_folder().unwrap(), None, "unset until first needed");
        let r = s.import_bookmarks("Add 1 link from the clipboard", NEW_ITEMS, &[link("https://a.example/")]).unwrap();
        let inbox = s.new_item_folder().unwrap().expect("created by the add");
        let f = s.folder(inbox).unwrap().unwrap();
        assert_eq!((f.name.as_str(), f.parent_id, f.new_items), ("New", Some(ROOT), true), "root-level \u{201c}New\u{201d} (ITEM-04)");
        assert_eq!(s.get(r.ids[0]).unwrap().unwrap().folder_id, inbox);
        // The role is the folder's, not its name: a rename keeps it.
        s.rename_folder(inbox, "Inbox").unwrap();
        let (b, _) = s.add_in(NEW_ITEMS, "https://b.example/", "B", "new", "").unwrap();
        assert_eq!(s.get(b).unwrap().unwrap().folder_id, inbox, "extension adds land there too");
        assert_eq!(s.folders().unwrap().len(), 2, "no second inbox");
        // Undoing the add that made it takes the folder away with it.
        s.undo().unwrap(); // rename
        s.undo().unwrap(); // the clipboard add
        assert_eq!(s.folder(inbox).unwrap(), None);
    }

    #[test]
    fn set_new_item_folder_keeps_one_and_undoes() {
        let mut s = Store::open(":memory:").unwrap();
        let a = s.create_folder(ROOT, "A", None).unwrap().unwrap();
        let p = s.create_folder(ROOT, "P", None).unwrap().unwrap();
        let b = s.create_folder(p, "B", None).unwrap().unwrap();
        assert!(s.set_new_item_folder(a).unwrap());
        assert!(s.set_new_item_folder(b).unwrap());
        assert_eq!(s.new_item_folder().unwrap(), Some(b));
        assert!(!s.folder(a).unwrap().unwrap().new_items, "one at most");
        assert_eq!(s.history_labels().0.as_deref(), Some("Set New Item Folder \u{201c}B\u{201d}"));
        s.undo().unwrap();
        assert_eq!(s.new_item_folder().unwrap(), Some(a));
        assert!(!s.set_new_item_folder(999).unwrap(), "missing folder refused");
        let (x, _) = s.add_in(NEW_ITEMS, "https://x.example/", "X", "", "").unwrap();
        assert_eq!(s.get(x).unwrap().unwrap().folder_id, a);
        let (y, _) = s.add_in(p, "https://y.example/", "Y", "", "").unwrap();
        assert_eq!(s.get(y).unwrap().unwrap().folder_id, p, "a chosen folder (popup picker)");
    }

    #[test]
    fn deleting_the_new_item_folder_falls_back_safely() {
        let mut s = Store::open(":memory:").unwrap();
        let a = s.create_folder(ROOT, "Reading", None).unwrap().unwrap();
        s.set_new_item_folder(a).unwrap();
        s.delete_folder(a).unwrap();
        assert_eq!(s.new_item_folder().unwrap(), None);
        // A folder picked in a stale popup that is gone: the root.
        let (x, _) = s.add_in(a, "https://x.example/", "X", "", "").unwrap();
        assert_eq!(s.get(x).unwrap().unwrap().folder_id, ROOT);
        // The next inbox add recreates it.
        let (y, _) = s.add_in(NEW_ITEMS, "https://y.example/", "Y", "", "").unwrap();
        let again = s.new_item_folder().unwrap().unwrap();
        let f = s.folder(again).unwrap().unwrap();
        assert_eq!((f.name.as_str(), f.parent_id, s.get(y).unwrap().unwrap().folder_id), ("New", Some(ROOT), again));
        // Undoing the delete brings the old one back as the inbox.
        s.undo().unwrap();
        let back = s.new_item_folder().unwrap().unwrap();
        assert_eq!(s.folder(back).unwrap().unwrap().name, "Reading");
    }

    #[test]
    fn v8_database_gains_folder_roles() {
        let db = TempDb::new("v9");
        {
            let s = Store::open(&db.0).unwrap();
            s.conn.execute_batch("ALTER TABLE folder DROP COLUMN role; PRAGMA user_version = 8;").unwrap();
        }
        let s = Store::open(&db.0).unwrap();
        assert_eq!(version(&db.0), crate::SCHEMA_VERSION);
        assert_eq!(s.new_item_folder().unwrap(), None);
    }

    #[test]
    fn folder_notes_are_edited_undone_and_searched() {
        let mut s = Store::open(":memory:").unwrap();
        let f = s.create_folder(ROOT, "Reading", None).unwrap().unwrap();
        let sub = s.create_folder(f, "Later", None).unwrap().unwrap();
        let (b, _) = s.add("https://a.example/", "A", "", "").unwrap();
        s.move_bookmark(b, sub, None).unwrap();
        assert!(s.set_folder_note(f, "  articles about gardening ").unwrap());
        assert_eq!(s.folder(f).unwrap().unwrap().note, "articles about gardening");
        assert_eq!(s.history_labels().0.as_deref(), Some("Note on \u{201c}Reading\u{201d}"));
        assert_eq!(s.search_ids("garden", &Default::default()).unwrap(), [b], "a folder's note searches with its path");
        assert!(s.set_folder_note(f, "articles about gardening").unwrap(), "unchanged: still there");
        s.undo().unwrap();
        assert_eq!(s.folder(f).unwrap().unwrap().note, "");
        assert!(s.search_ids("garden", &Default::default()).unwrap().is_empty(), "undo re-indexes");
        s.redo().unwrap();
        assert_eq!(s.folder(f).unwrap().unwrap().note, "articles about gardening");
        assert!(!s.set_folder_note(9999, "x").unwrap(), "missing folder");
    }
}
