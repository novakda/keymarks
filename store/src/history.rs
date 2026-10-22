//! Session undo/redo and restoring backups.
//!
//! Each undoable change snapshots the bookmark rows it touches (and, for keyword-dictionary
//! changes, the tagged-keyword set) before and after. Undo writes the "before" rows back,
//! redo the "after" rows. Only those rows are touched, so bookmarks the browser extension
//! adds meanwhile are never disturbed.

use crate::{Result, Store, index_keywords};
use rusqlite::{Connection, OpenFlags, backup::Backup, params_from_iter, types::Value};

const MAX_STEPS: usize = 50;

#[derive(Default)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

struct Step {
    name: String,
    ids: Vec<i64>,
    before: Snapshot,
    after: Snapshot,
}

/// Full rows of the touched bookmarks (a missing id means it didn't exist) and, if the
/// change can affect it, the whole tagged-keyword set.
struct Snapshot {
    rows: Vec<Vec<Value>>,
    tagged: Option<Vec<String>>,
}

impl Store {
    /// Run `change` as one named undo step covering bookmarks `ids` (and tags if `tags`).
    pub(crate) fn record<T>(
        &mut self,
        name: String,
        ids: &[i64],
        tags: bool,
        change: impl FnOnce(&mut Store) -> Result<T>,
    ) -> Result<T> {
        let before = self.snapshot(ids, tags)?;
        let out = change(self)?;
        let after = self.snapshot(ids, tags)?;
        let h = &mut self.history;
        h.redo.clear();
        h.undo.push(Step { name, ids: ids.to_vec(), before, after });
        if h.undo.len() > MAX_STEPS {
            h.undo.remove(0);
        }
        Ok(out)
    }

    /// Like `record`, for changes that create bookmarks: `change` returns the new ids, which
    /// can't be known beforehand. Undo deletes them; redo puts them back.
    pub(crate) fn record_created<T>(
        &mut self,
        name: String,
        change: impl FnOnce(&mut Store) -> Result<(T, Vec<i64>)>,
    ) -> Result<T> {
        let (out, ids) = change(self)?;
        let after = self.snapshot(&ids, false)?;
        let h = &mut self.history;
        h.redo.clear();
        h.undo.push(Step { name, ids, before: Snapshot { rows: Vec::new(), tagged: None }, after });
        if h.undo.len() > MAX_STEPS {
            h.undo.remove(0);
        }
        Ok(out)
    }

    /// Undo the most recent change. Returns its name, or None if there is nothing to undo.
    pub fn undo(&mut self) -> Result<Option<String>> {
        self.step(true)
    }

    /// Redo the most recently undone change.
    pub fn redo(&mut self) -> Result<Option<String>> {
        self.step(false)
    }

    /// Names of the changes Undo and Redo would apply next.
    pub fn history_labels(&self) -> (Option<String>, Option<String>) {
        let label = |v: &Vec<Step>| v.last().map(|s| s.name.clone());
        (label(&self.history.undo), label(&self.history.redo))
    }

    fn step(&mut self, undo: bool) -> Result<Option<String>> {
        let popped = if undo { self.history.undo.pop() } else { self.history.redo.pop() };
        let Some(step) = popped else { return Ok(None) };
        let snap = if undo { &step.before } else { &step.after };
        if let Err(e) = self.restore_rows(&step.ids, snap) {
            // Nothing was committed; keep the step so the user can try again.
            if undo { self.history.undo.push(step) } else { self.history.redo.push(step) }
            return Err(e);
        }
        let name = step.name.clone();
        if undo { self.history.redo.push(step) } else { self.history.undo.push(step) }
        Ok(Some(name))
    }

    fn snapshot(&self, ids: &[i64], tags: bool) -> Result<Snapshot> {
        let json = serde_json::to_string(ids).expect("ids serialize");
        let mut stmt = self.conn.prepare("SELECT * FROM bookmark WHERE id IN (SELECT value FROM json_each(?1))")?;
        let width = stmt.column_count();
        let rows = stmt
            .query_map([json], |r| (0..width).map(|i| r.get::<_, Value>(i)).collect())?
            .collect::<Result<_>>()?;
        let tagged = if tags {
            Some(self.conn.prepare("SELECT keyword FROM tagged_keyword")?.query_map([], |r| r.get(0))?.collect::<Result<_>>()?)
        } else {
            None
        };
        Ok(Snapshot { rows, tagged })
    }

    // ponytail: restores whole rows, so a visit counted between a change and its undo is
    // rolled back with it; track visits outside the snapshot if that ever matters.
    fn restore_rows(&mut self, ids: &[i64], snap: &Snapshot) -> Result<()> {
        let json = serde_json::to_string(ids).expect("ids serialize");
        let tx = self.conn.transaction()?;
        // Triggers keep the full-text index and keyword index in step with these writes.
        tx.execute("DELETE FROM bookmark WHERE id IN (SELECT value FROM json_each(?1))", [&json])?;
        for row in &snap.rows {
            let marks = vec!["?"; row.len()].join(", ");
            tx.execute(&format!("INSERT INTO bookmark VALUES ({marks})"), params_from_iter(row))?;
        }
        let restored: Vec<(i64, String)> = tx
            .prepare("SELECT id, keywords FROM bookmark WHERE id IN (SELECT value FROM json_each(?1))")?
            .query_map([&json], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;
        for (id, keywords) in restored {
            index_keywords(&tx, id, &keywords)?;
        }
        if let Some(tagged) = &snap.tagged {
            tx.execute("DELETE FROM tagged_keyword", [])?;
            for k in tagged {
                tx.execute("INSERT INTO tagged_keyword (keyword) VALUES (?1)", [k])?;
            }
        }
        tx.commit()
    }

    /// Replace the whole database with a backup file's contents (SQLite online backup,
    /// safe while other connections are open), then upgrade it if it is older. Clears undo.
    pub fn restore_from(&mut self, path: &str) -> Result<()> {
        let src = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Backup::new(&src, &mut self.conn)?.run_to_completion(1000, std::time::Duration::ZERO, None)?;
        self.migrate()?;
        self.history = History::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Bookmark, Store};

    fn titles(s: &Store) -> Vec<String> {
        let mut t: Vec<_> = s.search("", 100).unwrap().into_iter().map(|b| b.title).collect();
        t.sort();
        t
    }

    #[test]
    fn undo_redo_each_kind_of_change() {
        let mut s = Store::open(":memory:").unwrap();
        let (a, _) = s.add("https://a.example/", "A", "alpha shared", "").unwrap();
        let (b, _) = s.add("https://b.example/", "B", "beta shared", "").unwrap();
        assert_eq!(s.history_labels(), (None, None)); // adds come from the extension: not undoable here

        // Edit
        let mut bm: Bookmark = s.get(a).unwrap().unwrap();
        bm.title = "A2".into();
        bm.keywords = "gamma".into();
        s.update(&bm).unwrap();
        assert_eq!(s.history_labels().0.as_deref(), Some("Edit \"A2\""));
        assert_eq!(s.undo().unwrap().as_deref(), Some("Edit \"A2\""));
        assert_eq!(titles(&s), ["A", "B"]);
        assert_eq!(s.search("alpha", 5).unwrap().len(), 1, "full-text index restored");
        assert!(s.search("gamma", 5).unwrap().is_empty());
        assert_eq!(s.redo().unwrap().as_deref(), Some("Edit \"A2\""));
        assert_eq!(titles(&s), ["A2", "B"]);
        s.undo().unwrap();

        // Delete
        s.delete(&[a, b]).unwrap();
        assert!(titles(&s).is_empty());
        assert_eq!(s.undo().unwrap().as_deref(), Some("Delete 2 bookmarks"));
        assert_eq!(titles(&s), ["A", "B"]);
        assert_eq!(s.get(a).unwrap().unwrap().id, a, "ids preserved");
        assert!(s.keywords("").unwrap().iter().any(|k| k.keyword == "shared" && k.count == 2), "keyword index restored");

        // Keyword rename + tag, undone together with their tag state
        s.set_tagged("shared", true).unwrap();
        s.rename_keyword("shared", "common").unwrap();
        assert!(s.keywords("").unwrap().iter().any(|k| k.keyword == "common" && k.tagged));
        s.undo().unwrap();
        let kw = s.keywords("").unwrap();
        assert!(kw.iter().any(|k| k.keyword == "shared" && k.tagged && k.count == 2));
        assert!(kw.iter().all(|k| k.keyword != "common"));
        s.undo().unwrap(); // the tag
        assert!(s.keywords("").unwrap().iter().all(|k| !k.tagged));

        // Cleanup
        s.apply_cleanup(&[("alpha".into(), String::new())]).unwrap();
        assert!(s.search("alpha", 5).unwrap().is_empty());
        assert_eq!(s.undo().unwrap().as_deref(), Some("Keyword cleanup (1 keywords)"));
        assert_eq!(s.search("alpha", 5).unwrap().len(), 1);

        // A new change clears redo; history is capped.
        s.redo().unwrap();
        s.undo().unwrap();
        s.set_tagged("beta", true).unwrap();
        assert_eq!(s.history_labels().1, None);
        for _ in 0..60 {
            s.set_tagged("beta", true).unwrap();
        }
        let mut n = 0;
        while s.undo().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 50);
    }

    #[test]
    fn restore_from_backup_replaces_everything() {
        let dir = std::env::temp_dir();
        let live = dir.join(format!("store-restore-{}.db", std::process::id()));
        let bak = dir.join(format!("store-restore-{}.bak.db", std::process::id()));
        let _ = std::fs::remove_file(&live);
        let _ = std::fs::remove_file(&bak);
        let mut s = Store::open(live.to_str().unwrap()).unwrap();
        s.add("https://keep.example/", "Keep", "k", "").unwrap();
        s.backup_to(bak.to_str().unwrap()).unwrap();
        let (gone, _) = s.add("https://later.example/", "Later", "l", "").unwrap();
        s.delete(&[gone]).unwrap();
        s.add("https://after.example/", "After", "a", "").unwrap();

        s.restore_from(bak.to_str().unwrap()).unwrap();
        assert_eq!(titles(&s), ["Keep"]);
        assert_eq!(s.history_labels(), (None, None));
        assert_eq!(s.search("k", 5).unwrap().len(), 1);
        drop(s);
        for p in [&live, &bak] {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
            }
        }
    }
}
