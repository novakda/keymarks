//! Build the demo collection used for README screenshots (public sites only, no personal data).
//!
//!     cargo run -p store --example demo -- <folder>
//!
//! Then start the app on it: set KEYMARKS_DATA_DIR=<folder> and run keymarks.exe.

use store::{Store, browsers::read_netscape_html};

const RATINGS: &[(&str, i64)] = &[
    ("https://www.rust-lang.org/", 9),
    ("https://doc.rust-lang.org/book/", 10),
    ("https://tauri.app/", 8),
    ("https://www.sqlite.org/fts5.html", 9),
    ("https://developer.mozilla.org/", 10),
    ("https://www.w3.org/WAI/ARIA/apg/", 8),
    ("https://ghidra-sre.org/", 7),
    ("https://playwright.dev/", 8),
    ("https://www.seriouseats.com/", 7),
    ("https://www.openstreetmap.org/", 8),
    ("https://bandcamp.com/", 9),
    ("https://archive.org/", 10),
    ("https://www.gutenberg.org/", 8),
    ("https://excalidraw.com/", 7),
];

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: demo <folder>"));
    std::fs::create_dir_all(&dir).expect("create folder");
    let db = dir.join("bookmarks.db");
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
    let html = include_str!("../../docs/demo/bookmarks.html");
    let mut store = Store::open(db.to_str().expect("UTF-8 path")).expect("open");
    let report = store.import_bookmarks("Demo", &read_netscape_html(html)).expect("import");
    for (url, rating) in RATINGS {
        let mut b = store.find_url(url).expect("query").expect("demo bookmark");
        b.rating = *rating;
        store.update(&b).expect("rate");
    }
    store.set_tagged("rust", true).expect("tag");
    store.set_tagged("docs", true).expect("tag");
    println!("{} demo bookmarks in {}", report.added, db.display());
}
