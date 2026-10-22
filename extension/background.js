// Fast add: a keyboard shortcut and right-click items that add without opening the popup.
// Chrome runs this as a service worker (needs importScripts); Firefox loads common.js first.
if (typeof importScripts === 'function') importScripts('common.js');

api.runtime.onInstalled.addListener(() => {
  api.contextMenus.create({ id: 'fast-add-page', title: 'Fast add page to Keymarks', contexts: ['page'] });
  api.contextMenus.create({ id: 'fast-add-link', title: 'Fast add link to Keymarks', contexts: ['link'] });
});

api.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === 'fast-add-link') {
    fastAdd(tab, { url: info.linkUrl, title: info.linkText || info.linkUrl });
  } else if (info.menuItemId === 'fast-add-page') {
    fastAdd(tab);
  }
});

api.commands.onCommand.addListener(async (command, tab) => {
  if (command !== 'fast-add') return;
  fastAdd(tab ?? (await api.tabs.query({ active: true, currentWindow: true }))[0]);
});

async function fastAdd(tab, link) {
  const url = link?.url ?? tab?.url ?? '';
  if (!/^https?:\/\//.test(url)) return flash(tab, '!', 'Only web pages can be added to Keymarks');
  const meta = link ? { description: '', keywords: '' } : await readPage(tab.id);
  try {
    const reply = await api.runtime.sendNativeMessage(HOST, {
      type: 'add', fast: true, url, title: link?.title ?? tab.title ?? url, ...meta,
    });
    if (!reply?.ok) throw new Error(reply?.error ?? 'no reply');
    flash(tab, reply.existed ? '=' : '✓', reply.existed ? 'Already in Keymarks' : 'Added to Keymarks');
  } catch (e) {
    flash(tab, '!', `Couldn't reach Keymarks (${e.message}). Run keymarks-host --install.`);
  }
}

// The badge is the confirmation (the original rang a bell).
function flash(tab, text, title) {
  const tabId = tab?.id;
  api.action.setBadgeText({ tabId, text });
  api.action.setTitle({ tabId, title });
  setTimeout(() => {
    api.action.setBadgeText({ tabId, text: '' });
    api.action.setTitle({ tabId, title: 'Add to Keymarks' });
  }, 3000);
}
