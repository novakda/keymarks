const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);
const q = $('q');
const list = $('list');
const panel = $('panel');
const status = $('status');
const importBtn = $('import');
const LIMIT = 500;

let rows = [];
let total = 0;
let cursor = 0;            // index in rows of the focused item
let anchor = 0;            // start of a Shift range
let selected = new Set();  // bookmark ids
let editing = null;        // bookmark shown in the form
let dirty = false;
let seq = 0;
let version = null;

const say = (text) => { status.textContent = text; };
const fail = (what) => (e) => say(`${what} failed: ${e}`);

// ---- data -------------------------------------------------------------------

async function refresh({ keepSelection = true } = {}) {
  const mine = ++seq;
  const [n, found, v] = await Promise.all([
    invoke('count'), invoke('search', { query: q.value }), invoke('data_version'),
  ]);
  if (mine !== seq) return; // a newer search already rendered
  const cursorId = rows[cursor]?.id;
  total = n;
  rows = found;
  version = v;
  importBtn.hidden = total > 0;
  const ids = new Set(rows.map((b) => b.id));
  selected = keepSelection ? new Set([...selected].filter((id) => ids.has(id))) : new Set();
  cursor = Math.max(0, keepSelection ? rows.findIndex((b) => b.id === cursorId) : 0);
  if (selected.size === 0 && rows.length) selected.add(rows[cursor].id);
  render();
}

// Live refresh: the browser extension writes through another process.
setInterval(async () => {
  if (dirty || document.hidden) return;
  const v = await invoke('data_version').catch(() => version);
  if (v !== version) refresh();
}, 1500);

// ---- list -------------------------------------------------------------------

// Titles come from imported files and web pages: textContent only, never innerHTML.
function render() {
  list.replaceChildren(...rows.map((b, i) => {
    const li = document.createElement('li');
    li.role = 'option';
    li.ariaSelected = String(selected.has(b.id));
    li.classList.toggle('sel', selected.has(b.id));
    li.classList.toggle('cursor', i === cursor);
    const title = document.createElement('div');
    title.className = 'title';
    title.textContent = (b.rating > 0 ? '★'.repeat(Math.ceil(b.rating / 2)) + ' ' : '') + (b.title || b.url);
    const meta = document.createElement('div');
    meta.className = 'meta';
    meta.textContent = [b.keywords, b.url].filter(Boolean).join(' · ');
    li.append(title, meta);
    li.onclick = (e) => select(i, e);
    li.ondblclick = () => open(b.id);
    return li;
  }));
  list.children[cursor]?.scrollIntoView({ block: 'nearest' });
  renderPanel();
  say(total === 0
    ? 'No bookmarks yet. Import your Powermarks database to get started.'
    : `${rows.length}${rows.length === LIMIT ? '+' : ''} shown · ${total} bookmarks · ${selected.size} selected · Enter opens · Del deletes`);
}

function select(i, { ctrlKey = false, metaKey = false, shiftKey = false } = {}) {
  if (!rows[i] || !confirmDiscard()) return;
  const id = rows[i].id;
  if (shiftKey) {
    const [a, b] = [Math.min(anchor, i), Math.max(anchor, i)];
    if (!(ctrlKey || metaKey)) selected.clear();
    rows.slice(a, b + 1).forEach((r) => selected.add(r.id));
  } else if (ctrlKey || metaKey) {
    selected.has(id) ? selected.delete(id) : selected.add(id);
    anchor = i;
  } else {
    selected = new Set([id]);
    anchor = i;
  }
  cursor = i;
  render();
}

function onListKeys(e) {
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    const i = Math.max(0, Math.min(rows.length - 1, cursor + (e.key === 'ArrowDown' ? 1 : -1)));
    select(i, { shiftKey: e.shiftKey });
  } else if (e.key === 'Enter') {
    if (rows[cursor]) open(rows[cursor].id);
  } else if (e.key === 'a' && (e.ctrlKey || e.metaKey) && e.target === list) {
    rows.forEach((r) => selected.add(r.id));
    render();
  } else if (e.key === 'Delete' && e.target === list) {
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

async function remove(ids) {
  if (!ids.length) return;
  const one = rows.find((r) => r.id === ids[0]);
  const what = ids.length === 1 ? `"${one?.title || one?.url}"` : `${ids.length} bookmarks`;
  if (!confirm(`Delete ${what}? This can't be undone yet.`)) return;
  try {
    const n = await invoke('delete_bookmarks', { ids });
    dirty = false;
    selected.clear();
    await refresh();
    say(`Deleted ${n} bookmark${n === 1 ? '' : 's'}.`);
  } catch (e) { fail('Delete')(e); }
}

// ---- properties panel -------------------------------------------------------

function renderPanel() {
  const one = selected.size === 1 ? rows.find((r) => selected.has(r.id)) : null;
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
    p.textContent = rows.length ? 'Select a bookmark to edit it.' : '';
    panel.replaceChildren(p);
    return;
  }
  panel.replaceChildren($('edit-form').content.cloneNode(true));
  const form = $('form');
  const rating = $('f-rating');
  rating.append(...Array.from({ length: 11 }, (_, n) => new Option(n ? `${n} / 10` : 'Not rated', n)));
  for (const name of ['title', 'url', 'keywords', 'description', 'notes', 'rating']) {
    form.elements[name].value = editing[name] ?? '';
  }
  const date = (t) => (t ? new Date(t * 1000).toLocaleString() : 'never');
  $('info').textContent =
    `Added ${date(editing.added)} · Visited ${editing.visits} time${editing.visits === 1 ? '' : 's'} · Last visited ${date(editing.last_visited)}`;
  form.oninput = () => setDirty(true);
  form.onsubmit = (e) => { e.preventDefault(); save(); };
  $('revert').onclick = () => { dirty = false; renderPanel(); };
  $('del-one').onclick = () => remove([editing.id]);
}

function setDirty(d) {
  dirty = d;
  $('save').disabled = !d;
  $('revert').disabled = !d;
}

async function save() {
  const form = $('form');
  if (!dirty || !form.reportValidity()) return;
  const bookmark = { ...editing };
  for (const name of ['title', 'url', 'keywords', 'description', 'notes']) {
    bookmark[name] = form.elements[name].value.trim();
  }
  bookmark.rating = Number(form.elements.rating.value);
  try {
    if (!(await invoke('update_bookmark', { bookmark }))) throw 'bookmark no longer exists';
    dirty = false;
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

q.addEventListener('input', () => { if (confirmDiscard()) refresh({ keepSelection: false }); });
q.addEventListener('keydown', onListKeys);
list.addEventListener('keydown', onListKeys);
document.addEventListener('keydown', (e) => {
  if (e.key === 's' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); save(); }
});

importBtn.addEventListener('click', async () => {
  say('Importing…');
  try {
    const n = await invoke('import_pm35');
    if (n !== null) await refresh({ keepSelection: false });
    else render();
  } catch (e) { fail('Import')(e); }
});

refresh({ keepSelection: false });
