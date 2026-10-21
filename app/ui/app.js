const { invoke } = window.__TAURI__.core;
const q = document.getElementById('q');
const list = document.getElementById('list');
const status = document.getElementById('status');
const importBtn = document.getElementById('import');

let rows = [];
let sel = 0;
let total = 0;
let seq = 0;

async function refresh() {
  const mine = ++seq;
  const [n, found] = await Promise.all([invoke('count'), invoke('search', { query: q.value })]);
  if (mine !== seq) return; // a newer search already rendered
  total = n;
  rows = found;
  sel = 0;
  importBtn.hidden = total > 0;
  render();
}

// Titles come from imported files: build nodes with textContent, never innerHTML.
function render() {
  list.replaceChildren(...rows.map((b, i) => {
    const li = document.createElement('li');
    li.role = 'option';
    li.ariaSelected = String(i === sel);
    if (i === sel) li.className = 'sel';
    const title = document.createElement('div');
    title.className = 'title';
    title.textContent = (b.rating > 0 ? '★'.repeat(Math.ceil(b.rating / 2)) + ' ' : '') + (b.title || b.url);
    const meta = document.createElement('div');
    meta.className = 'meta';
    meta.textContent = [b.keywords, b.url].filter(Boolean).join(' · ');
    li.append(title, meta);
    li.onclick = () => { sel = i; render(); };
    li.ondblclick = () => open(i);
    return li;
  }));
  list.children[sel]?.scrollIntoView({ block: 'nearest' });
  status.textContent = total === 0
    ? 'No bookmarks yet. Import your Powermarks database to get started.'
    : `${rows.length}${rows.length === 500 ? '+' : ''} shown · ${total} bookmarks · Enter opens · ↑↓ select`;
}

async function open(i) {
  if (!rows[i]) return;
  try { await invoke('open_url', { url: rows[i].url }); }
  catch (e) { status.textContent = String(e); }
}

q.addEventListener('input', refresh);
q.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown') sel = Math.min(sel + 1, rows.length - 1);
  else if (e.key === 'ArrowUp') sel = Math.max(sel - 1, 0);
  else if (e.key === 'Enter') return open(sel);
  else return;
  e.preventDefault();
  render();
});

importBtn.addEventListener('click', async () => {
  status.textContent = 'Importing…';
  try {
    const n = await invoke('import_pm35');
    if (n !== null) await refresh();
    else render();
  } catch (e) { status.textContent = 'Import failed: ' + e; }
});

refresh();
