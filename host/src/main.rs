//! Native messaging host: lets the browser extension add bookmarks to the Keymarks database.
//! Protocol (both directions): 4-byte native-endian length, then UTF-8 JSON.
//!
//! `keymarks-host --install` registers it with Chrome, Edge and Firefox for the current
//! Windows user; `--uninstall` removes that. Any other invocation is the browser starting us.

use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use store::Store;

const NAME: &str = "com.pattern158.keymarks";
const CHROME_ORIGIN: &str = "chrome-extension://bpjpedmfmpepadclpoaogjhmdjcaoild/";
const FIREFOX_ID: &str = "keymarks@pattern158";
const CHROMIUM_KEYS: [&str; 2] = [
    r"HKCU\Software\Google\Chrome\NativeMessagingHosts",
    r"HKCU\Software\Microsoft\Edge\NativeMessagingHosts",
];
const FIREFOX_KEY: &str = r"HKCU\Software\Mozilla\NativeMessagingHosts";
const MAX_FIELD: usize = 8 * 1024;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Request {
    Ping,
    Add {
        url: String,
        title: String,
        #[serde(default)]
        keywords: String,
        #[serde(default)]
        description: String,
    },
}

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("--install") => install(),
        Some("--uninstall") => uninstall(),
        _ => serve().map_err(|e| e.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("keymarks-host: {e}");
            ExitCode::FAILURE
        }
    }
}

fn db_path() -> Result<PathBuf, String> {
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA not set")?;
    Ok(PathBuf::from(appdata).join("com.pattern158.keymarks").join("bookmarks.db"))
}

fn serve() -> io::Result<()> {
    let store = db_path().and_then(|p| {
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        Store::open(p.to_str().ok_or("database path is not UTF-8")?).map_err(|e| e.to_string())
    });
    let (mut stdin, mut stdout) = (io::stdin().lock(), io::stdout().lock());
    while let Some(msg) = read_message(&mut stdin)? {
        let reply = match &store {
            Ok(store) => handle(store, &msg),
            Err(e) => json!({ "ok": false, "error": e }),
        };
        write_message(&mut stdout, &reply)?;
    }
    Ok(())
}

fn handle(store: &Store, msg: &[u8]) -> Value {
    let request = match serde_json::from_slice::<Request>(msg) {
        Ok(r) => r,
        Err(e) => return json!({ "ok": false, "error": format!("bad request: {e}") }),
    };
    match request {
        Request::Ping => json!({ "ok": true, "count": store.count().unwrap_or(-1) }),
        Request::Add { url, title, keywords, description } => {
            let scheme_ok = url.starts_with("http://") || url.starts_with("https://");
            let sizes_ok = [&url, &title, &keywords, &description].iter().all(|f| f.len() <= MAX_FIELD);
            if !scheme_ok || !sizes_ok {
                return json!({ "ok": false, "error": "only http(s) pages up to 8 KB per field" });
            }
            match store.add(&url, title.trim(), keywords.trim(), description.trim()) {
                Ok((id, existed)) => json!({ "ok": true, "id": id, "existed": existed }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            }
        }
    }
}

/// None on clean EOF (the browser closed the port).
fn read_message(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message over 1 MB"));
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn write_message(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(v)?;
    w.write_all(&(body.len() as u32).to_ne_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Write the two host manifests next to this exe and point the browsers' registry keys at them.
fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("exe has no directory")?;
    let base = json!({ "name": NAME, "description": "Keymarks", "path": exe, "type": "stdio" });
    let mut chromium = base.clone();
    chromium["allowed_origins"] = json!([CHROME_ORIGIN]);
    let mut firefox = base;
    firefox["allowed_extensions"] = json!([FIREFOX_ID]);
    let chromium_path = dir.join(format!("{NAME}.chromium.json"));
    let firefox_path = dir.join(format!("{NAME}.firefox.json"));
    for (path, manifest) in [(&chromium_path, &chromium), (&firefox_path, &firefox)] {
        std::fs::write(path, serde_json::to_vec_pretty(manifest).unwrap()).map_err(|e| e.to_string())?;
    }
    for key in CHROMIUM_KEYS {
        reg(&["add", &format!(r"{key}\{NAME}"), "/ve", "/t", "REG_SZ", "/d", path_str(&chromium_path)?, "/f"])?;
    }
    reg(&["add", &format!(r"{FIREFOX_KEY}\{NAME}"), "/ve", "/t", "REG_SZ", "/d", path_str(&firefox_path)?, "/f"])?;
    println!("Registered {NAME} for Chrome, Edge and Firefox -> {}", exe.display());
    Ok(())
}

fn uninstall() -> Result<(), String> {
    for key in CHROMIUM_KEYS.iter().chain([&FIREFOX_KEY]) {
        // A key that was never added is fine; ignore reg's failure for it.
        let _ = reg(&["delete", &format!(r"{key}\{NAME}"), "/f"]);
    }
    println!("Unregistered {NAME}");
    Ok(())
}

fn path_str(p: &std::path::Path) -> Result<&str, String> {
    p.to_str().ok_or_else(|| format!("{} is not UTF-8", p.display()))
}

fn reg(args: &[&str]) -> Result<(), String> {
    let out = Command::new("reg").args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("reg {}: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framed_round_trip_and_requests() {
        let store = Store::open(":memory:").unwrap();
        let mut wire = Vec::new();
        write_message(&mut wire, &json!({ "type": "add", "url": "https://a.example/", "title": " A ", "keywords": "k" })).unwrap();
        let msg = read_message(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(handle(&store, &msg)["existed"], false);
        assert_eq!(handle(&store, &msg)["existed"], true);
        assert_eq!(store.search("k", 5).unwrap()[0].title, "A");
        assert_eq!(handle(&store, br#"{"type":"ping"}"#)["count"], 1);
        assert_eq!(handle(&store, br#"{"type":"add","url":"javascript:alert(1)","title":"x"}"#)["ok"], false);
        assert_eq!(handle(&store, b"nope")["ok"], false);
        assert!(read_message(&mut [].as_slice()).unwrap().is_none());
    }
}
