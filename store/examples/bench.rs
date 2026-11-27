//! SYS-22 timings on generated collections (13k and 42k bookmarks, ~500 folders up to 6
//! deep, keywords, notes, ~2% duplicate links). Prints a markdown table of medians.
//!
//!     cargo run --release -p store --example bench [-- <runs>]

use std::time::Instant;
use store::{Entry, FoldersAt, Mode, NewBookmark, ROOT, SearchOptions, SortBy, SortKey, Store};

const WORDS: &[&str] = &[
    "rust", "python", "java", "recipe", "travel", "music", "history", "linux", "windows", "design", "garden", "finance",
    "health", "science", "photo", "video", "game", "news", "review", "tutorial", "project", "programming", "database",
    "network", "security", "kitchen", "bread", "coffee", "camera", "bike", "hiking", "portland", "finland", "career",
    "resume", "sqlite", "tauri", "browser", "keyboard", "archive", "library", "podcast", "poetry", "novel", "map",
];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len())]
    }
    fn words(&mut self, n: usize) -> String {
        (0..n).map(|_| self.word()).collect::<Vec<_>>().join(" ")
    }
}

/// Folder tree: `parent[i]` for folder i (folder 0 is the root, depth 0); folder 1 is "Big"
/// (2,000 items) at the top level.
fn build(n_items: usize, n_folders: usize, rng: &mut Rng) -> Vec<Entry> {
    let mut parent = vec![usize::MAX, 0];
    let mut depth = vec![0, 1];
    while parent.len() < n_folders + 1 {
        let p = rng.below(parent.len());
        if depth[p] < 6 && p != 1 {
            parent.push(p);
            depth.push(depth[p] + 1);
        }
    }
    let mut items: Vec<Vec<NewBookmark>> = (0..parent.len()).map(|_| Vec::new()).collect();
    for i in 0..n_items {
        let f = if i < 2000 { 1 } else { 2 + rng.below(parent.len() - 2) };
        let host = format!("{}{}.example", rng.word(), i % 3000);
        items[f].push(NewBookmark {
            url: format!("https://{host}/{}/{i}", rng.word()),
            title: format!("{} {} {}", rng.words(3), i, rng.word()),
            keywords: { let k = 1 + rng.below(4); rng.words(k) },
            description: rng.words(8),
            notes: if i % 5 == 0 { rng.words(15) } else { String::new() },
            added: Some(1_500_000_000 + i as i64 * 3600),
        });
    }
    let mut kids: Vec<Vec<usize>> = (0..parent.len()).map(|_| Vec::new()).collect();
    for f in 1..parent.len() {
        kids[parent[f]].push(f);
    }
    let mut out = Vec::new();
    fn emit(f: usize, kids: &[Vec<usize>], items: &mut [Vec<NewBookmark>], out: &mut Vec<Entry>) {
        for &k in &kids[f] {
            out.push(Entry::Folder { name: if k == 1 { "Big".into() } else { format!("Folder {k}") }, note: String::new(), added: None });
            emit(k, kids, items, out);
            out.push(Entry::Close);
        }
        out.extend(std::mem::take(&mut items[f]).into_iter().map(Entry::Bookmark));
    }
    emit(0, &kids, &mut items, &mut out);
    out
}

fn median(runs: usize, mut f: impl FnMut(usize)) -> f64 {
    let mut t: Vec<f64> = (0..runs)
        .map(|i| {
            let s = Instant::now();
            f(i);
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(|a, b| a.total_cmp(b));
    t[t.len() / 2]
}

fn bench(n: usize, runs: usize) -> Vec<(String, f64)> {
    let dir = std::env::temp_dir().join(format!("keymarks-bench-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Store::open(dir.join("b.db").to_str().unwrap()).unwrap();
    let mut rng = Rng(0x9e3779b97f4a7c15 ^ n as u64);
    s.import_tree("Bench", &build(n, 500, &mut rng), ROOT, None, false).unwrap();
    // ~2% duplicate links (an import skips them, so make them after).
    let all = s.search_ids("", &SearchOptions::default()).unwrap();
    for k in 0..n / 50 {
        let mut b = s.get(all[k * 37 % all.len()]).unwrap().unwrap();
        b.url = s.get(all[(k * 37 + 11) % all.len()]).unwrap().unwrap().url.replace("https://", "http://www.");
        s.update(&b).unwrap();
    }
    let folders = s.folders().unwrap();
    let big = folders.iter().find(|f| f.name == "Big").unwrap().id;
    let deep: Vec<i64> = folders.iter().filter(|f| f.id != big && f.id != ROOT).map(|f| f.id).collect();
    let in_big = s.search_ids("", &SearchOptions { folder: Some(big), ..Default::default() }).unwrap();
    let top = folders.iter().filter(|f| f.parent_id == Some(ROOT) && f.id != big).max_by_key(|f| f.id).unwrap().id;
    let mut r: Vec<(String, f64)> = Vec::new();
    let big_left = in_big.len();
    let t = median(runs, |i| {
        s.move_items(&[big], &[], if i % 2 == 0 { top } else { ROOT }, None).unwrap();
    });
    r.push((format!("move folder ({big_left} items)"), t));
    let t = median(runs, |i| {
        s.sort_folder(big, if i % 2 == 0 { SortBy::Title } else { SortBy::Added }, false, FoldersAt::First, false).unwrap();
    });
    r.push(("sort folder (Big) by title/added".into(), t));
    let list = SearchOptions { folder: Some(big), ..Default::default() };
    r.push((format!("list folder (Big, {} items): ids", s.search_ids("", &list).unwrap().len()), median(runs, |_| drop(s.search_ids("", &list).unwrap()))));
    r.push(("list folder (Big): children".into(), median(runs, |_| drop(s.children(big).unwrap()))));
    let t = median(runs, |i| {
        s.move_items(&[], &[in_big[i]], deep[i % deep.len()], None).unwrap();
    });
    r.push(("move 1 bookmark".into(), t));
    let t = median(runs, |i| {
        s.move_items(&[], &in_big[200 + i * 100..300 + i * 100], deep[(i * 7) % deep.len()], None).unwrap();
    });
    r.push(("move 100 bookmarks".into(), t));
    let all_opts = SearchOptions::default();
    let scoped = SearchOptions { folder: Some(top), in_folder: true, ..Default::default() };
    for (q, label) in [("python", "word"), ("pro", "prefix"), ("rust | python !java", "operators"), ("garden coffee", "two words")] {
        let hits = s.search_ids(q, &all_opts).unwrap().len();
        r.push((format!("search all: {label} `{q}` ({hits} hits)"), median(runs, |_| drop(s.search_ids(q, &all_opts).unwrap()))));
        let hits = s.search_ids(q, &scoped).unwrap().len();
        r.push((format!("search folder: {label} ({hits} hits)"), median(runs, |_| drop(s.search_ids(q, &scoped).unwrap()))));
    }
    let exact = SearchOptions { mode: Mode::Exact, fields: vec!["title".into(), "url".into()], ..Default::default() };
    r.push(("search all: exact `coffee` (title, url)".into(), median(runs, |_| drop(s.search_ids("coffee", &exact).unwrap()))));
    let by_folder = SearchOptions { sort: vec![SortKey { column: "folder".into(), desc: false }], ..Default::default() };
    r.push(("search all `python`, sorted by folder".into(), median(runs, |_| drop(s.search_ids("python", &by_folder).unwrap()))));
    let page: Vec<i64> = all.iter().take(200).copied().collect();
    r.push(("get 200 rows (list page)".into(), median(runs, |_| drop(s.get_many(&page).unwrap()))));
    let groups = s.duplicate_groups().unwrap().len();
    r.push((format!("duplicate groups ({groups})"), median(runs, |_| drop(s.duplicate_groups().unwrap()))));
    r.push(("folders (tree load)".into(), median(runs, |_| drop(s.folders().unwrap()))));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn main() {
    let runs: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(9);
    let a = bench(13_000, runs);
    let b = bench(42_000, runs);
    println!("| operation (median of {runs}, ms) | 13k | 42k |\n|---|---:|---:|");
    for ((name, x), (_, y)) in a.iter().zip(&b) {
        println!("| {name} | {x:.1} | {y:.1} |");
    }
}
