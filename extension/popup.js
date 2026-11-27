const $ = (id) => document.getElementById(id);
const show = (text) => { $('status').textContent = text; };
let url = '';
let existing = false;

(async () => {
  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  url = tab?.url ?? '';
  $('url').textContent = url;
  if (!/^https?:\/\//.test(url)) {
    $('add').disabled = true;
    $('title').value = tab?.title ?? '';
    show('Only web pages can be added.');
    return;
  }
  // Already bookmarked? Then show and edit that bookmark instead of making a duplicate.
  const found = await api.runtime.sendNativeMessage(HOST, { type: 'lookup', url }).catch(() => null);
  if (found?.found) {
    existing = true;
    $('title').value = found.title;
    $('keywords').value = found.keywords;
    $('desc').value = found.description;
    $('add').textContent = 'Update in Keymarks';
    $('folder').hidden = $('folder-label').hidden = true; // an update doesn't move it
    show(found.folder ? `Already in Keymarks, in ${found.folder}: edit it here.` : 'Already in Keymarks: edit it here.');
  } else {
    $('title').value = tab?.title ?? '';
    const meta = await readPage(tab.id);
    $('desc').value = meta.description;
    $('keywords').value = meta.keywords;
    await fillFolders();
  }
  $('keywords').focus();
})();

$('form').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('add').disabled = true;
  try {
    const reply = await api.runtime.sendNativeMessage(HOST, {
      type: 'add',
      url,
      title: $('title').value,
      keywords: $('keywords').value,
      description: $('desc').value,
      update: existing,
      ...($('folder').value ? { folder: Number($('folder').value) } : {}), // none: the New Item Folder
    });
    if (!reply?.ok) throw new Error(reply?.error ?? 'no reply');
    show(reply.updated ? 'Updated ✓' : reply.existed ? 'Already in Keymarks.' : 'Added ✓');
    setTimeout(() => window.close(), 700);
  } catch (err) {
    show(`Couldn't reach Keymarks (${err.message}). Run keymarks-host --install.`);
    $('add').disabled = false;
  }
});

// The folder picker defaults to the New Item Folder (ITEM-01); until one exists, the first
// add makes it, so it is offered by name with an empty value.
async function fillFolders() {
  const r = await api.runtime.sendNativeMessage(HOST, { type: 'folders' }).catch(() => null);
  const pick = $('folder');
  if (!r?.ok) { pick.hidden = $('folder-label').hidden = true; return; }
  const opts = r.folders.map((f) => new Option(f.id === r.new_items ? `${f.path} (New Item Folder)` : f.path, String(f.id)));
  if (r.new_items == null) opts.splice(1, 0, new Option('New (New Item Folder)', ''));
  pick.replaceChildren(...opts);
  pick.value = r.new_items == null ? '' : String(r.new_items);
}

$('open').addEventListener('click', async () => {
  const problem = await openApp();
  if (problem) show(problem); else window.close();
});
