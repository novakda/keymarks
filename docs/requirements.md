# Powermarks 2026: feature-parity requirements

Target: rebuild Powermarks 3.5 (build 344, Kaylon Technologies, 2002) as a Windows desktop app
(Tauri 2 + Rust + SQLite FTS5, plain HTML/JS UI) plus an MV3 extension (Chrome/Edge/Firefox)
that talks to it over native messaging.

Sources: `help/chapter_N.htm` (cited as **§x.y**), `changes.txt` (**b344** etc.), `readme.txt`,
`pm35.ini` key names (**ini `section.key`**), and strings in `pm.exe` (**exe "…"**).
The database format is in [pm35-format.md](pm35-format.md) and is not repeated here.

Priority: **P1** = core parity, **P2** = nice to have, **P3** = skip or later.
Status: **done**, **partial**, **todo**, **drop** (deliberately not rebuilt).

---

## 1. What Powermarks was

- A "personal Internet search engine" (exe version info: "Powermarks(tm) Personal Search Engine"). It
  ran as a small, always-on bookmark manager that worked independently of any browser.
- **No folders.** Bookmarks are one flat list (§1.3). Grouping comes from **keywords**: give
  every search engine the keyword `search`, then type `search` to see them all. Kaylon left out
  folders on purpose: folders are slow to browse, need upkeep, force one category per item, and
  can't show arbitrary groups.
- **Instant, incremental search.** The search field is always active and results update with each
  keystroke. Two modes: keyword search (keywords field only, prefix matching, Boolean `& | !`,
  parentheses) and exact string search (substring in chosen fields).
- **Keyword dictionary.** Every keyword in use is listed in a dictionary pane. Users can rename,
  delete or "tag" keywords. Tagged keywords act as a controlled vocabulary shown in a picker.
- **Upkeep.** Status checking finds dead links and changed pages, on schedules if wanted. Fetch
  fills in the title, description and keywords from the page itself. Ratings run 1–10, visits are
  counted, and macros give one-click searches or URLs.
- **Browser glue.** Add/Fast Add from the frontmost browser over DDE/COM/SendKeys, an IE toolband,
  tray icons and global hotkeys, sync from browser bookmarks, and NetSync (a Kaylon server, now
  gone) for backup and multi-PC sync.
- **Privacy.** Network traffic only when checking or fetching, detecting a connection, checking
  versions, or running NetSync (§1.1). No telemetry. The rebuild keeps this principle.
- Storage is one Netscape-compatible HTML file (`pm35.htm`) with Powermarks data in comments.
  It is fast with large collections (30–60k bookmarks claimed, §1.2 and b278).

---

## 2. Feature inventory

### 2.1 Search and display

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| SRCH-01 | Always-active incremental search | Results update per keystroke; search field in toolbar (§5.1, §1.3) | Search box focused on launch/activation; query per `input` event; stale responses discarded | P1 | done |
| SRCH-02 | Keyword search mode | Matches only the **keywords** field; internal keyword index (§10.5.1) | FTS5 column filter `keywords:` or a separate `keyword` table. Currently every field is searched; see Q1 | P1 | partial |
| SRCH-03 | Partial (prefix) word matching option | `comp` matches `computer` when "Partial Matches" is on; otherwise whole word only (§10.5.1, ini `search.partial_match`) | FTS5 `"term"*` vs `"term"`; toggle in search options | P1 | partial (prefix always on) |
| SRCH-04 | Trailing period = whole word | `win.` matches `win` but not `windows` (§10.5.1) | Parser: a term ending in `.` drops the `*` | P1 | todo |
| SRCH-05 | Implied AND | Space between words = AND (§10.5.1) | FTS5 implicit AND | P1 | done |
| SRCH-06 | Implied OR option | Hidden option, ini `search.implied_or` (no UI found in manual or exe dialogs) | Setting "space means OR" | P3 | todo |
| SRCH-07 | Boolean operators | `&` AND, `\|` OR, `!` NOT, also `.and .or .not` (§10.5.1). Example `stock ! free` | Small recursive-descent parser → FTS5 `AND/OR/NOT` (FTS5 has no unary NOT: rewrite as `x NOT y`, and use `* NOT` via a subquery when NOT comes first) | P1 | todo |
| SRCH-08 | Parentheses grouping | `(comp \| sci) & mag` (§10.5.1; parenthesis fix b297) | Same parser; FTS5 supports grouping | P1 | todo |
| SRCH-09 | Search expression errors | exe "Search expression error", "No matching keywords" in status bar | Show inline hint under the box and keep the last good result | P1 | todo |
| SRCH-10 | Exact string search mode | Substring, character by character, in chosen fields; no word concept (§10.5.2) | `instr()`/`LIKE` scan over selected columns (fine at 60k rows), or FTS5 trigram table | P1 | todo |
| SRCH-11 | Field selection for exact search | Fields dropdown and Select All/None (§10.5.2, exe "Fields to Search:") | Checkbox menu next to the search box | P1 | todo |
| SRCH-12 | Match case option | Exact search only (§10.5.2; ini `search.case_ins`) | Toggle; use `instr` (case-sensitive) vs `lower()` | P2 | todo |
| SRCH-13 | Search mode toggle UI | Two toolbar buttons plus options dropdown and Search Options dialog (§10.5, Tools menu) | Mode toggle (e.g. Ctrl+K / Ctrl+S, or a prefix such as `"…"` for exact). No separate dialog | P1 | todo |
| SRCH-14 | Esc clears search | exe "Keyword search, press ESC to clear search field"; toolbar Clear button (§10.2) | Esc clears and shows all | P1 | todo |
| SRCH-15 | Live matching-word feedback | While typing, matching dictionary words show in the status bar and dictionary pane; after a space, the pane shows all keywords used by the displayed bookmarks (§10.5.1) | Dictionary pane follows the query (see KW-05) | P2 | todo |
| SRCH-16 | Search for URL | FAQ: exact search with only the URL field (§9) | Covered by SRCH-10/11; FTS already indexes URL | P1 | partial |
| SRCH-17 | Stop list | ini `gui.use_stop_list`. Word list not found in exe strings; probably filters common words when deriving keywords from titles (unconfirmed) | Built-in English stop list applied when keywords are auto-generated (import/fetch), never to user-typed keywords | P2 | todo |
| SRCH-18 | Search settings persisted | ini `search.type`, `partial_match`, `case_ins`, `implied_or`, `mpset` (meaning of `mpset` unknown) | Settings table | P1 | todo |
| DISP-01 | Bookmark list (virtualised) | List view, fast with 60k entries (b278) | Virtual scrolling over the full result set (the 500-row cap is a stopgap) | P1 | partial |
| DISP-02 | Configurable columns | Right-click header or View > Columns; exe column names: Name, URL, Keywords, Description, Notes, Add Date, Last Visited, Last Modified, Last Checked, Visited, Rating, Status, Schedule, Status Checking, user field (§5.3, exe "Customize Columns") | Table mode with column chooser. The current two-line card view stays as a "compact" view | P1 | todo |
| DISP-03 | Reorder columns | Ctrl+drag headers (§5.3) | Drag headers | P2 | todo |
| DISP-04 | Click-to-sort columns | §5.3; ini `display.sort_prop`, `sort_order` | Click header toggles asc/desc; persist | P1 | todo |
| DISP-05 | Two-level stable sort | Ties broken by the previous sort key (b326) | `ORDER BY new, previous` | P2 | todo |
| DISP-06 | Resort on demand | F5 Refresh (b282) | Not needed if the list re-queries live; keep F5 as refresh | P3 | todo |
| DISP-07 | Status icon per row | Waiting/Checking/Error/Changed icons (§5.4.4) | Small icon or emoji column | P1 | todo |
| DISP-08 | Customisable status icons | Options > Icons; ini `gui.status_ind_map` (§8.1.9) | Drop; fixed accessible icons plus tooltip text | P3 | drop |
| DISP-09 | Rating colour coding | readme "Color code bookmarks based on their rating"; ini `colors.ratings` | Row tint or badge colour scale by rating; configurable | P2 | todo |
| DISP-10 | Hide http:// and ftp:// prefixes | §8.1.2; ini `gui.hide_http_prefix` | Display option (strip `https://`, `www.` too) | P2 | todo |
| DISP-11 | Date/time format | Custom format builder (§8.1.6; ini `gui.time_format`, default `%m-%d-%y %h:%M:%s %p`) | Use OS locale via `Intl.DateTimeFormat`; optional relative dates | P2 | todo |
| DISP-12 | Display font | §8.1.3; ini `gui.font_*` | Font size setting plus system UI font; follow OS text scaling | P2 | todo |
| DISP-13 | Header colours | §8.1.4; ini `colors.header_*` | Drop; light/dark theme follows OS | P3 | drop |
| DISP-14 | Three-pane layout | Main list plus Macro pane plus Dictionary pane with splitters (§5.5.2, §5.8; ini `gui.splitter_0/1`, `macro_wnd_visible`, `dict_wnd_visible`) | Collapsible side panels (Saved searches, Keywords); persist sizes | P1 | todo |
| DISP-15 | Status bar | Counts and messages ("%u Powermarks loaded", "SC: %u", "SC: paused", "%u of %u") (exe) | Footer: shown/total, check queue, last action | P1 | partial |
| DISP-16 | Tooltips, flat toolbar | §8.1.2; ini `gui.tooltips`, `flat_toolbar` | Drop as options; always have tooltips (title attr) | P3 | drop |
| DISP-17 | Selected bookmark keyboard nav | Up/Down/Enter, Shift+Enter new window (§10.3) | Up/Down/Enter done; add Shift+Enter, PgUp/PgDn, Home/End, multi-select | P1 | partial |
| DISP-18 | Multi-select | Shift/Ctrl click; Ctrl+A select all (§4.7, §10.3) | Standard multi-select in the list | P1 | todo |
| DISP-19 | Context menus | Bookmark(s), tray icon, search field, macros, dictionary words (§10.4). Bookmark menu: Open in <browser>, Open in New Window, Properties, Make Shortcut Macro, Copy URL, Set Status Checking, Add Common Keywords, Status Check (Ctrl+T), Mark Visited, Export (exe) | Custom HTML context menu (or Tauri native menu) | P1 | todo |
| DISP-20 | Main window state restore | Size, position, maximised (b326; ini `window.*`) | `tauri-plugin-window-state` | P1 | todo |

### 2.2 Adding, editing and fetching bookmarks

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| ADD-01 | Add from browser (edit first) | Ins / toolbar / IE toolband; fills Properties from the frontmost browser page (§4.4) | Extension toolbar popup with title, keywords, description prefilled | P1 | done |
| ADD-02 | Prefill from page meta | Fetch-style extraction of name, description, keywords (§5.2.2) | Popup reads `meta description/og:description/keywords` from the live DOM | P1 | done |
| ADD-03 | Duplicate on Add | Add shows the **existing** bookmark instead of a duplicate; Properties OK asks "A bookmark with the same URL already exists… replace it?" (§4.4, exe) | Host dedupes by URL; the popup says "Already in Powermarks". Todo: load the existing record into the popup for editing | P1 | partial |
| ADD-04 | Fast Add (no dialog) | Ctrl+Ins / toolbar / tray icon; adds silently, does nothing if it exists (§4.4) | Extension command (keyboard shortcut) and context-menu item "Fast add to Powermarks"; badge ✓ as confirmation | P1 | todo |
| ADD-05 | Fast Add default keywords | ini `bookmarks.fast_add_keywords` (§8.1.12), e.g. `new` so they can be reviewed later | Setting applied by host on fast add | P1 | todo |
| ADD-06 | Fast Add confirmation sound | Bell, can be turned off (b344; ini `bookmarks.fast_add_sound`) | Badge/notification instead; optional sound | P3 | todo |
| ADD-07 | Add from clipboard | Alt+Ins and global hotkey: extracts **all** URLs from the clipboard (b290), adds them, then fetches title, description and keywords in the background (§8.1.7, §10.1.4) | App command and global hotkey: read clipboard text/HTML, regex URLs, insert, queue Fetch (FETCH-01) | P1 | todo |
| ADD-08 | Blank manual add | Shift+Add opens an empty Properties dialog (§4.4) | "New bookmark" button/shortcut in app | P1 | todo |
| ADD-09 | Add from a specific browser | Bookmarks > Add from IE/Netscape/Opera/NetCaptor/Mozilla (§10.1.4) | Drop; each browser has its own extension instance | P3 | drop |
| ADD-10 | Bookmark local files | Drag files from Explorer → file-path bookmark; opens with the associated app (§4.5) | Tauri drag-drop event → `file:///` URL; open via shell. `open_url` must allow `file:` for these (it currently rejects non-http) | P2 | todo |
| ADD-11 | Add bookmark validation | Name and URL required (exe "Please enter a URL/name for this bookmark") | Same; also normalise URL (trim, add scheme) | P1 | todo |
| EDIT-01 | Properties dialog | Tabs: General (Name, URL, Keywords, visit count, Tagged Keywords picker, sort-keywords A→Z button), Desc & Notes, User (user field, Rating slider), Status (enable checking and fetching, schedule, Added/Modified/Visited/Checked dates), Auth (username, password) (§5.2, screenshot, exe) | One edit panel or modal with sections; all fields above | P1 | todo |
| EDIT-02 | Prev/next in Properties | Alt+Left/Right (b326); Ctrl+PgUp/PgDn switch tabs (b286) | Same shortcuts in the edit panel | P2 | todo |
| EDIT-03 | Notes field | Free text (§5.2) | Textarea; indexed by FTS (already in schema) | P1 | partial (stored/imported, not editable) |
| EDIT-04 | User-defined field | Per-file label set in File > Information (b278, §10.1.1) | `user_field` column plus a label in settings | P2 | partial (stored) |
| EDIT-05 | Delete bookmark(s) | Del key, toolbar; confirmations for single and multiple deletes are separate options (§4.7, §8.1.2; ini `gui.confirm_*_deletes`) | Del with undo toast rather than a confirm by default; options kept | P1 | todo |
| EDIT-06 | Cut/Copy/Paste bookmarks | Copy puts URLs on the clipboard; Cut deletes; Paste into the list adds bookmarks (§10.1.3; exe `k_clipboard_bookmark_format`, HTML format) | Copy = URLs as text plus HTML links; Paste = ADD-07 path | P2 | todo |
| EDIT-07 | Copy URL to clipboard | Toolbar, context menu (§10.2) | Ctrl+C on selection | P1 | todo |
| FETCH-01 | Fetch page metadata | F7 / Properties Fetch: GET page, replace name and description, **merge** new keywords; reports e.g. "Name and description updated and %d keyword(s) added" (§5.2.2, exe) | Rust `reqwest` GET, parse `<title>`, meta description/keywords, `og:*`; merge, don't overwrite keywords | P1 | todo |
| FETCH-02 | Fetch with authentication | Per-bookmark username/password, Basic auth (b278, Auth tab) | Basic auth from the credential store (CHK-11) | P3 | todo |
| FETCH-03 | Background fetch queue | Clipboard adds fetch in the background; "Fetching" status (exe) | Same worker pool as status checking | P1 | todo |

### 2.3 Keywords and the tagged-keyword dictionary

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| KW-01 | Space-separated keywords per bookmark | §5.2; format spec | Stored as text; also normalise into a `keyword(bookmark_id, word)` table for the dictionary | P1 | partial |
| KW-02 | Allowed-character rule | Forbidden: `< > / \ ! & * % + = ? . , \| ^ ( ) { } [ ] : ; ~`; `.` allowed only inside (`v1.0`) (§5.2.1) | Validate/strip on save; matters because `& \| ! ( ) .` are search operators | P1 | todo |
| KW-03 | Dictionary pane | List of all keywords; exe "Dictionary [%d]" count; type-ahead find; arrow/Esc resets (§5.5.2) | Side panel, keyword list with usage counts, filter box | P1 | todo |
| KW-04 | Click/Enter keyword → search; Ctrl+Enter add to search | §5.5.2, §10.3 | Same | P1 | todo |
| KW-05 | Dictionary follows the result set | After a space, shows only keywords of the displayed bookmarks (§10.5.1); exe "The Dictionary Window is only displayed when the search type is set to Keyword" | Facet list: keywords in the current result set with counts (drill-down) | P1 | todo |
| KW-06 | Tag/untag keywords | Yellow tag marks "important" keywords; multi-select tag/untag (§5.5.2). Stored in `<!--kword=…-->` | `keyword_meta(word, tagged)`; import from pm35 `kword` block (parser keeps it, store doesn't yet) | P1 | todo |
| KW-07 | Show only tagged | ini `gui.show_tagged_kwords_only` | Filter toggle | P2 | todo |
| KW-08 | Tagged-keyword picker in editor | "Tagged Keywords ▾" button adds a word to the bookmark (§5.5.2) | Autocomplete chips in the keyword field: tagged first, then all keywords | P1 | todo |
| KW-09 | Rename keyword globally | §5.5.2 Rename | One UPDATE across bookmarks; undoable | P1 | todo |
| KW-10 | Delete keyword globally | §5.5.2 Delete; Del key | Same; undoable | P1 | todo |
| KW-11 | Copy keyword(s) | Copy selected words (§5.5.2) | Same | P3 | todo |
| KW-12 | Copy whole dictionary to clipboard | Tools > Copy Keywords to Clipboard (§10.1.6) | Menu command (or via export) | P3 | todo |
| KW-13 | Add Common Keywords | Add one or more keywords to all selected bookmarks; dialog shows the dictionary (§5.5.1) | Bulk "Add keywords…" on selection, with autocomplete | P1 | todo |
| KW-14 | Remove keyword from selection | Not in original (only global delete) | Bulk "Remove keywords…" (cheap addition) | P2 | todo |
| KW-15 | Dictionary Cleanup | Merge keywords with common roots; remove keywords with non-printable characters; remove numeric-only keywords (exe dialog "Dictionary Cleanup"; not in manual) | Cleanup wizard with a preview before applying | P2 | todo |
| KW-16 | Sort keywords A→Z | Button in Properties (screenshot) | Normalise order on save, or a button | P3 | todo |
| KW-17 | Auto keywords on import | Folder names become keywords (b297, ini `import.tag_folders`); keywords also from names (§7.1) | Import option: folder path → keywords (done in IMP-xx); title words optional, with the stop list | P1 | todo |

### 2.4 Macros (saved searches and shortcuts)

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| MAC-01 | URL macro | Name plus URL, jump to site (§5.8, §10.1.5) | "Pinned links" in a Saved panel | P1 | todo |
| MAC-02 | Keyword-search macro | Stored keyword expression (§5.8) | Saved search (mode=keyword) | P1 | todo |
| MAC-03 | String-search macro | Stored string plus fields plus match case (§5.8; b294 fix) | Saved search (mode=exact, fields, case) | P1 | todo |
| MAC-04 | Create from current search / selected bookmark | Macros > Current Search, Selected URL; context "Make Shortcut Macro" (§10.1.5) | Star button on the search box; context action on bookmark | P1 | todo |
| MAC-05 | Per-macro hotkey | Hotkey field in all macro dialogs (exe `msctls_hotkey32`). Global or in-app is unknown | In-app shortcuts (Alt+1…9 or user-defined); global optional | P2 | todo |
| MAC-06 | Macro pane ops | Cut/Copy/Paste/Delete/Move Up/Down (Alt+Up/Down), Properties, Sort by Type/Name (§5.8, exe) | Drag-reorder list; edit/delete; sort | P2 | todo |
| MAC-07 | Run macro | Enter / double-click; Shift+Enter or Alt+double-click = new window (§10.3) | Same | P1 | todo |
| MAC-08 | Import/export macros from pm35 | `<!--macro=[...]-->` in head (format spec) | Map into saved-searches table; write back on pm35 export | P1 | todo (parser keeps raw) |

### 2.5 Status checking, schedules and change detection

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| CHK-01 | Check selected bookmarks | Context "Status Check" Ctrl+T (§5.4.1) | Command on selection; async worker pool in Rust | P1 | todo |
| CHK-02 | Force check all | F8 Bookmarks > Force Status Check: all with checking enabled (§5.4.1) | Command; progress in footer | P1 | todo |
| CHK-03 | Per-bookmark enable/disable | Status tab "Enable Status Checking and Fetching" (§5.4.2; mdata f8) | Boolean column; also bulk "Set Status Checking for Group" (exe) | P1 | todo |
| CHK-04 | Dead-link detection | Status texts: Never checked, Waiting, Checking, Fetching, No change, Changed, Page not found, Server not found, Server address unknown, Unauthorized, Proxy Unauthorized, Redirection error, Server error, Timed Out, No Response, Network Failure, Unchecked scheme, Checking Off, Bad URL (exe) | Map to a modern enum (HTTP status, DNS failure, TLS error, timeout); keep text for the Status column | P1 | todo |
| CHK-05 | Change detection | Page "changed since last visit"; Last-Modified plus content hash ("intelligent: ignores advertising banners and other extraneous tags", readme); mdata f7/f9 hash mode | Prefer ETag/Last-Modified; else hash of normalised text (strip scripts, styles, iframes, ad/tracker elements, timestamps). Store the hash | P1 | todo |
| CHK-06 | Changed state cleared on visit | Mark Visited / opening the page resets "changed" (§10.1.4, exe "Mark the bookmark as being visited since the last modification") | Same | P1 | todo |
| CHK-07 | Recheck pages already known changed | Option, off by default (§8.1.8; ini `statchk.recheck_new_pages`) | Setting | P3 | todo |
| CHK-08 | Repair permanent redirects | Update URL on 301 (§8.1.8; ini `statchk.repair_url`) | Update on 301/308 (not 302/307); log old URL in history so it can be undone | P1 | todo |
| CHK-09 | Max concurrent connections | Slider, default about 8 (§8.1.8; ini `statchk.max_conn`, `high_conn_limit`) | Concurrency setting (default 8, max 32); per-host limit 1–2; polite User-Agent | P1 | todo |
| CHK-10 | Pause/resume checking | Bookmarks > Pause Checking (§10.1.4; ini `statchk.pause`) | Toggle in footer/tray | P1 | todo |
| CHK-11 | Authenticated pages | Per-bookmark user/password, Basic auth (b278; Auth tab; mdata f12/f13) | Windows Credential Manager (`keyring` crate); never in the DB in plain text. Most sites now use cookies/SSO, so this has limited use | P3 | todo |
| CHK-12 | Offline detection | Suspends checking when offline; ICMP TTL=3 probe to a Microsoft IP; Dial-up/Direct setting (§8.1.8, §9; ini `internet.use_icmp`, `ttl`, `host`, `direct_conn`) | Windows Network List Manager connectivity (or a failed-DNS heuristic); drop ICMP and dial-up options | P1 | todo |
| CHK-13 | Only http checked | ftp/gopher skipped (§5.4) | http/https only; `file:` bookmarks could check existence (P3) | P1 | todo |
| CHK-14 | Schedules (named intervals) | Tools > Status Checking Schedules: New/Edit/Delete; name plus interval (exe regex `([0-9]*)?([.][0-9]*)? *([mhdwy])([io]?)`, so units are minutes/hours/days/weeks/months/years); "Make this the default schedule for new bookmarks" (ini `statchk.default_cat`); stored `<!--scs=…-->` (§5.4.3) | `schedule(name, interval_secs, is_default)`; import from pm35 head; background scheduler in the app (tray-resident) runs due checks and catches up after sleep or startup | P1 | todo |
| CHK-15 | Assign schedule per bookmark | Status tab dropdown (§5.4.3; mdata f0) | Field in editor; bulk assign | P1 | todo |
| CHK-16 | Status column and sort by status | §5.4.5 | Column (DISP-02) | P1 | todo |
| CHK-17 | Proxy for checks | Manual proxy host/port, Basic auth; exceptions by host suffix or CIDR (§8.1.10–11; ini `proxy.*`) | Use system proxy (reqwest/WinHTTP auto-config) by default; optional manual override with exceptions list. Drop proxy auth unless asked | P3 | todo |
| CHK-18 | "Identity masquerade" | ini `internet.masq_ident` (no manual text; probably sends a browser-like User-Agent) | Configurable User-Agent | P3 | todo |

### 2.6 Ratings, colours and usage counts

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| RATE-01 | Rating 1–10 (0 = not rated) | Slider on User tab; "Rating: NR" (b278, exe; mdata f11) | Stored; shown as stars (½-star per point). Needs an editor | P1 | partial (imported/displayed) |
| RATE-02 | Ctrl+Plus/Minus rating | §10.3 | Same keys on selection | P1 | todo |
| RATE-03 | Sort/filter by rating | Column sort (§5.3) | Column sort plus query filter (e.g. `rating>=7`) | P1 | todo |
| RATE-04 | Colour by rating | readme; ini `colors.ratings` | DISP-09 | P2 | todo |
| USE-01 | Visit count | Counts visits opened via Powermarks (b278; mdata f10); "Visited %d time%s" | Increment on open from the app or extension; column; sort by "most used" | P1 | todo |
| USE-02 | Last visited timestamp | mdata f2 | Set on open | P1 | todo |
| USE-03 | Disable usage tracking | Privacy option (§8.1.2; ini `gui.no_usage_count`) | Setting | P2 | todo |
| USE-04 | Mark Visited | Bookmarks > Mark Visited (§10.1.4) | Command on selection | P2 | todo |
| USE-05 | Count visits made directly in the browser | Not in original. The "Access Cache" (`pm35.cache`, ini `misc.readonly_cache`) appears to store visit data for read-only files (unconfirmed) | Optional: extension `history.onVisited` for bookmarked URLs updates counts (off by default, for privacy) | P3 | todo |

### 2.7 Import and export

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| IMP-01 | Import pm35.htm losslessly | Native format (§7) | Parser plus writer round-trip; store import of every bookmark field plus raw mdata | P1 | done |
| IMP-02 | Import pm35 head/tail blocks | `document=`, `scs=`, `macro=`, `kword=` (format spec) | Parser keeps them as raw head/tail; store must import schedules, macros and tagged keywords | P1 | todo |
| IMP-03 | Import into a non-empty collection | Import merges and skips duplicate URLs (§7) | Currently only into an empty store. Need a merge import that dedupes by URL | P1 | todo |
| IMP-04 | Netscape bookmark HTML import | Folder names → keywords (§7.1, b297) | Universal format exported by every browser; parse `<H3>` path → keywords | P1 | todo |
| IMP-05 | Any HTML file | Extracts all links, honours `<BASE>` (b286) | Same parser, links outside `<DL>` too | P2 | todo |
| IMP-06 | Old Powermarks 1.0/2.0/3.0 files | Import plus "Convert" dialog with backup (§7, exe) | Only if the user has such files; see Q6 | P3 | todo |
| IMP-07 | IE Favorites folder (.url) | §7.2 | Edge/IE legacy `.url` folder import (simple INI parse) | P3 | todo |
| IMP-08 | Opera .adr hotlist; Mozilla bookmarks.html | §7.3, b326 | Drop .adr; Mozilla HTML = IMP-04 | P3 | drop |
| IMP-09 | Import Wizard (auto-detect installed browsers) | §7.3 | "Import from browser": Chrome/Edge `Bookmarks` JSON, Firefox `places.sqlite` (read-only copy), or via the extension `bookmarks` API | P1 | todo |
| IMP-10 | Sync with browser bookmarks | F6 / at startup: add **new** browser bookmarks not yet in Powermarks; per-browser include flags (§8.1.1, §8.1.13–17; ini `*.autosync`, `startup.browser_sync`) | Extension (with `bookmarks` permission) pushes new or changed browser bookmarks, or one-shot scan of browser files. One-way, browser → PM | P2 | todo |
| IMP-11 | Import from clipboard | b290 | ADD-07 | P1 | todo |
| IMP-12 | Import report | "%u Powermarks imported from %s"; Wizard: "%d Favorites, %d Bookmarks…" (exe) | Toast with added/skipped counts | P1 | partial |
| EXP-01 | Export pm35 (3.5) format | "can be imported at a later time with no loss of information" (exe) | Store → pm35 writer using raw mdata plus edited fields. Escape hatch back to the original program | P1 | todo |
| EXP-02 | Export Netscape HTML | Without notes/keywords (exe) | Netscape HTML with `TAGS="…"` attribute (Firefox/Pinboard keep tags) | P1 | todo |
| EXP-03 | Export printable HTML | Title, fields chosen (§5.6) | Printable HTML or app Print view (`window.print()`) | P2 | todo |
| EXP-04 | Delimited text | Delimiter string configurable (§7.4; ini `export.delimit_str`) | CSV (RFC 4180) | P2 | todo |
| EXP-05 | ASCII text / RTF | §7.4 | Drop RTF; plain text or Markdown list | P3 | drop |
| EXP-06 | Old PM 1.0/2.0/3.0 formats | §7.4 | Drop: no consumer exists | P3 | drop |
| EXP-07 | Export options | Range All/Selected; sort field and order; field checklist; title; overwrite warning (§7.4; ini `export.*`) | Same in one dialog; default "current results" as a third range | P2 | todo |
| EXP-08 | JSON export/import | Not in original | Full-fidelity JSON (for scripts/backups) | P2 | todo |

### 2.8 Browser integration

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| BR-01 | Open bookmark in preferred browser | Double-click/Enter (§4.8) | Opens in OS default browser | P1 | done |
| BR-02 | Open in a specific browser | Context menu lists installed browsers (§5.7) | Enumerate `HKLM/HKCU\SOFTWARE\Clients\StartMenuInternet`; launch exe with URL | P2 | todo |
| BR-03 | Preferred browser setting | §8.1.12; ini `browser.preferred` | Default = OS default; optional override from BR-02 list | P2 | todo |
| BR-04 | Always open in new window | Per-browser option; Shift+Enter (b326, §10.3) | Shift+Enter → `--new-window` (Chromium) / `-new-window` (Firefox) when the browser is known; else default | P2 | todo |
| BR-05 | Reuse running browser vs start preferred | ini `browser.always_use_preferred` (§8.1.12, b342) | Drop; the OS/browser handles it | P3 | drop |
| BR-06 | Don't steal focus / bring to front | b342; ini `*.bring_to_forefront` | Drop (browser decides) | P3 | drop |
| BR-07 | Minimise PM after opening | §8.1.2; ini `gui.minimize_on_open_url` | Setting (hide to tray after open) | P2 | todo |
| BR-08 | Run browser maximised; launch browser at startup; Run IE/Netscape… (Alt+I etc.) | §8.1.13–17, §10.1.6 | Drop: browsers manage their own windows | P3 | drop |
| BR-09 | DDE/COM/Direct methods, Bypass DDE, Use Netscape 4, profile dialog wait, Opera "clear saved windows", exec_wait/sleep_on_startup | §8.1.13–15; ini `[netscape]`, `[explorer]`, `[opera]` | Drop: replaced by the extension (gets the active tab URL/title directly) and the OS shell open | P3 | drop |
| BR-10 | IE toolband / link-bar buttons (Show PM, Add, Fast Add) | §8.1.13; ini `explorer.show_toolband`, `integrate_linkbar`; b282 transparent toolbar | Extension toolbar button (Add), context-menu items (Add / Fast Add / Add link), and an "Open Powermarks" item that asks the host to focus the app | P1 | partial |
| BR-11 | Add link under cursor | Not in original | Context menu on links: "Fast add link to Powermarks" (fetch fills metadata) | P2 | todo |
| BR-12 | Search Powermarks from the browser | Not in original | Omnibox keyword (`pm <query>`) via host search → suggestions. High value with no browser-side UI | P2 | todo |
| BR-13 | Mail links go to the mail client | b297 | `mailto:` opened via shell | P2 | todo |
| BR-14 | Firefox and Edge support | Many browsers (§3.1) | MV3 extension works in Chrome/Edge; Firefox needs host manifest registration under the Mozilla key and `gecko` id (present) | P1 | partial (verify Firefox registration) |
| BR-15 | Live list refresh when the extension adds | Single process, no issue | Host writes to SQLite; app must notice (SQLite `data_version` poll or a host → app local IPC ping) and refresh | P1 | todo |

### 2.9 NetSync (backup and multi-PC sync)

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| SYNC-01 | Synchronize two-way | Kaylon server; merge of bookmarks, macros and schedules; edits = delete plus add (§6.2) | **Folder-based sync**: each device writes its change log (ops with per-record UUID, timestamps, tombstones) to a user-chosen folder (OneDrive/Dropbox/Syncthing). Merge on startup or on a timer. No server | P2 | todo |
| SYNC-02 | Upload-only (backup) | §6.2 | Covered by BAK-02 (snapshots into the synced folder) | P2 | todo |
| SYNC-03 | Download-only (restore) | §6.2 | "Restore from backup/snapshot" | P2 | todo |
| SYNC-04 | Erase server; change password; account request; lost password | §6.2, §6.5 | Drop: no server or account | P3 | drop |
| SYNC-05 | Clock-skew check and fix (>30 min) | §6.2; ini `netsync.auto_fixtime` | Drop: use logical/HLC clocks in the change log | P3 | drop |
| SYNC-06 | Progress/details/error dialog | §6.2 | Footer status plus a sync log entry | P3 | todo |
| SYNC-07 | Multiple accounts = multiple collections | §6.3 | Ties to Q4 (multiple collections) | P3 | todo |
| SYNC-08 | Privacy/encryption | Passwords hashed; bookmarks sent unencrypted (§6.6) | No third party sees the data except the user's own cloud folder; optional age/XChaCha encryption of sync files (P3) | P3 | todo |

### 2.10 System tray, hotkeys and startup

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| TRAY-01 | Tray icon; double-click restores | §5.9; ini `gui.systray_icon` | Tauri tray icon | P1 | todo |
| TRAY-02 | Tray menu = top N of current results | Right-click shows the top bookmarks of the current list, N configurable; plus Add, Fast Add, Restore, Exit (§5.9, §8.1.5; ini `gui.systray_menu_count`) | Tray menu rebuilt on result change (cap about 20) | P2 | todo |
| TRAY-03 | Separate Add / Fast Add tray icons | b340; ini `systray_add_icon`, `systray_fast_add_icon` | Drop: the extension button covers it. Tray menu could offer "Add from clipboard" | P3 | drop |
| TRAY-04 | Close to tray | b326; ini `gui.close_to_systray` | Intercept close → hide | P1 | todo |
| TRAY-05 | Hide from taskbar when minimised | §8.1.5; ini `gui.taskbar_hide` | `skip_taskbar` when hidden | P2 | todo |
| HOT-01 | Global hotkey: switch to Powermarks | §8.1.7; ini `hotkeys.app` | `tauri-plugin-global-shortcut`; show window, focus and select the search box. This is the main way in | P1 | todo |
| HOT-02 | Global hotkey: add / fast add from browser | ini `hotkeys.browser_add`, `fast_add` | Extension `commands` (Alt+Shift+P exists for the popup); add a fast-add command. Optionally `"global": true` in Chrome | P1 | partial |
| HOT-03 | Global hotkey: add from clipboard | ini `hotkeys.clipboard_add` | Global shortcut → ADD-07 | P2 | todo |
| HOT-04 | In-app shortcut set | §10.3 (Ins, Ctrl+Ins, Alt+Ins, Ctrl+A/C/E/F/I/N/O/P/S, F5–F9, Del, Enter, Shift+Enter, Ctrl+±, Alt+Enter Properties, Shift+F1 context menu) | Keep the same keys where they don't conflict with WebView defaults | P1 | todo |
| HOT-05 | Shortcut list | Help > Shortcut Keys (b282) | `?` overlay | P2 | todo |
| START-01 | Start with Windows; start minimised | §5.10, §8.1.1; ini `startup.startup_link`, `start_minimized` | `tauri-plugin-autostart` with `--minimized` → tray | P1 | todo |
| START-02 | Reload last file | ini `startup.load_file` | Implicit (single DB) | P1 | done (implicit) |
| START-03 | Import browser bookmarks at startup | §8.1.1 | IMP-10 trigger | P3 | todo |
| START-04 | Single instance | Implied (hotkey switches to the running app) | `tauri-plugin-single-instance`: a second launch focuses the first | P1 | todo |

### 2.11 Options and settings

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| OPT-01 | Options dialog with tree of pages | Startup, UI, Fonts, Colors, Tray, Date/Time, Hotkeys, Status Checking, Icons, Proxy, Proxy Exceptions, Browsers (+5 browser tabs), Diagnostics (§8.1) | One Settings page: General (startup/tray), Search, Display, Hotkeys, Checking, Import/Export, Sync, Advanced. Drop the per-browser tabs | P1 | todo |
| OPT-02 | Settings portable in one file | pm35.ini (§8) | `settings` table in the DB (moves with backups), or `settings.json` in AppData | P1 | todo |
| OPT-03 | Import pm35.ini settings | Not in original | Optional one-shot mapping of relevant keys (search, display, hotkeys, fast-add keywords) | P3 | todo |
| OPT-04 | Process priority boost | ini `gui.boost_priority` | Drop | P3 | drop |

### 2.12 Backups, autosave, undo and files

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| BAK-01 | Backup files | `.ba0` = previous save, `.ba1` = largest collection in the past couple of months (§8.1.2; ini `misc.make_backup`) | Daily rotating `VACUUM INTO` snapshots (keep 7 daily + 4 weekly) **plus** a "high-water mark" snapshot whenever the count reaches a new maximum. Protects against mass deletion just like `.ba1` | P1 | todo |
| BAK-02 | Restore from backup | FAQ: New → Import .ba0/.ba1 → Save As (§9) | "Restore…" lists snapshots with date and count | P1 | todo |
| BAK-03 | Autosave every N minutes | §8.1.2; ini `autosave.*` | Drop: SQLite commits each change | P1 | done (by design) |
| BAK-04 | Undo | Edit > Undo "%s" for Delete Powermark(s), Bookmark Change, Set Automatic Checking, Delete Macro(s), Delete Keyword(s) (exe) | Undo stack (Ctrl+Z) for delete, edit, bulk keyword ops, keyword rename/delete; store inverse ops | P1 | todo |
| BAK-05 | Save prompt / dirty state | "The current bookmark list has been modified, would you like to save it?" (exe) | Drop (autosave) | P3 | drop |
| FILE-01 | Multiple bookmark files: New/Open/Save As, MRU (4) | §10.1.1; ini `mru.file_0..3` | See Q4. Default: one collection | P3 | todo |
| FILE-02 | File changed by another program: Overwrite / Merge / Discard | exe dialog "File Changed" | Drop: SQLite WAL handles concurrent app and host writers | P3 | drop |
| FILE-03 | Read-only files | Open read-only; offers Save As; Access Cache for visits (exe; §1.2) | Drop unless multiple collections are added | P3 | drop |
| FILE-04 | File Information | Counts of bookmarks/macros/schedules/keywords; user-field label; read-only flag (exe) | "About this collection" in Settings: counts plus user-field label | P3 | todo |
| FILE-05 | `.hpm` file association; file on the command line | b290, b280 | Optional: open a `.htm` passed on the command line → import dialog | P3 | todo |

### 2.13 Miscellaneous

| ID | Feature | Original (source) | Modern approach | Pri | Status |
|---|---|---|---|---|---|
| MISC-01 | License entry, 30/45-day trial, purchase menu | §2, §4.2, §13 | Drop: personal rebuild | P3 | drop |
| MISC-02 | Check for new version | Help menu, manual only (b278) | Tauri updater against GitHub releases, manual only | P3 | todo |
| MISC-03 | Bug report sending diagnostics to Kaylon | §10.1.7 | Drop; "Copy diagnostics" button instead | P3 | drop |
| MISC-04 | Diagnostic log with categories, View Log | §8.1.18; ini `[syslog]` (control, hotkeys, sendkeys, processes, sockets, files, netsync, browsers, startup, http, gui) | `tracing` to a rolling file in AppData; level setting; "Open log" | P2 | todo |
| MISC-05 | HTML help manual | Chapters 1–13 | Short in-app help plus `?` shortcut overlay | P3 | todo |
| MISC-06 | Privacy guarantee | No telemetry; network only for check/fetch/connectivity/version/sync (§1.1) | Keep and document; no analytics, no remote calls except user-triggered or scheduled checks | P1 | done (so far) |
| MISC-07 | Large collections | 60k bookmarks, fast load/search/sort (b278) | Benchmark target: 100k rows, search under 30 ms, startup under 1 s | P1 | partial |
| MISC-08 | Native host input validation | n/a | Host accepts only known message types, http(s), size-capped fields | P1 | done |

---

## 3. Original quirks and behaviours worth preserving

1. **Keyword search looks only at keywords** (§10.5). This kept results tight. The rebuild
   currently matches words anywhere (title, description, notes, URL), which is broader and noisier.
   See Q1.
2. **Prefix match by default; `word.` forces a whole word** (§10.5.1). `win.` ≠ `windows`.
3. **Operators:** `&` / `|` / `!` and the dotted forms `.and .or .not`. The dot exists so that
   plain `or` stays a search word. Implicit AND. Parentheses work.
4. **Implied OR** is a hidden ini switch (`search.implied_or`). Keep it as an advanced setting, off.
5. **Keyword character rules** (§5.2.1) keep operators unambiguous: no `& | ! ( ) . , :` etc.;
   `.` only inside a word (`v1.0`). Enforce them on input, and when importing keywords from
   browsers, split on those characters.
6. **Exact string search** is plain substring (no word logic) over user-chosen fields, with
   optional match case. This is the "search the URL for `.edu`" tool.
7. **Results-driven dictionary:** after a space, the dictionary shows only keywords present in the
   current results, so it drills down like faceted search. The status bar lists matching words
   while you type.
8. **Duplicate URLs are never created:** Add shows the existing bookmark, Fast Add does nothing,
   and import skips duplicates (§4.4, §7). The original seems to use exact URL match
   (`cxl_url_match` in the exe; normalisation rules unknown). Decide whether to normalise
   trailing `/`, `http` vs `https` and `www.` (Q3).
9. **Fetch merges keywords but replaces name and description** (§5.2.2). The status message says
   exactly what changed.
10. **Fast Add tags new bookmarks** with configurable keywords (e.g. `new`) so they can be
    reviewed later. This is the inbox workflow.
11. **Folder names become keywords on import** (b297), and are *not* filtered by the stop list.
    "sea eng" finds a "search engines" folder.
12. **"Changed" is sticky until visited:** a changed page is not rechecked (unless that option is
    set), and Mark Visited/opening clears it.
13. **Redirect repair only on permanent redirects.**
14. **Two-level sort**: the previous sort key breaks ties.
15. **The tray menu mirrors the current search results**, so it works as a quick launcher.
16. **`.ba1` keeps the largest collection seen recently**, a cheap guard against catastrophic
    deletes. Keep the idea.
17. **Ratings 1–10 with "NR"** rather than 5 stars. Keep the 10-step granularity in storage.
18. **Visits are counted only when opened via Powermarks**, and the count can be turned off for
    privacy.
19. **Undo with named actions** ("Undo Delete Powermarks").
20. **Esc clears the search; the list always shows everything when the search is empty.**
    The current rebuild shows the newest 500 when empty. Default sort for the empty search is Q7.

---

## 4. Decisions (2026-10-21)

- **Search (Q1):** match everywhere, rank keyword hits first (FTS5 column weights); keyword-only stays available as a mode.
- **Round-trip (Q11):** one-time Export to pm35.htm as an escape hatch; new features are not limited to what 3.5 can store.
- **Sync (Q5):** two-way multi-PC sync is wanted (SYNC-01 → P1-later, after core editing/search work).
- **Data point for Q8:** every imported bookmark has the `Weekly` schedule; ~250 of 13,057 were ever checked.

## 5. Open questions

1. **Keyword vs everywhere search (SRCH-02).** Make strict keyword-only the default (original), or
   keep "matches anywhere" as the default with keyword-only as a mode? A third option: rank
   keyword hits first, then others.
2. **Fast Add from the desktop side.** The original's global hotkey grabbed the URL from the
   frontmost browser. Is an extension shortcut (works only while the browser is focused) enough,
   or do you want a global desktop hotkey? The latter needs a persistent app → extension channel
   (`connectNative` port kept open by the extension).
3. **URL duplicate normalisation**: exact match only (original), or treat `http/https`, `www.` and
   a trailing `/` as the same?
4. **Multiple collections**: did you use more than one `.htm` file (New/Open/MRU)? If not,
   FILE-01/03/SYNC-07 stay dropped.
5. **Sync**: do you actually need multi-PC sync (SYNC-01), or are rotating backups into a
   OneDrive folder enough?
6. **Old formats**: any Powermarks 1.0/2.0/3.0 files or Opera hotlists left to import?
7. **Empty-search default order**: newest added (current), most visited, or alphabetical (original
   default from ini `display.sort_prop`)?
8. **Status checking appetite**: how many bookmarks have schedules today (mdata f0 is set)?
   This decides whether the scheduler (CHK-14) is P1 or can wait behind manual checks.
9. **Browser bookmark sync (IMP-10)**: one-shot import is enough, or continuous one-way pickup of
   bookmarks made with the browser's own star button?
10. **Macro hotkeys**: did macro hotkeys work globally in 3.5 or only in-app? Unknown from the
    sources. Which do you want?
11. **Round-trip expectation**: must every edit made in the new app export back to a pm35.htm that
    Powermarks 3.5 still opens (EXP-01 as an ongoing escape hatch), or is a one-way migration fine?
12. **Unknown ini keys** `search.mpset`, `internet.masq_ident`, `startup.itec`,
    `startup.show_obsolete_warning`: no documentation found. Probably safe to ignore. Shout if any
    of them changed behaviour you relied on.
