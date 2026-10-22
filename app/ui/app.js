const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);
const q = $('q');
const list = $('list');
const panel = $('panel');
const status = $('status');
const importBtn = $('import');
// Virtual scrolling: fixed row heights; only rows near the viewport exist in the DOM.
const ROW_H = { cards: 50, table: 26 };
const BUFFER = 15; // rows rendered beyond each edge of the viewport

let ids = [];              // every matching bookmark id, in display order
const cache = new Map();   // id -> bookmark, for rows fetched so far
let total = 0;
let cursor = 0;            // index in ids of the focused item
let anchor = 0;            // start of a Shift range
let selected = new Set();  // bookmark ids
let editing = null;        // bookmark shown in the form
let chips = [];            // its keywords while editing
let dirty = false;
let seq = 0;
let version = null;
let narrowed = [];         // keyword dictionary for the current search
let dictionary = [];       // every keyword (autocomplete, tag state)

// Search options persist per machine; storage may be unavailable, so never depend on it.
const DEFAULT_OPTS = { mode: 'all', partial: true, fields: ['title', 'url', 'keywords', 'description', 'notes'], match_case: false, sort: [] };
let opts = { ...DEFAULT_OPTS };
try { opts = { ...DEFAULT_OPTS, ...JSON.parse(localStorage.getItem('searchOptions') || '{}') }; } catch {}

// View: card list or Powermarks-style table with chosen columns.
const date = (t) => (t ? new Date(t * 1000).toLocaleDateString() : '');
const shortUrl = (u) => u.replace(/^(https?|ftp):\/\/(www\.)?/i, '');
const stars = (r) => (r > 0 ? '★'.repeat(Math.ceil(r / 2)) + ' ' : '');
// Link-check results as shown to people (symbol + words; the symbol alone marks cards).
const CHECK_LABELS = {
  ok: '✓ OK', changed: '◆ Changed', not_found: '✗ Not found', server_not_found: '✗ Site not found',
  bad_url: '✗ Bad URL', forbidden: '⛔ Blocked', unauthorized: '🔒 Needs login', rate_limited: '⏳ Rate limited',
  server_error: '⚠ Server error', client_error: '⚠ Error', no_response: '⚠ No response',
  network_error: '⚠ Network error', redirect_error: '⚠ Redirect loop', skipped: '– Not a web page',
};
function checkLabel(b) {
  if (!b.check_enabled) return '– Off';
  if (!b.check_status) return '';
  const label = CHECK_LABELS[b.check_status] ?? b.check_status;
  return b.http_status && !['ok', 'changed'].includes(b.check_status) ? `${label} (${b.http_status})` : label;
}
const checkMark = (b) => (b.check_enabled && b.check_status && b.check_status !== 'ok' ? checkLabel(b).split(' ')[0] + ' ' : '');
const COLUMNS = [
  { id: 'title', label: 'Title', width: '34%', cell: (b) => stars(b.rating) + (b.title || b.url) },
  { id: 'url', label: 'URL', width: '24%', cell: (b) => shortUrl(b.url) },
  { id: 'keywords', label: 'Keywords', width: '18%', cell: (b) => b.keywords },
  { id: 'description', label: 'Description', width: '20%', cell: (b) => b.description },
  { id: 'notes', label: 'Notes', width: '16%', cell: (b) => b.notes },
  { id: 'added', label: 'Added', width: '7em', cell: (b) => date(b.added), desc: true },
  { id: 'last_visited', label: 'Last visited', width: '7em', cell: (b) => date(b.last_visited), desc: true },
  { id: 'last_modified', label: 'Last modified', width: '7em', cell: (b) => date(b.last_modified), desc: true },
  { id: 'last_checked', label: 'Last checked', width: '7em', cell: (b) => date(b.last_checked), desc: true },
  { id: 'visits', label: 'Visits', width: '4.5em', cell: (b) => String(b.visits), num: true, desc: true },
  { id: 'rating', label: 'Rating', width: '4.5em', cell: (b) => (b.rating ? String(b.rating) : 'NR'), num: true, desc: true },
  { id: 'check_status', label: 'Status', width: '9em', cell: checkLabel },
];
let view = 'cards';
let shownCols = ['title', 'url', 'keywords', 'added', 'visits', 'rating'];
try {
  view = localStorage.getItem('view') || view;
  shownCols = JSON.parse(localStorage.getItem('columns') || 'null') || shownCols;
} catch {}
const tablewrap = $('tablewrap');

const say = (text) => { status.textContent = text; };
const fail = (what) => (e) => say(`${what} failed: ${e}`);
const words = (s) => s.split(/[\s,]+/).filter(Boolean);
// Search terms without operators, for "is this keyword already in the query?".
const terms = (s) => s.split(/[\s&|!()]+/).map((w) => w.replace(/\.+$/, '')).filter((w) => w && !/^\.(and|or|not)$/i.test(w));
const lower = (s) => s.toLocaleLowerCase();

// ---- data -------------------------------------------------------------------

// aria-busy is "true" while any search or user action is in flight (assistive tech, and the
// e2e tests wait on it). A counter, because actions overlap: a save includes a refresh.
let busy = 0;
async function busyWhile(work) {
  busy++;
  document.body.ariaBusy = 'true';
  try {
    return await work();
  } finally {
    if (--busy === 0) document.body.ariaBusy = 'false';
  }
}
const busyAware = (fn) => (...args) => busyWhile(() => fn(...args));

function refresh(options) {
  const mine = ++seq;
  return busyWhile(() => runRefresh(mine, options));
}

async function runRefresh(mine, { keepSelection = true } = {}) {
  let n, found, v, kw, hist;
  try {
    [n, found, v, kw, hist] = await Promise.all([
      invoke('count'), invoke('search_ids', { query: q.value, opts }), invoke('data_version'),
      invoke('keywords', { query: q.value, opts }), invoke('history'),
    ]);
  } catch (e) {
    if (mine !== seq) return;
    // Bad syntax: say why and keep showing the last good results.
    q.setAttribute('aria-invalid', 'true');
    say(`Search: ${e}`);
    return;
  }
  if (mine !== seq) return; // a newer search already rendered
  q.removeAttribute('aria-invalid');
  showHistory(hist);
  const cursorId = ids[cursor];
  total = n;
  ids = found;
  version = v;
  narrowed = kw;
  cache.clear(); // rows may have changed; visible ones are refetched below
  const present = new Set(ids);
  selected = keepSelection ? new Set([...selected].filter((id) => present.has(id))) : new Set();
  cursor = Math.max(0, keepSelection ? ids.indexOf(cursorId) : 0);
  if (!keepSelection) scroller().scrollTop = 0;
  if (pendingReveal !== null && ids.includes(pendingReveal)) {
    selected = new Set([pendingReveal]);
    cursor = anchor = ids.indexOf(pendingReveal);
    pendingReveal = null;
    revealCursor();
  }
  if (selected.size === 0 && ids.length) selected.add(ids[cursor]);
  await fetchRows([...ids.slice(...windowRange()), ...ids.slice(0, TRAY_RESULTS), ...selected, ids[cursor]]);
  if (mine !== seq) return;
  render();
  renderKeywords();
  updateTray();
}

// The tray menu lists the top results of the current search, like the original's.
const TRAY_RESULTS = 15;
let trayItems = '';
function updateTray() {
  const items = ids.slice(0, TRAY_RESULTS).filter((id) => cache.has(id)).map((id) => [id, cache.get(id).title || cache.get(id).url]);
  const key = JSON.stringify(items);
  if (key === trayItems) return;
  trayItems = key;
  invoke('set_tray_results', { items }).catch(() => {});
}

async function fetchRows(wanted) {
  const missing = [...new Set(wanted)].filter((id) => id !== undefined && !cache.has(id));
  if (!missing.length) return;
  for (const b of await invoke('get_bookmarks', { ids: missing })) cache.set(b.id, b);
}

async function refreshDictionary() {
  dictionary = await invoke('keywords', { query: '', opts: {} });
  // Tagged keywords first, then most used: the order autocomplete shows them in.
  const sorted = [...dictionary].sort((a, b) => (b.tagged - a.tagged) || (b.count - a.count));
  $('kw-options').replaceChildren(...sorted.map((k) => new Option(k.keyword)));
}

// Live refresh: the browser extension writes through another process.
setInterval(async () => {
  if (dirty || document.hidden) return;
  const v = await invoke('data_version').catch(() => version);
  if (v !== version) { await refreshDictionary(); refresh(); }
}, 1500);

// ---- bookmark list ----------------------------------------------------------

const scroller = () => (view === 'table' ? tablewrap : list);

/** [first, end) indexes of ids worth rendering for the current scroll position. */
function windowRange() {
  const el = scroller();
  const h = ROW_H[view];
  const first = Math.max(0, Math.floor(el.scrollTop / h) - BUFFER);
  return [first, Math.min(ids.length, first + Math.ceil(el.clientHeight / h) + 2 * BUFFER)];
}

// Titles come from imported files and web pages: textContent only, never innerHTML.
function render() {
  renderRows();
  renderPanel();
  say(total === 0
    ? 'No bookmarks yet. Use Import… to bring them in from your browsers or a Powermarks database.'
    : `${ids.length} matching · ${total} bookmarks · ${selected.size} selected · Enter opens · Del deletes`);
}

function renderRows() {
  if (view === 'table') renderTable(); else renderCards();
}

let scrollQueued = false;
function onScroll() {
  if (scrollQueued) return;
  scrollQueued = true;
  requestAnimationFrame(async () => {
    scrollQueued = false;
    renderRows(); // placeholders first, so scrolling never waits on the database
    const before = cache.size;
    await fetchRows(ids.slice(...windowRange()));
    if (cache.size !== before) renderRows();
  });
}

/** Scroll just enough to show the cursor row. */
function revealCursor() {
  const el = scroller();
  const h = ROW_H[view];
  const head = view === 'table' ? $('table-head').offsetHeight : 0;
  const top = cursor * h;
  if (top < el.scrollTop) el.scrollTop = top;
  else if (top + h + head > el.scrollTop + el.clientHeight) el.scrollTop = top + h + head - el.clientHeight;
}

function spacer(tag, height, colspan) {
  const el = document.createElement(tag);
  el.className = 'spacer';
  el.ariaHidden = 'true';
  if (tag === 'tr') {
    const td = document.createElement('td');
    td.colSpan = colspan;
    td.style.height = `${height}px`;
    el.append(td);
  } else {
    el.style.height = `${height}px`;
  }
  return el;
}

function renderTable() {
  const cols = COLUMNS.filter((c) => shownCols.includes(c.id));
  $('table-cols').replaceChildren(...cols.map((c) => Object.assign(document.createElement('col'), { style: `width:${c.width}` })));
  $('table-head').replaceChildren(...cols.map((c) => {
    const th = document.createElement('th');
    th.scope = 'col';
    const level = opts.sort.findIndex((k) => k.column === c.id);
    th.ariaSort = level === 0 ? (opts.sort[0].desc ? 'descending' : 'ascending') : 'none';
    const btn = document.createElement('button');
    btn.textContent = c.label;
    if (level >= 0) {
      const mark = document.createElement('span');
      mark.className = level ? 'sort2' : '';
      mark.textContent = (opts.sort[level].desc ? ' ▼' : ' ▲') + (level ? '2' : '');
      btn.append(mark);
    }
    btn.title = 'Sort (click again to reverse, a third time to clear)';
    btn.onclick = () => sortBy(c);
    th.append(btn);
    return th;
  }));
  const [a, z] = windowRange();
  const h = ROW_H.table;
  const body = [];
  if (a > 0) body.push(spacer('tr', a * h, cols.length));
  for (let i = a; i < z; i++) {
    const b = cache.get(ids[i]);
    const tr = document.createElement('tr');
    tr.className = 'vrow';
    tr.ariaSelected = String(selected.has(ids[i]));
    tr.classList.toggle('sel', selected.has(ids[i]));
    tr.classList.toggle('cursor', i === cursor);
    tr.append(...cols.map((c) => {
      const td = document.createElement('td');
      td.textContent = b ? c.cell(b) : (c.id === 'title' ? '…' : '');
      td.title = td.textContent;
      if (c.num) td.className = 'num';
      return td;
    }));
    tr.onclick = (e) => select(i, e);
    tr.ondblclick = () => open(ids[i]);
    tr.oncontextmenu = (e) => { e.preventDefault(); showBookmarkMenu(e, i); };
    body.push(tr);
  }
  if (z < ids.length) body.push(spacer('tr', (ids.length - z) * h, cols.length));
  $('table-body').replaceChildren(...body);
}

// Like Powermarks: the new column sorts first and the previous one breaks ties.
function sortBy(c) {
  const [first] = opts.sort;
  if (first?.column === c.id) {
    if (first.desc === !!c.desc) first.desc = !first.desc; // second click: reverse
    else opts.sort.shift(); // third click: back to relevance
  } else {
    opts.sort = [{ column: c.id, desc: !!c.desc }, ...(first ? [first] : [])];
  }
  saveOpts();
  refresh();
}

function renderCards() {
  const [a, z] = windowRange();
  const h = ROW_H.cards;
  const items = [];
  if (a > 0) items.push(spacer('li', a * h));
  for (let i = a; i < z; i++) {
    const b = cache.get(ids[i]);
    const li = document.createElement('li');
    li.className = 'vrow';
    li.role = 'option';
    li.ariaSelected = String(selected.has(ids[i]));
    li.classList.toggle('sel', selected.has(ids[i]));
    li.classList.toggle('cursor', i === cursor);
    const title = document.createElement('div');
    title.className = 'title';
    title.textContent = b ? checkMark(b) + stars(b.rating) + (b.title || b.url) : '…';
    const meta = document.createElement('div');
    meta.className = 'meta';
    meta.textContent = b ? [b.keywords, b.url].filter(Boolean).join(' · ') : '';
    li.append(title, meta);
    li.onclick = (e) => select(i, e);
    li.ondblclick = () => open(ids[i]);
    li.oncontextmenu = (e) => { e.preventDefault(); showBookmarkMenu(e, i); };
    items.push(li);
  }
  if (z < ids.length) items.push(spacer('li', (ids.length - z) * h));
  list.replaceChildren(...items);
}

function setView(v) {
  view = v;
  try { localStorage.setItem('view', v); } catch {}
  list.hidden = v === 'table';
  tablewrap.hidden = v !== 'table';
  $('cols').hidden = v !== 'table';
  $('view-cards').ariaPressed = String(v === 'cards');
  $('view-table').ariaPressed = String(v === 'table');
  revealCursor();
  render();
  onScroll(); // fetch whatever the new view shows
}

$('cols-menu').replaceChildren(...COLUMNS.map((c) => {
  const label = document.createElement('label');
  const box = document.createElement('input');
  box.type = 'checkbox';
  box.checked = shownCols.includes(c.id);
  box.disabled = c.id === 'title';
  box.onchange = () => {
    shownCols = COLUMNS.map((x) => x.id).filter((id) => (id === c.id ? box.checked : shownCols.includes(id)));
    try { localStorage.setItem('columns', JSON.stringify(shownCols)); } catch {}
    render();
  };
  label.append(box, c.label);
  return label;
}));
$('view-cards').onclick = () => setView('cards');
$('view-table').onclick = () => setView('table');
list.addEventListener('scroll', onScroll);
tablewrap.addEventListener('scroll', onScroll);
addEventListener('resize', onScroll);

async function select(i, { ctrlKey = false, metaKey = false, shiftKey = false } = {}) {
  if (ids[i] === undefined || !confirmDiscard()) return;
  const id = ids[i];
  if (shiftKey) {
    const [a, b] = [Math.min(anchor, i), Math.max(anchor, i)];
    if (!(ctrlKey || metaKey)) selected.clear();
    ids.slice(a, b + 1).forEach((x) => selected.add(x));
  } else if (ctrlKey || metaKey) {
    selected.has(id) ? selected.delete(id) : selected.add(id);
    anchor = i;
  } else {
    selected = new Set([id]);
    anchor = i;
  }
  cursor = i;
  revealCursor();
  await fetchRows([id]); // the properties panel needs it
  render();
}

function onListKeys(e) {
  const page = Math.max(1, Math.floor(scroller().clientHeight / ROW_H[view]) - 1);
  const moves = { ArrowDown: 1, ArrowUp: -1, PageDown: page, PageUp: -page, End: Infinity, Home: -Infinity };
  const inList = e.target === list || e.target === tablewrap;
  if (e.key in moves && (inList || e.key.startsWith('Arrow') || e.key.startsWith('Page'))) {
    const i = Math.max(0, Math.min(ids.length - 1, cursor + moves[e.key]));
    select(i, { shiftKey: e.shiftKey });
  } else if (e.key === 'Enter') {
    if (ids[cursor] !== undefined) open(ids[cursor]);
  } else if (e.key === 'a' && (e.ctrlKey || e.metaKey) && inList) {
    ids.forEach((x) => selected.add(x));
    render();
  } else if (e.key === 'Delete' && inList) {
    remove([...selected]);
  } else return;
  e.preventDefault();
}

async function open(id) {
  try {
    await invoke('open_bookmark', { id });
    refresh(); // visit count changed
  } catch (e) { say(String(e)); }
}

async function remove(toDelete) {
  if (!toDelete.length) return;
  const one = cache.get(toDelete[0]);
  const what = toDelete.length === 1 ? `"${one?.title || one?.url || 'this bookmark'}"` : `${toDelete.length} bookmarks`;
  if (!confirm(`Delete ${what}? (Ctrl+Z undoes it.)`)) return;
  try {
    const n = await invoke('delete_bookmarks', { ids: toDelete });
    dirty = false;
    selected.clear();
    await refreshDictionary();
    await refresh();
    say(`Deleted ${n} bookmark${n === 1 ? '' : 's'}.`);
  } catch (e) { fail('Delete')(e); }
}

// Right-click a bookmark: it joins the selection (or replaces it, like Explorer).
async function showBookmarkMenu(e, i) {
  if (!selected.has(ids[i])) await select(i);
  if (!selected.has(ids[i])) return; // unsaved edits kept the old selection
  const m = $('bctx');
  const many = selected.size > 1;
  $('bctx-title').textContent = many ? `${selected.size} bookmarks` : (cache.get(ids[i])?.title || cache.get(ids[i])?.url || '');
  const on = [...selected].every((id) => cache.get(id)?.check_enabled);
  m.querySelector('[data-act=checking]').textContent = on ? 'Turn link checking off' : 'Turn link checking on';
  m.querySelector('[data-act=pin]').hidden = many;
  m.querySelector('[data-act=copy]').textContent = many ? 'Copy URLs' : 'Copy URL';
  m.hidden = false;
  m.style.left = `${Math.min(e.clientX, innerWidth - m.offsetWidth - 4)}px`;
  m.style.top = `${Math.min(e.clientY, innerHeight - m.offsetHeight - 4)}px`;
  m.querySelector('button').focus();
}

$('bctx').addEventListener('click', async (e) => {
  const act = e.target.dataset?.act;
  if (!act) return;
  $('bctx').hidden = true;
  const chosen = ids.filter((id) => selected.has(id)); // in list order
  if (act === 'open') chosen.forEach(open);
  else if (act === 'copy') {
    await fetchRows(chosen);
    await navigator.clipboard.writeText(chosen.map((id) => cache.get(id)?.url).filter(Boolean).join('\n'));
    say(chosen.length === 1 ? 'URL copied.' : `${chosen.length} URLs copied.`);
  } else if (act === 'check') startCheck(chosen);
  else if (act === 'checking') setCheckEnabled(chosen, e.target.textContent.endsWith('on'));
  else if (act === 'pin') $('pin-link').click();
  else if (act === 'export') {
    $('export-open').click();
    document.querySelector('input[name=ex-scope][value=selected]').checked = true;
  } else if (act === 'delete') remove(chosen);
});
document.addEventListener('click', (e) => { if (!$('bctx').contains(e.target)) $('bctx').hidden = true; });
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') $('bctx').hidden = true; });

// ---- undo / redo ----------------------------------------------------------------

function showHistory({ undo, redo }) {
  for (const [id, label, verb] of [['undo', undo, 'Undo'], ['redo', redo, 'Redo']]) {
    $(id).disabled = !label;
    $(id).title = label ? `${verb}: ${label} (${id === 'undo' ? 'Ctrl+Z' : 'Ctrl+Y'})` : `Nothing to ${verb.toLowerCase()}`;
  }
}

async function runHistory(which) {
  if (!confirmDiscard()) return;
  try {
    const name = await invoke(which);
    await refreshDictionary();
    await refresh();
    say(name ? `${which === 'undo' ? 'Undid' : 'Redid'}: ${name}` : `Nothing to ${which}.`);
  } catch (e) { fail(which === 'undo' ? 'Undo' : 'Redo')(e); }
}

$('undo').onclick = () => runHistory('undo');
$('redo').onclick = () => runHistory('redo');
document.addEventListener('keydown', (e) => {
  if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
  // Inside text fields Ctrl+Z is the field's own text undo.
  if (e.target.closest('input, textarea, select, [contenteditable]')) return;
  const key = e.key.toLowerCase();
  if (key === 'z' && !e.shiftKey) runHistory('undo');
  else if (key === 'y' || (key === 'z' && e.shiftKey)) runHistory('redo');
  else return;
  e.preventDefault();
});

// ---- link checking ----------------------------------------------------------------

async function setCheckEnabled(toChange, enabled) {
  try {
    await invoke('set_check_enabled', { ids: toChange, enabled });
    await refresh();
  } catch (e) { fail('Change link checking')(e); }
}

// body[data-checking] is "true" while a batch runs (the e2e test waits on it too).
document.body.dataset.checking = 'false';
async function startCheck(toCheck) {
  if (!toCheck.length) return say('Select the bookmarks to check first (or use Check shown).');
  if (toCheck.length > 200 && !confirm(`Check ${toCheck.length.toLocaleString()} links?\n\nThis contacts every one of those websites, a few at a time; it can take a while. You can cancel at any point.`)) return;
  document.body.dataset.checking = 'true'; // before any await, so watchers never see a gap
  try {
    const n = await invoke('start_check', { ids: toCheck });
    showCheckBar({ done: 0, total: n, counts: {} });
  } catch (e) {
    document.body.dataset.checking = 'false';
    fail('Link check')(e);
  }
}

function showCheckBar(p) {
  const running = !p.finished;
  $('check-selected').hidden = running;
  $('check-shown').hidden = running;
  $('check-cancel').hidden = !running;
  const problems = Object.entries(p.counts)
    .filter(([k]) => !['ok', 'changed', 'skipped'].includes(k))
    .reduce((n, [, v]) => n + v, 0);
  $('check-progress').textContent = running
    ? `Checking ${p.done}/${p.total}${problems ? ` · ${problems} problem${problems === 1 ? '' : 's'}` : ''}${p.counts.changed ? ` · ${p.counts.changed} changed` : ''}`
    : '';
}

let checkRefresh = null;
window.__TAURI__.event.listen('check-progress', async ({ payload: p }) => {
  showCheckBar(p);
  if (!p.finished) {
    // Show results as they come in, at most every 1.5 s and never over unsaved edits.
    if (!checkRefresh) checkRefresh = setTimeout(() => { checkRefresh = null; if (!dirty) refresh(); }, 1500);
    return;
  }
  clearTimeout(checkRefresh);
  checkRefresh = null;
  if (!dirty) await refresh();
  const parts = Object.entries(p.counts).sort((a, b) => b[1] - a[1]).map(([k, v]) => `${v} ${(CHECK_LABELS[k] ?? k).replace(/^\S+ /, '').toLowerCase()}`);
  let msg = p.offline ? 'Stopped: you seem to be offline.'
    : `${p.cancelled ? 'Cancelled after' : 'Checked'} ${p.done} link${p.done === 1 ? '' : 's'}${parts.length ? `: ${parts.join(', ')}` : ''}.`;
  if (p.repaired) msg += ` Updated ${p.repaired} moved URL${p.repaired === 1 ? '' : 's'} (Ctrl+Z undoes).`;
  if (p.error) msg += ` Error: ${p.error}`;
  say(msg);
  document.body.dataset.checking = 'false';
});

$('check-selected').onclick = () => startCheck([...selected]);
$('check-shown').onclick = () => startCheck(ids);
$('check-cancel').onclick = () => invoke('cancel_check');
// Ctrl+T works everywhere, including the search box (it means nothing while typing).
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 't') {
    e.preventDefault();
    startCheck([...selected]);
  }
});
showCheckBar({ finished: true, counts: {} });

// ---- settings -------------------------------------------------------------------

$('settings-open').onclick = async () => {
  try {
    const s = await invoke('get_settings');
    $('s-autostart').checked = s.autostart;
    $('s-minimized').checked = s.start_minimized;
    $('s-close').checked = s.close_to_tray;
    $('s-hotkey').value = s.hotkey;
    $('s-repair').checked = s.repair_moved_urls;
    $('s-fast').value = s.fast_add_keywords;
    $('s-error').textContent = '';
    $('settings').showModal();
  } catch (e) { fail('Settings')(e); }
};

// Record the pressed combination instead of free text, e.g. "Ctrl+Alt+P".
$('s-hotkey').addEventListener('keydown', (e) => {
  if (e.key === 'Tab') return;
  e.preventDefault();
  if ((e.key === 'Backspace' || e.key === 'Delete') && !e.ctrlKey && !e.altKey) { e.target.value = ''; return; }
  if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return;
  const key = e.code.startsWith('Key') ? e.code.slice(3) : e.code.startsWith('Digit') ? e.code.slice(5) : e.key.length === 1 ? e.key.toUpperCase() : e.code;
  const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Super'].filter(Boolean);
  e.target.value = [...mods, key].join('+');
});

$('s-save').onclick = busyAware(async () => {
  const settings = {
    close_to_tray: $('s-close').checked,
    start_minimized: $('s-minimized').checked,
    hotkey: $('s-hotkey').value.trim(),
    repair_moved_urls: $('s-repair').checked,
    fast_add_keywords: $('s-fast').value.trim(),
  };
  try {
    await invoke('save_settings', { settings, autostart: $('s-autostart').checked });
    $('settings').close();
    say('Settings saved.');
  } catch (e) { $('s-error').textContent = String(e); }
});

// The global hotkey and the tray bring the window up ready to type.
window.__TAURI__.event.listen('focus-search', () => { q.focus(); q.select(); });

// ---- backups --------------------------------------------------------------------

async function showBackups() {
  const items = await invoke('list_backups');
  const kb = (n) => `${Math.max(1, Math.round(n / 1024)).toLocaleString()} KB`;
  if (!items.length) {
    $('backup-list').replaceChildren(Object.assign(document.createElement('p'), { textContent: 'No backups yet.' }));
    return;
  }
  const table = document.createElement('table');
  const head = table.createTHead().insertRow();
  for (const h of ['Taken', 'Kind', 'Size', '']) head.append(Object.assign(document.createElement('th'), { textContent: h }));
  const body = table.createTBody();
  for (const b of items) {
    const tr = body.insertRow();
    const when = new Date(b.created * 1000).toLocaleString();
    tr.insertCell().textContent = when;
    tr.insertCell().textContent = b.kind;
    Object.assign(tr.insertCell(), { textContent: kb(b.bytes), className: 'num' });
    const btn = Object.assign(document.createElement('button'), { type: 'button', className: 'secondary', textContent: 'Restore' });
    btn.onclick = () => restoreBackup(b.name, when);
    tr.insertCell().append(btn);
  }
  $('backup-list').replaceChildren(table);
}

async function restoreBackup(name, when) {
  if (!confirmDiscard()) return;
  if (!confirm(`Replace all bookmarks with the backup taken ${when}?\n\nYour current bookmarks are backed up first, so this can be reversed from this list.`)) return;
  try {
    const safety = await invoke('restore_backup', { name });
    $('backups').close();
    selected.clear();
    await refreshDictionary();
    await refreshSaved();
    await refresh({ keepSelection: false });
    say(`Restored the backup from ${when}. Previous state saved as ${safety}.`);
  } catch (e) { fail('Restore')(e); }
}

$('backups-open').onclick = async () => {
  try { await showBackups(); $('backups').showModal(); } catch (e) { fail('Backups')(e); }
};
$('backup-now').onclick = async () => {
  try { const name = await invoke('backup_now'); await showBackups(); say(`Backed up as ${name}.`); } catch (e) { fail('Backup')(e); }
};

// ---- saved searches and links (Powermarks' macros) ---------------------------------

let savedItems = [];
const MODE_NAMES = { all: 'All fields', keywords: 'Keywords only', exact: 'Exact text' };

async function refreshSaved() {
  savedItems = await invoke('saved_items');
  renderSaved();
}

function renderSaved() {
  if (!savedItems.length) {
    $('savedlist').replaceChildren(Object.assign(document.createElement('li'), {
      className: 'hint', textContent: 'Save searches with ☆ and pin links from a bookmark.',
    }));
    return;
  }
  $('savedlist').replaceChildren(...savedItems.map((item, i) => {
    const li = document.createElement('li');
    li.title = (item.kind === 'link' ? item.url : item.query) + ' · click to run, right-click for more';
    const icon = document.createElement('span');
    icon.textContent = item.kind === 'link' ? '🔗' : '🔍';
    icon.ariaHidden = 'true';
    const name = document.createElement('span');
    name.textContent = item.name;
    li.append(icon, name);
    if (i < 9) li.append(Object.assign(document.createElement('span'), { className: 'n', textContent: `Alt+${i + 1}` }));
    li.onclick = () => runSaved(item);
    li.oncontextmenu = (e) => { e.preventDefault(); showSavedMenu(e, item, li); };
    return li;
  }));
}

async function runSaved(item) {
  if (item.kind === 'link') {
    try { await invoke('open_link', { url: item.url }); say(`Opened ${item.name}.`); } catch (e) { fail('Open')(e); }
    return;
  }
  if (!confirmDiscard()) return;
  let o = {};
  try { o = JSON.parse(item.options || '{}'); } catch {}
  opts = { ...opts, mode: o.mode ?? 'all', fields: o.fields ?? DEFAULT_OPTS.fields, match_case: !!o.match_case, partial: o.partial ?? true };
  saveOpts();
  applyOptsToUi();
  q.value = item.query;
  await refresh({ keepSelection: false });
  say(`Saved search "${item.name}": ${ids.length} matching.`);
}

// One dialog for saving/editing both kinds. `item.id` 0 means new.
let savedDraft = null;
function openSavedDialog(item) {
  savedDraft = item;
  const link = item.kind === 'link';
  $('saved-title').textContent = `${item.id ? 'Edit' : 'Save'} ${link ? 'link' : 'search'}`;
  $('sv-value-label').textContent = link ? 'URL' : 'Search';
  $('sv-name').value = item.name;
  $('sv-value').value = link ? item.url : item.query;
  let o = {};
  try { o = JSON.parse(item.options || '{}'); } catch {}
  $('sv-mode').textContent = link ? '' : `Mode: ${MODE_NAMES[o.mode ?? 'all']}${o.mode === 'exact' ? ` in ${(o.fields ?? []).join(', ')}${o.match_case ? ', match case' : ''}` : ''}`;
  $('sv-error').textContent = '';
  $('saved-dlg').showModal();
  $('sv-name').select();
}

async function submitSaved() {
  const item = { ...savedDraft, name: $('sv-name').value };
  if (item.kind === 'link') item.url = $('sv-value').value; else item.query = $('sv-value').value;
  try {
    await invoke('save_item', { item });
    $('saved-dlg').close();
    await refreshSaved();
    say(`Saved "${item.name.trim()}".`);
  } catch (e) { $('sv-error').textContent = String(e); }
}

$('saved-form').addEventListener('submit', (e) => { e.preventDefault(); submitSaved(); });

$('save-search').onclick = () => {
  if (!q.value.trim()) return say('Type a search first, then save it with ☆.');
  const { mode, fields, match_case, partial } = opts;
  openSavedDialog({ id: 0, kind: 'search', name: q.value.trim(), query: q.value, options: JSON.stringify({ mode, fields, match_case, partial }), url: '' });
};

let menuSaved = null;
function showSavedMenu(e, item, li) {
  menuSaved = item;
  hideMenu();
  li.classList.add('active');
  const m = $('sctx');
  $('sctx-title').textContent = item.name;
  m.hidden = false;
  m.style.left = `${Math.min(e.clientX, innerWidth - m.offsetWidth - 4)}px`;
  m.style.top = `${Math.min(e.clientY, innerHeight - m.offsetHeight - 4)}px`;
  m.querySelector('button').focus();
}

function hideSavedMenu() {
  $('sctx').hidden = true;
  document.querySelectorAll('#savedlist li.active').forEach((x) => x.classList.remove('active'));
}

$('sctx').addEventListener('click', busyAware(async (e) => {
  const act = e.target.dataset?.act;
  const item = menuSaved;
  if (!act || !item) return;
  hideSavedMenu();
  try {
    if (act === 'run') await runSaved(item);
    else if (act === 'edit') openSavedDialog(item);
    else if (act === 'up' || act === 'down') { await invoke('move_saved_item', { id: item.id, by: act === 'up' ? -1 : 1 }); await refreshSaved(); }
    else if (act === 'delete' && confirm(`Delete the saved ${item.kind} "${item.name}"?`)) { await invoke('delete_saved_item', { id: item.id }); await refreshSaved(); say(`Deleted "${item.name}".`); }
  } catch (err) { fail('Saved item')(err); }
}));
document.addEventListener('click', (e) => { if (!$('sctx').contains(e.target)) hideSavedMenu(); });
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') hideSavedMenu();
  // Alt+1…9 run the first nine saved items (Powermarks gave each macro a hotkey).
  if (e.altKey && !e.ctrlKey && /^[1-9]$/.test(e.key) && savedItems[Number(e.key) - 1]) {
    e.preventDefault();
    runSaved(savedItems[Number(e.key) - 1]);
  }
});

// ---- keyword panel (Powermarks' Dictionary) ---------------------------------

function renderKeywords() {
  const find = lower($('kw-filter').value.trim());
  const taggedOnly = $('kw-tagged').checked;
  const inQuery = new Set(opts.mode === 'exact' ? [] : terms(lower(q.value)));
  const shown = narrowed.filter((k) =>
    (!taggedOnly || k.tagged) && (!find || lower(k.keyword).startsWith(find)) && !inQuery.has(lower(k.keyword)));
  $('kw-head').textContent = `Keywords [${shown.length}]`;
  $('kwlist').replaceChildren(...shown.map((k) => {
    const li = document.createElement('li');
    li.title = 'Click: add to search · Double-click: search only this · Right-click: more';
    const mark = document.createElement('span');
    mark.className = 'tagmark';
    mark.textContent = k.tagged ? '🏷' : '';
    const name = document.createElement('span');
    name.textContent = k.keyword;
    const n = document.createElement('span');
    n.className = 'n';
    n.textContent = k.count;
    li.append(mark, name, n);
    li.onclick = () => addToSearch(k.keyword);
    li.ondblclick = () => setSearch(k.keyword);
    li.oncontextmenu = (e) => { e.preventDefault(); showMenu(e, k, li); };
    return li;
  }));
}

function setSearch(text) {
  if (!confirmDiscard()) return;
  q.value = text;
  refresh({ keepSelection: false });
}

function addToSearch(keyword) {
  if (opts.mode === 'exact') return setSearch(keyword);
  if (!terms(q.value).some((w) => lower(w) === lower(keyword))) setSearch(`${q.value.trim()} ${keyword}`.trim());
}

let menuKeyword = null;
function showMenu(e, k, li) {
  menuKeyword = k;
  document.querySelectorAll('#kwlist li.active').forEach((x) => x.classList.remove('active'));
  li.classList.add('active');
  const ctx = $('ctx');
  $('ctx-title').textContent = k.keyword;
  ctx.querySelector('[data-act=tag]').textContent = k.tagged ? 'Untag keyword' : 'Tag keyword';
  ctx.hidden = false;
  ctx.style.left = `${Math.min(e.clientX, innerWidth - ctx.offsetWidth - 4)}px`;
  ctx.style.top = `${Math.min(e.clientY, innerHeight - ctx.offsetHeight - 4)}px`;
  ctx.querySelector('button').focus();
}

function hideMenu() {
  $('ctx').hidden = true;
  document.querySelectorAll('#kwlist li.active').forEach((x) => x.classList.remove('active'));
}

$('ctx').addEventListener('click', async (e) => {
  const act = e.target.dataset?.act;
  const k = menuKeyword;
  if (!act || !k) return;
  hideMenu();
  try {
    if (act === 'search') setSearch(k.keyword);
    else if (act === 'add') addToSearch(k.keyword);
    else if (act === 'tag') await keywordChanged(invoke('set_tagged', { keyword: k.keyword, tagged: !k.tagged }), '');
    else if (act === 'rename') {
      const to = prompt(`Rename "${k.keyword}" on ${k.count} bookmark(s) to:\n(an existing keyword merges them)`, k.keyword);
      if (to && to.trim() && to.trim() !== k.keyword && confirmDiscard()) {
        await keywordChanged(invoke('rename_keyword', { old: k.keyword, new: to.trim() }), `Renamed on # bookmark(s).`);
      }
    } else if (act === 'delete') {
      if (confirm(`Remove "${k.keyword}" from all bookmarks that use it?`) && confirmDiscard()) {
        await keywordChanged(invoke('rename_keyword', { old: k.keyword, new: '' }), `Removed from # bookmark(s).`);
      }
    }
  } catch (err) { fail('Keyword change')(err); }
});

async function keywordChanged(call, message) {
  const n = await call;
  dirty = false;
  await refreshDictionary();
  await refresh();
  if (message) say(message.replace('#', n));
}

document.addEventListener('click', (e) => { if (!$('ctx').contains(e.target)) hideMenu(); });
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') hideMenu(); });
$('kw-filter').addEventListener('input', renderKeywords);
$('kw-tagged').addEventListener('change', renderKeywords);

// ---- keyword cleanup (Powermarks' Dictionary Cleanup) ------------------------

const CLEANUP_GROUPS = [
  ['common word', 'Common words', true],
  ['number only', 'Numbers only', true],
  ['no letters or digits', 'No letters or digits', true],
  ['stray punctuation', 'Stray punctuation (renamed to the clean word)', true],
  ['plural of another keyword', 'Plurals (merged into the more-used form)', true],
  ['used once', 'Used by only one bookmark (often auto-generated from titles)', false],
];
let cleanupItems = [];
let openGroups = new Set(); // which preview groups are expanded

$('cleanup-open').addEventListener('click', async () => {
  if (!confirmDiscard()) return;
  try {
    const plan = await invoke('cleanup_plan');
    cleanupItems = plan.map((i) => ({ ...i, on: CLEANUP_GROUPS.find((g) => g[0] === i.reason)?.[2] ?? false }));
    openGroups = new Set(CLEANUP_GROUPS.filter((g) => g[2]).map((g) => g[0]));
    renderCleanup();
    $('cleanup').showModal();
  } catch (e) { fail('Cleanup')(e); }
});

function renderCleanup() {
  const groups = CLEANUP_GROUPS.map(([reason, label]) => {
    const items = cleanupItems.filter((i) => i.reason === reason);
    if (!items.length) return null;
    const details = document.createElement('details');
    const summary = document.createElement('summary');
    const all = document.createElement('input');
    all.type = 'checkbox';
    all.checked = items.every((i) => i.on);
    all.indeterminate = !all.checked && items.some((i) => i.on);
    all.ariaLabel = `Apply all: ${label}`;
    all.onclick = (e) => e.stopPropagation(); // don't toggle the <details>
    all.onchange = () => { items.forEach((i) => { i.on = all.checked; }); renderCleanup(); };
    const n = document.createElement('span');
    n.className = 'n';
    n.textContent = ` ${items.length} keyword${items.length === 1 ? '' : 's'}, ${items.reduce((s, i) => s + i.count, 0)} uses`;
    summary.append(all, label, n);
    const ul = document.createElement('ul');
    // Big groups render their rows only when opened.
    const fill = () => ul.replaceChildren(...items.map((i) => {
      const li = document.createElement('li');
      const label = document.createElement('label');
      const box = document.createElement('input');
      box.type = 'checkbox';
      box.checked = i.on;
      box.onchange = () => { i.on = box.checked; updateApply(); syncGroup(); };
      const count = document.createElement('span');
      count.className = 'n';
      count.textContent = ` (${i.count})`;
      label.append(box, i.into ? `${i.keyword} → ${i.into}` : i.keyword, count);
      li.append(label);
      return li;
    }));
    const syncGroup = () => {
      all.checked = items.every((i) => i.on);
      all.indeterminate = !all.checked && items.some((i) => i.on);
    };
    details.ontoggle = () => {
      details.open ? openGroups.add(reason) : openGroups.delete(reason);
      if (details.open && !ul.childElementCount) fill();
    };
    details.append(summary, ul);
    if (openGroups.has(reason)) { details.open = true; fill(); }
    return details;
  }).filter(Boolean);
  $('cleanup-groups').replaceChildren(...(groups.length ? groups : [Object.assign(document.createElement('p'), { textContent: 'Nothing to clean up.' })]));
  updateApply();
}

function updateApply() {
  const n = cleanupItems.filter((i) => i.on).length;
  $('cleanup-apply').textContent = `Apply ${n} change${n === 1 ? '' : 's'}`;
  $('cleanup-apply').disabled = n === 0;
}

$('cleanup-apply').addEventListener('click', busyAware(async () => {
  const changes = cleanupItems.filter((i) => i.on).map((i) => [i.keyword, i.into ?? '']);
  $('cleanup-apply').disabled = true;
  try {
    const [n, backup] = await invoke('apply_cleanup', { changes });
    $('cleanup').close();
    await refreshDictionary();
    await refresh();
    say(`Cleaned up ${n} keywords. Backup: ${backup}`);
  } catch (e) {
    $('cleanup-apply').disabled = false;
    fail('Cleanup')(e);
  }
}));

// ---- properties panel -------------------------------------------------------

function renderPanel() {
  const one = selected.size === 1 ? cache.get([...selected][0]) : null;
  if (dirty && editing && one?.id === editing.id) return; // keep unsaved edits on screen
  dirty = false;
  editing = one ? { ...one } : null;
  if (selected.size > 1) {
    const p = document.createElement('p');
    p.textContent = `${selected.size} bookmarks selected.`;
    const del = document.createElement('button');
    del.className = 'danger';
    del.textContent = `Delete ${selected.size}`;
    del.onclick = () => remove([...selected]);
    panel.replaceChildren(p, del);
    return;
  }
  if (!editing) {
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = ids.length ? 'Select a bookmark to edit it.' : '';
    panel.replaceChildren(p);
    return;
  }
  panel.replaceChildren($('edit-form').content.cloneNode(true));
  const form = $('form');
  const rating = $('f-rating');
  rating.append(...Array.from({ length: 11 }, (_, n) => new Option(n ? `${n} / 10` : 'Not rated', n)));
  for (const name of ['title', 'url', 'description', 'notes', 'rating']) {
    form.elements[name].value = editing[name] ?? '';
  }
  chips = words(editing.keywords);
  renderChips();
  const date = (t) => (t ? new Date(t * 1000).toLocaleString() : 'never');
  $('info').textContent =
    `Added ${date(editing.added)} · Visited ${editing.visits} time${editing.visits === 1 ? '' : 's'} · Last visited ${date(editing.last_visited)}`;
  form.oninput = (e) => { if (e.target.id !== 'f-kw' && e.target.id !== 'f-check') setDirty(true); };
  form.onsubmit = (e) => { e.preventDefault(); save(); };
  $('revert').onclick = () => { dirty = false; renderPanel(); };
  $('del-one').onclick = () => remove([editing.id]);
  $('f-check').checked = editing.check_enabled;
  $('f-check-status').textContent = editing.check_status
    ? `· last result: ${checkLabel(editing)}${editing.last_checked ? `, ${new Date(editing.last_checked * 1000).toLocaleDateString()}` : ''}`
    : '· never checked';
  $('f-check').onchange = (e) => setCheckEnabled([editing.id], e.target.checked);
  $('pin-link').onclick = () => openSavedDialog({ id: 0, kind: 'link', name: editing.title || editing.url, query: '', options: '{}', url: editing.url });
  $('kw-sort').onclick = () => { chips.sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' })); renderChips(); setDirty(true); };
  $('f-tags').onclick = (e) => { if (e.target.id === 'f-tags') $('f-kw').focus(); };
  const input = $('f-kw');
  input.onkeydown = (e) => {
    if ((e.key === 'Enter' || e.key === ' ' || e.key === ',' || e.key === 'Tab') && input.value.trim()) {
      if (e.key !== 'Tab') e.preventDefault();
      commitChip();
    } else if (e.key === 'Enter') {
      e.preventDefault(); // empty input: don't submit the form by accident
    } else if (e.key === 'Backspace' && !input.value && chips.length) {
      chips.pop();
      renderChips();
      setDirty(true);
    }
  };
  // Picking from the autocomplete list fires 'input' with the full keyword.
  input.oninput = (e) => {
    if (e.inputType === 'insertReplacementText' || e.inputType === undefined) commitChip();
  };
  input.onblur = () => { if (input.value.trim()) commitChip(); };
}

function commitChip() {
  const input = $('f-kw');
  for (const w of words(input.value)) {
    if (!chips.some((c) => lower(c) === lower(w))) chips.push(w);
  }
  input.value = '';
  renderChips();
  setDirty(true);
}

function renderChips() {
  const box = $('f-tags');
  const tagged = new Set(dictionary.filter((k) => k.tagged).map((k) => lower(k.keyword)));
  box.replaceChildren(...chips.map((c, i) => {
    const chip = document.createElement('span');
    chip.className = 'chip';
    chip.textContent = (tagged.has(lower(c)) ? '🏷 ' : '') + c;
    const x = document.createElement('button');
    x.type = 'button';
    x.textContent = '×';
    x.ariaLabel = `Remove keyword ${c}`;
    x.onclick = () => { chips.splice(i, 1); renderChips(); setDirty(true); $('f-kw').focus(); };
    chip.append(x);
    return chip;
  }), $('f-kw'));
}

function setDirty(d) {
  dirty = d;
  $('save').disabled = !d;
  $('revert').disabled = !d;
}

async function save() {
  const form = $('form');
  if (!form) return;
  if ($('f-kw').value.trim()) commitChip();
  if (!dirty || !form.reportValidity()) return;
  const bookmark = { ...editing, keywords: chips.join(' ') };
  for (const name of ['title', 'url', 'description', 'notes']) {
    bookmark[name] = form.elements[name].value.trim();
  }
  bookmark.rating = Number(form.elements.rating.value);
  try {
    if (!(await invoke('update_bookmark', { bookmark }))) throw 'bookmark no longer exists';
    dirty = false;
    await refreshDictionary();
    await refresh();
    say('Saved.');
  } catch (e) { fail('Save')(e); }
}

function confirmDiscard() {
  if (!dirty) return true;
  if (!confirm('Discard unsaved changes?')) return false;
  dirty = false;
  return true;
}

// ---- wiring -----------------------------------------------------------------

// ---- search options -----------------------------------------------------------

function applyOptsToUi() {
  $('mode').value = opts.mode;
  $('opt-partial').checked = opts.partial;
  $('opt-partial').disabled = opts.mode === 'exact';
  $('exact-opts').disabled = opts.mode !== 'exact';
  $('opt-case').checked = opts.match_case;
  document.querySelectorAll('input[name=field]').forEach((b) => { b.checked = opts.fields.includes(b.value); });
  q.placeholder = opts.mode === 'exact' ? 'Type exact text (e.g. .edu/)…' : opts.mode === 'keywords' ? 'Type keywords (keywords field only)…' : 'Type keywords…';
}

function saveOpts() {
  try { localStorage.setItem('searchOptions', JSON.stringify(opts)); } catch {}
}

function optsChanged() {
  opts = {
    ...opts,
    mode: $('mode').value,
    partial: $('opt-partial').checked,
    fields: [...document.querySelectorAll('input[name=field]:checked')].map((b) => b.value),
    match_case: $('opt-case').checked,
  };
  saveOpts();
  applyOptsToUi();
  refresh({ keepSelection: false });
}

$('mode').addEventListener('change', optsChanged);
$('opts').addEventListener('change', optsChanged);
document.addEventListener('click', (e) => {
  for (const id of ['opts', 'cols', 'more']) if (!$(id).contains(e.target)) $(id).open = false;
  if (e.target.closest('#more .menu button')) $('more').open = false;
});
applyOptsToUi();

q.addEventListener('input', () => { if (confirmDiscard()) refresh({ keepSelection: false }); });
q.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && q.value) { e.preventDefault(); setSearch(''); return; }
  onListKeys(e);
});
list.addEventListener('keydown', onListKeys);
tablewrap.addEventListener('keydown', onListKeys);
document.addEventListener('keydown', (e) => {
  if (e.key === 's' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); save(); }
});

// ---- adding ----------------------------------------------------------------------

/** Show one bookmark: clear the search if needed, then select and scroll to it. Whichever
 *  refresh first lists it does the selecting (a background fetch may refresh concurrently). */
let pendingReveal = null;
async function reveal(id) {
  pendingReveal = id;
  if (!ids.includes(id)) q.value = '';
  await refresh({ keepSelection: false });
}

function newBookmark() {
  if (!confirmDiscard()) return;
  $('n-url').value = '';
  $('n-title').value = '';
  $('n-error').textContent = '';
  $('new-dlg').showModal();
  $('n-url').focus();
}

async function addNewBookmark() {
  $('n-error').textContent = '';
  try {
    const report = await invoke('add_bookmark', { url: $('n-url').value, title: $('n-title').value });
    $('new-dlg').close();
    await refreshDictionary();
    if (report.added) {
      await reveal(report.ids[0]);
      say($('n-title').value.trim() ? 'Added.' : 'Added. Fetching its title…');
    } else {
      say('That URL is already bookmarked.');
    }
  } catch (e) { $('n-error').textContent = String(e); }
}

$('new-form').addEventListener('submit', (e) => {
  e.preventDefault();
  addNewBookmark();
});
$('n-cancel').onclick = () => $('new-dlg').close();
// Cancel buttons are type=button (so Enter never means Cancel) and close their dialog.
document.addEventListener('click', (e) => { if (e.target.matches('dialog [data-close]')) e.target.closest('dialog').close(); });

async function addFromClipboard() {
  if (!confirmDiscard()) return;
  try {
    const r = await invoke('add_from_clipboard');
    await refreshDictionary();
    await refresh();
    const dup = r.duplicates ? `, ${r.duplicates} already here` : '';
    say(`Added ${r.added} link${r.added === 1 ? '' : 's'} from the clipboard${dup}. Fetching titles… (Ctrl+Z undoes)`);
  } catch (e) { fail('Add from clipboard')(e); }
}

$('new-bookmark').onclick = () => newBookmark();
$('add-clipboard').onclick = () => addFromClipboard();
// Powermarks' keys: Insert adds a bookmark, Alt+Insert adds links from the clipboard.
// (Ctrl+N works too where WebView2 doesn't keep it for itself.)
document.addEventListener('keydown', (e) => {
  const typing = e.target.closest('input, textarea, select');
  if (e.key === 'Insert' && !e.ctrlKey && !e.altKey && !e.shiftKey && !typing) { e.preventDefault(); newBookmark(); }
  else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'n') { e.preventDefault(); newBookmark(); }
  else if (e.altKey && e.key === 'Insert') { e.preventDefault(); addFromClipboard(); }
});
// Background fetches (titles for new bookmarks) finished.
window.__TAURI__.event.listen('bookmarks-changed', async () => {
  if (dirty) return;
  await refreshDictionary();
  await refresh();
});

// ---- export --------------------------------------------------------------------

$('export-open').onclick = () => {
  const n = (k) => k.toLocaleString();
  $('ex-all').textContent = `All ${n(total)} bookmarks`;
  $('ex-matching').textContent = `The ${n(ids.length)} matching the current search`;
  $('ex-selected').textContent = `The ${n(selected.size)} selected`;
  document.querySelector('input[name=ex-scope][value=selected]').disabled = selected.size === 0;
  document.querySelector('input[name=ex-scope][value=all]').checked = true;
  $('export-dlg').showModal();
};

$('ex-go').onclick = busyAware(async () => {
  const format = document.querySelector('input[name=ex-format]:checked').value;
  const scope = document.querySelector('input[name=ex-scope]:checked').value;
  const chosen = scope === 'all' ? null : scope === 'matching' ? ids : [...selected];
  try {
    const done = await invoke('export_file', { format, ids: chosen });
    if (!done) return; // save dialog cancelled
    $('export-dlg').close();
    say(`Exported ${done[0].toLocaleString()} bookmark${done[0] === 1 ? '' : 's'} to ${done[1]}.`);
  } catch (e) { fail('Export')(e); }
});

// ---- import --------------------------------------------------------------------

function importDone(report, from) {
  if (!report) return; // file dialog cancelled
  const dup = report.duplicates ? `, skipped ${report.duplicates} already here` : '';
  say(`Imported ${report.added} bookmark${report.added === 1 ? '' : 's'} from ${from}${dup}. Ctrl+Z undoes it.`);
}

async function afterImport(report, from) {
  $('import-dlg').close();
  await refreshDictionary();
  await refresh({ keepSelection: false });
  importDone(report, from);
}

importBtn.onclick = busyAware(async () => {
  if (!confirmDiscard()) return;
  $('import-sources').replaceChildren(Object.assign(document.createElement('p'), { textContent: 'Looking for browsers…' }));
  $('import-dlg').showModal();
  try {
    const sources = await invoke('browser_sources');
    if (!sources.length) {
      $('import-sources').replaceChildren(Object.assign(document.createElement('p'), { textContent: 'No Chrome, Edge or Firefox profiles found. You can still import an exported bookmarks file.' }));
      return;
    }
    const table = document.createElement('table');
    for (const src of sources) {
      const tr = table.insertRow();
      tr.insertCell().textContent = `${src.browser} · ${src.profile}`;
      Object.assign(tr.insertCell(), { className: 'num', textContent: src.count === null ? "can't read" : `${src.count.toLocaleString()} bookmarks` });
      const btn = Object.assign(document.createElement('button'), { type: 'button', className: 'secondary', textContent: 'Import', disabled: !src.count });
      btn.onclick = busyAware(async () => {
        try { await afterImport(await invoke('import_browser', { browser: src.browser, profile: src.profile }), `${src.browser}`); }
        catch (e) { fail('Import')(e); }
      });
      tr.insertCell().append(btn);
    }
    $('import-sources').replaceChildren(table);
  } catch (e) { fail('Import')(e); }
});

$('import-html').onclick = busyAware(async () => {
  try {
    const report = await invoke('import_html_file');
    if (report) await afterImport(report, 'the file');
  } catch (e) { fail('Import')(e); }
});

$('import-pm35').onclick = busyAware(async () => {
  try {
    const n = await invoke('import_pm35');
    if (n !== null) await afterImport({ added: n, duplicates: 0 }, 'Powermarks');
  } catch (e) { fail('Import')(e); }
});

// Every user action counts as busy until its follow-up refresh and message are done.
/* eslint-disable no-func-assign */
select = busyAware(select);
open = busyAware(open);
remove = busyAware(remove);
save = busyAware(save);
runHistory = busyAware(runHistory);
restoreBackup = busyAware(restoreBackup);
keywordChanged = busyAware(keywordChanged);
/* eslint-enable no-func-assign */

setCheckEnabled = busyAware(setCheckEnabled);
addNewBookmark = busyAware(addNewBookmark);
runSaved = busyAware(runSaved);
submitSaved = busyAware(submitSaved);
addFromClipboard = busyAware(addFromClipboard);

setView(view);
refreshSaved().catch(fail('Saved items'));
refreshDictionary().then(() => refresh({ keepSelection: false }));
