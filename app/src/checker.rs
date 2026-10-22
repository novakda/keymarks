//! Link checking: is the page still there, has it changed, did it move permanently?
//!
//! Change detection prefers the server's validators (ETag / Last-Modified, sent back as a
//! conditional request so an unchanged page answers 304). Otherwise it hashes the page text
//! with scripts, styles, markup and digits removed, so rotating ads and timestamps don't
//! count as changes (the original's "intelligent status checking").

use crate::Db;
use futures::StreamExt;
use regex::Regex;
use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION};
use reqwest::{Client, Url, redirect::Policy};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use store::{CheckRecord, CheckTarget, PageMeta};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Semaphore;

/// Politeness: at most this many requests at once, and per site.
const PARALLEL: usize = 8;
const PER_HOST: usize = 2;
/// If the first this-many results are all network failures, assume we're offline and stop.
const OFFLINE_SAMPLE: usize = 10;

#[derive(Default)]
pub struct CheckState {
    pub running: AtomicBool,
    pub cancel: AtomicBool,
    /// No new links are started while set; the ones already under way finish.
    pub paused: AtomicBool,
}

/// Sent to the UI as "check-progress" while a batch runs and once when it ends.
#[derive(Clone, Default, serde::Serialize)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub counts: BTreeMap<String, usize>,
    pub finished: bool,
    pub cancelled: bool,
    pub offline: bool,
    pub repaired: usize,
    pub error: Option<String>,
    /// Bookmarks finished since the previous event (the UI clears their "waiting" mark).
    pub recent: Vec<i64>,
}

/// Check `targets` in the background, store each result, and finally repair permanently
/// moved URLs (one undoable step) if `repair` is set.
pub async fn run_batch(app: AppHandle, targets: Vec<CheckTarget>, repair: bool) {
    let state = app.state::<CheckState>();
    let mut progress = Progress { total: targets.len(), ..Default::default() };
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            progress.error = Some(e.to_string());
            return end(&app, &state, progress);
        }
    };
    let hosts: Mutex<HashMap<String, Arc<Semaphore>>> = Mutex::default();
    let (client, hosts, cancel, paused) = (&client, &hosts, &state.cancel, &state.paused);
    let mut results = futures::stream::iter(targets)
        .map(|t| async move {
            let host = Url::parse(&t.url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
            let site = hosts.lock().unwrap().entry(host).or_insert_with(|| Arc::new(Semaphore::new(PER_HOST))).clone();
            let _permit = site.acquire().await.ok()?;
            while paused.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            Some((t.id, check(client, &t).await))
        })
        .buffer_unordered(PARALLEL);
    let mut moved = Vec::new();
    let mut network_failures = 0;
    while let Some(result) = results.next().await {
        let Some((id, out)) = result else { continue }; // cancelled before it started
        if let Err(e) = app.state::<Db>().lock().unwrap().record_check(id, &out.record) {
            progress.error = Some(e.to_string());
        }
        progress.done += 1;
        progress.recent.push(id);
        *progress.counts.entry(out.record.status.clone()).or_default() += 1;
        if let Some(to) = out.moved_to {
            moved.push((id, to));
        }
        if progress.done <= OFFLINE_SAMPLE && NETWORK_FAILURES.contains(&out.record.status.as_str()) {
            network_failures += 1;
            if network_failures == OFFLINE_SAMPLE {
                progress.offline = true;
                cancel.store(true, Ordering::Relaxed);
            }
        }
        let _ = app.emit("check-progress", &progress); // every result: the UI throttles its refreshes
        progress.recent.clear();
    }
    progress.cancelled = cancel.load(Ordering::Relaxed);
    if repair && !moved.is_empty() {
        match app.state::<Db>().lock().unwrap().repair_urls(&moved) {
            Ok(n) => progress.repaired = n,
            Err(e) => progress.error = Some(e.to_string()),
        }
    }
    end(&app, &state, progress);
}

fn end(app: &AppHandle, state: &CheckState, mut progress: Progress) {
    progress.finished = true;
    state.cancel.store(false, Ordering::Relaxed);
    state.paused.store(false, Ordering::Relaxed);
    state.running.store(false, Ordering::Relaxed);
    let _ = app.emit("check-progress", &progress);
}

const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
pub const NETWORK_FAILURES: [&str; 3] = ["server_not_found", "no_response", "network_error"];

pub fn client() -> reqwest::Result<Client> {
    Client::builder()
        .redirect(Policy::none()) // followed by hand, to tell permanent moves from temporary ones
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Keymarks/", env!("CARGO_PKG_VERSION"), " link checker"))
        .build()
}

pub struct Outcome {
    pub record: CheckRecord,
    /// Where the page now lives, if every redirect on the way was permanent (301/308).
    pub moved_to: Option<String>,
}

fn outcome(status: &str, http: Option<u16>, error: Option<String>) -> Outcome {
    Outcome {
        record: CheckRecord { status: status.into(), http_status: http.map(i64::from), error, ..Default::default() },
        moved_to: None,
    }
}

pub async fn check(client: &Client, target: &CheckTarget) -> Outcome {
    let Ok(start) = Url::parse(target.url.trim()) else { return outcome("bad_url", None, None) };
    if !matches!(start.scheme(), "http" | "https") {
        return outcome("skipped", None, None);
    }
    let mut url = start.clone();
    let mut permanent = true;
    for hop in 0..=MAX_REDIRECTS {
        let mut req = client.get(url.clone());
        if hop == 0 {
            if let Some(etag) = &target.etag {
                req = req.header(IF_NONE_MATCH, etag);
            }
            if let Some(t) = target.last_modified.filter(|&t| t > 0) {
                req = req.header(IF_MODIFIED_SINCE, httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(t as u64)));
            }
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return failure(&e),
        };
        let code = resp.status().as_u16();
        if (300..400).contains(&code) && code != 304 {
            let next = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).and_then(|l| url.join(l).ok());
            let Some(next) = next else { return outcome("redirect_error", Some(code), None) };
            permanent &= matches!(code, 301 | 308);
            url = next;
            continue;
        }
        let mut out = finish(resp, target).await;
        if url != start && permanent && out.record.http_status.is_some_and(|c| (200..300).contains(&c) || c == 304) {
            out.moved_to = Some(url.to_string());
        }
        return out;
    }
    outcome("redirect_error", None, Some("too many redirects".into()))
}

async fn finish(mut resp: reqwest::Response, target: &CheckTarget) -> Outcome {
    let code = resp.status().as_u16();
    let status = match code {
        304 => return outcome("ok", Some(304), None),
        200..=299 => "ok",
        401 | 407 => "unauthorized",
        403 => "forbidden",
        404 | 410 => "not_found",
        429 => "rate_limited",
        500..=599 => "server_error",
        _ => "client_error",
    };
    if status != "ok" {
        return outcome(status, Some(code), None);
    }
    let header = |name| resp.headers().get(name).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).map(String::from);
    let etag = header(ETAG);
    let last_modified = header(LAST_MODIFIED)
        .and_then(|d| httpdate::parse_http_date(&d).ok())
        .and_then(|t: SystemTime| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        body.extend_from_slice(&chunk);
        if body.len() >= MAX_BODY {
            break;
        }
    }
    let hash = format!("{:016x}", fnv1a(normalize(&String::from_utf8_lossy(&body)).as_bytes()));
    // Same validator as last time means unchanged even if the server ignored our conditional request.
    let same_validator = (etag.is_some() && etag == target.etag) || (last_modified.is_some() && last_modified == target.last_modified);
    let changed = !same_validator && target.content_hash.as_ref().is_some_and(|old| *old != hash);
    Outcome {
        record: CheckRecord {
            status: if changed { "changed" } else { "ok" }.into(),
            http_status: Some(code.into()),
            etag,
            last_modified,
            content_hash: Some(hash),
            changed,
            error: None,
        },
        moved_to: None,
    }
}

fn failure(e: &reqwest::Error) -> Outcome {
    // Walk the source chain: DNS failures surface as nested "dns error" causes.
    let mut chain = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        chain.push_str(": ");
        chain.push_str(&s.to_string());
        src = s.source();
    }
    let status = if chain.to_lowercase().contains("dns") {
        "server_not_found"
    } else if e.is_timeout() || e.is_connect() {
        "no_response"
    } else if e.is_redirect() {
        "redirect_error"
    } else {
        "network_error"
    };
    outcome(status, None, Some(chain.chars().take(300).collect()))
}

// ---- Fetch: what a page says about itself ---------------------------------------------

static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap());
static META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<meta\s[^>]*>"#).unwrap());
static META_ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)(name|property|content)\s*=\s*("([^"]*)"|'([^']*)')"#).unwrap());
static URLS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bhttps?://[^\s<>"'`]+"#).unwrap());

/// Title, description and keywords from a page's HTML (Powermarks' Fetch).
pub fn page_meta(html: &str) -> PageMeta {
    let clean = |s: &str| html_escape::decode_html_entities(s).split_whitespace().collect::<Vec<_>>().join(" ");
    let mut meta = PageMeta { title: TITLE.captures(html).map(|c| clean(&c[1])).unwrap_or_default(), ..Default::default() };
    for tag in META.find_iter(html) {
        let (mut key, mut content) = (String::new(), None);
        for a in META_ATTR.captures_iter(tag.as_str()) {
            let value = a.get(3).or(a.get(4)).map_or("", |m| m.as_str());
            match a[1].to_ascii_lowercase().as_str() {
                "content" => content = Some(clean(value)),
                _ => key = value.to_ascii_lowercase(),
            }
        }
        let Some(content) = content else { continue };
        match key.as_str() {
            "description" | "og:description" if meta.description.is_empty() => meta.description = content,
            "og:title" if meta.title.is_empty() => meta.title = content,
            "keywords" => meta.keywords = content.split(',').map(str::trim).collect::<Vec<_>>().join(" "),
            _ => {}
        }
    }
    meta
}

/// Client for Fetch: like the checker's, but follows redirects (the checker must see them).
pub fn fetch_client() -> reqwest::Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Keymarks/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Fetch a page and read its metadata. None for non-web URLs, errors and failed replies.
pub async fn fetch_meta(client: &Client, url: &str) -> Option<PageMeta> {
    let parsed = Url::parse(url).ok().filter(|u| matches!(u.scheme(), "http" | "https"))?;
    let mut resp = client.get(parsed).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        body.extend_from_slice(&chunk);
        if body.len() >= 512 * 1024 {
            break;
        }
    }
    Some(page_meta(&String::from_utf8_lossy(&body)))
}

/// Every distinct http(s) URL in a piece of text (e.g. the clipboard), in order. Trailing
/// punctuation from prose ("see https://a.example/.") is not part of the URL.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    URLS.find_iter(text)
        .map(|m| m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']).to_string())
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<script\b.*?</script>|<style\b.*?</style>|<iframe\b.*?</iframe>|<noscript\b.*?</noscript>|<!--.*?-->").unwrap()
});
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());

/// Page text without markup, scripts, ads in iframes, numbers (dates, counters) or spacing.
pub fn normalize(html: &str) -> String {
    let text = NOISE.replace_all(html, " ");
    let text = TAGS.replace_all(&text, " ");
    let text = DIGITS.replace_all(&text, "");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FNV-1a: tiny and stable across Rust versions (stored hashes must stay comparable).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A tiny local HTTP server: no test ever touches the internet.
    fn serve() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut inm = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() <= 2 {
                        break;
                    }
                    if line.to_lowercase().starts_with("if-none-match:") {
                        inm = line[14..].trim().to_string();
                    }
                }
                let (status, headers, body): (&str, String, String) = match path.as_str() {
                    "/page" => ("200 OK", String::new(), "<p>Hello <b>world</b> 1234</p><script>ads()</script>".into()),
                    "/page-ad" => ("200 OK", String::new(), "<p>Hello world 5678</p><script>other()</script><iframe>x</iframe>".into()),
                    "/page-new" => ("200 OK", String::new(), "<p>Hello brave new world</p>".into()),
                    "/etag" if inm == "\"v1\"" => ("304 Not Modified", "ETag: \"v1\"\r\n".into(), String::new()),
                    "/etag" => ("200 OK", "ETag: \"v1\"\r\n".into(), "versioned".into()),
                    "/gone" => ("404 Not Found", String::new(), "nope".into()),
                    "/old" => ("301 Moved Permanently", "Location: /page\r\n".into(), String::new()),
                    "/temp" => ("302 Found", "Location: /page\r\n".into(), String::new()),
                    "/loop" => ("301 Moved Permanently", "Location: /loop\r\n".into(), String::new()),
                    "/boom" => ("500 Internal Server Error", String::new(), String::new()),
                    _ => ("403 Forbidden", String::new(), String::new()),
                };
                let mut s = stream;
                let _ = write!(s, "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        base
    }

    fn target(url: String) -> CheckTarget {
        CheckTarget { id: 1, url, etag: None, last_modified: None, content_hash: None }
    }

    #[test]
    fn statuses_changes_and_redirects() {
        let base = serve();
        let client = client().unwrap();
        let run = |t: CheckTarget| tauri::async_runtime::block_on(check(&client, &t));

        let first = run(target(format!("{base}/page")));
        assert_eq!((first.record.status.as_str(), first.record.http_status), ("ok", Some(200)));
        let hash = first.record.content_hash.clone().unwrap();

        // Ads/scripts/iframes/numbers differ: not a change. Real text differs: changed.
        let mut t = target(format!("{base}/page-ad"));
        t.content_hash = Some(hash.clone());
        assert!(!run(t).record.changed);
        let mut t = target(format!("{base}/page-new"));
        t.content_hash = Some(hash);
        let changed = run(t);
        assert!(changed.record.changed && changed.record.status == "changed");

        // ETag round trip: the server answers 304 to our conditional request.
        let e = run(target(format!("{base}/etag")));
        assert_eq!(e.record.etag.as_deref(), Some("\"v1\""));
        let mut t = target(format!("{base}/etag"));
        t.etag = e.record.etag;
        t.content_hash = Some("anything".into());
        let again = run(t);
        assert_eq!((again.record.status.as_str(), again.record.http_status, again.record.changed), ("ok", Some(304), false));

        assert_eq!(run(target(format!("{base}/gone"))).record.status, "not_found");
        assert_eq!(run(target(format!("{base}/boom"))).record.status, "server_error");
        assert_eq!(run(target(format!("{base}/x"))).record.status, "forbidden");

        let moved = run(target(format!("{base}/old")));
        assert_eq!(moved.moved_to.as_deref(), Some(format!("{base}/page").as_str()), "301 is repaired");
        assert_eq!(run(target(format!("{base}/temp"))).moved_to, None, "302 is not");
        assert_eq!(run(target(format!("{base}/loop"))).record.status, "redirect_error");

        assert_eq!(run(target("ftp://example.com/".into())).record.status, "skipped");
        assert_eq!(run(target("not a url".into())).record.status, "bad_url");
        let dead = run(target("http://127.0.0.1:9/".into())); // discard port: nothing listens
        assert!(NETWORK_FAILURES.contains(&dead.record.status.as_str()), "{}", dead.record.status);
    }

    #[test]
    fn page_meta_and_clipboard_urls() {
        let m = page_meta(r#"<html><head><title> Caf&eacute;
             Guide </title><meta name="Description" content="All about &quot;coffee&quot;">
             <meta property='og:title' content='ignored'><meta name="keywords" content="coffee, espresso ,brew"></head>"#);
        assert_eq!(m, PageMeta { title: "Café Guide".into(), description: "All about \"coffee\"".into(), keywords: "coffee espresso brew".into() });
        assert_eq!(page_meta("<meta property=\"og:title\" content=\"OG only\">").title, "OG only");

        let text = "See https://a.example/x. Also (http://b.example/?q=1) and https://a.example/x again, <https://c.example/p>";
        assert_eq!(extract_urls(text), ["https://a.example/x", "http://b.example/?q=1", "https://c.example/p"]);
        assert!(extract_urls("no links, ftp://x.example/ isn't one").is_empty());
    }

    #[test]
    fn fetch_meta_from_local_server() {
        let base = serve();
        let client = fetch_client().unwrap();
        // /old redirects to /page; fetch follows it.
        let meta = tauri::async_runtime::block_on(fetch_meta(&client, &format!("{base}/old"))).unwrap();
        assert_eq!(meta.title, "", "the test page has no <title>");
        assert!(tauri::async_runtime::block_on(fetch_meta(&client, &format!("{base}/gone"))).is_none());
        assert!(tauri::async_runtime::block_on(fetch_meta(&client, "ftp://x.example/")).is_none());
    }

    #[test]
    fn normalization_ignores_noise() {
        assert_eq!(normalize("<p>Hi <!-- c --> <b>there</b> 12:30</p><style>p{}</style>"), "Hi there :");
    }
}
