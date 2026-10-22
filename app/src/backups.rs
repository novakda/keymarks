//! Backup files in `<data dir>/backups`:
//! - `auto-<time>.db` at every startup, newest 10 kept;
//! - `largest-<count>-<time>.db`, replaced only when the collection is at least that big
//!   (the original's `.ba1` idea: survives a mass delete that later backups would copy);
//! - `before-cleanup-…`, `before-restore-…` and `manual-…` taken on those occasions.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use store::Store;

const KEEP_AUTO: usize = 10;

#[derive(serde::Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub kind: &'static str,
    /// Unix seconds.
    pub created: u64,
    pub bytes: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn kind(name: &str) -> &'static str {
    match name.split('-').next() {
        Some("auto") => "Automatic",
        Some("largest") => "Largest collection",
        Some("manual") => "Manual",
        _ if name.starts_with("before-cleanup") => "Before keyword cleanup",
        _ if name.starts_with("before-restore") => "Before restore",
        _ => "Other",
    }
}

/// Write a consistent copy named `<prefix>-<unix time>.db` (with `-2`, `-3`… if several are
/// taken in the same second); returns its file name.
pub fn take(store: &Store, dir: &Path, prefix: &str) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = now();
    let name = (1..)
        .map(|n| if n == 1 { format!("{prefix}-{stamp}.db") } else { format!("{prefix}-{stamp}-{n}.db") })
        .find(|name| !dir.join(name).exists())
        .expect("unbounded");
    let path = dir.join(&name);
    store.backup_to(path.to_str().ok_or("backup path is not UTF-8")?).map_err(|e| e.to_string())?;
    Ok(name)
}

/// Newest first.
pub fn list(dir: &Path) -> Vec<BackupInfo> {
    let mut out: Vec<BackupInfo> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok()?;
            let created = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            name.ends_with(".db").then(|| BackupInfo { kind: kind(&name), name, created, bytes: meta.len() })
        })
        .collect();
    out.sort_by(|a, b| b.created.cmp(&a.created).then(b.name.cmp(&a.name)));
    out
}

/// At startup: an automatic backup (pruned to the newest few), and a new "largest" copy if
/// the collection has not shrunk below the previous largest.
pub fn on_startup(store: &Store, dir: &Path) -> Result<(), String> {
    let count = store.count().map_err(|e| e.to_string())?;
    if count == 0 {
        return Ok(());
    }
    take(store, dir, "auto")?;
    for old in list(dir).into_iter().filter(|b| b.kind == "Automatic").skip(KEEP_AUTO) {
        let _ = std::fs::remove_file(dir.join(old.name));
    }
    let largest: Vec<(i64, String)> = list(dir)
        .into_iter()
        .filter(|b| b.kind == "Largest collection")
        .filter_map(|b| Some((b.name.split('-').nth(1)?.parse().ok()?, b.name)))
        .collect();
    if largest.iter().all(|(n, _)| count >= *n) {
        take(store, dir, &format!("largest-{count}"))?;
        for (_, old) in largest {
            let _ = std::fs::remove_file(dir.join(old));
        }
    }
    Ok(())
}

/// Resolve a backup name from the UI to a file inside `dir`; rejects anything path-like.
pub fn resolve(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let ok = name.ends_with(".db")
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    let path = dir.join(name);
    if !ok || !path.is_file() {
        return Err(format!("no backup named {name}"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_rotation_largest_and_name_checks() {
        let dir = std::env::temp_dir().join(format!("pm-backups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Store::open(":memory:").unwrap();
        on_startup(&s, &dir).unwrap();
        assert!(list(&dir).is_empty(), "nothing to back up yet");

        let (a, _) = s.add("https://a.example/", "A", "", "").unwrap();
        s.add("https://b.example/", "B", "", "").unwrap();
        on_startup(&s, &dir).unwrap();
        let names: Vec<_> = list(&dir).into_iter().map(|b| b.name).collect();
        assert!(names.iter().any(|n| n.starts_with("largest-2-")), "{names:?}");

        // Shrinking keeps the old largest copy.
        s.delete(&[a]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100)); // distinct timestamps
        on_startup(&s, &dir).unwrap();
        let largest: Vec<_> = list(&dir).into_iter().filter(|b| b.kind == "Largest collection").map(|b| b.name).collect();
        assert_eq!(largest.len(), 1);
        assert!(largest[0].starts_with("largest-2-"));

        // Old automatic backups are pruned.
        for i in 0..12 {
            std::fs::write(dir.join(format!("auto-{i}.db")), b"x").unwrap();
        }
        on_startup(&s, &dir).unwrap();
        assert_eq!(list(&dir).iter().filter(|b| b.kind == "Automatic").count(), KEEP_AUTO);

        // Several backups in the same second get distinct names.
        let (a, b) = (take(&s, &dir, "manual").unwrap(), take(&s, &dir, "manual").unwrap());
        assert_ne!(a, b);

        let any = list(&dir).remove(0).name;
        assert!(resolve(&dir, &any).is_ok());
        for bad in ["..\\bookmarks.db", "../x.db", "a/b.db", ".db", "x.txt", "missing.db"] {
            assert!(resolve(&dir, bad).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
