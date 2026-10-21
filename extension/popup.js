const api = globalThis.browser ?? chrome;
const $ = (id) => document.getElementById(id);
const HOST = 'com.xhiris.powermarks';
const show = (text) => { $('status').textContent = text; };
let url = '';

(async () => {
  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  url = tab?.url ?? '';
  $('url').textContent = url;
  $('title').value = tab?.title ?? '';
  if (!/^https?:\/\//.test(url)) {
    $('add').disabled = true;
    show('Only web pages can be added.');
    return;
  }
  // Like Powermarks' Fetch: prefill description and keywords from the page's meta tags.
  try {
    const [{ result }] = await api.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => {
        const meta = (sel) => document.querySelector(sel)?.content ?? '';
        return {
          description: meta('meta[name="description" i]') || meta('meta[property="og:description"]'),
          keywords: meta('meta[name="keywords" i]'),
        };
      },
    });
    $('desc').value = result.description.trim();
    $('keywords').value = result.keywords.split(',').map((k) => k.trim()).filter(Boolean).join(' ');
  } catch {
    // Browser-internal or store pages can't be scripted; leave the fields empty.
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
    });
    if (!reply?.ok) throw new Error(reply?.error ?? 'no reply');
    if (reply.existed) {
      show('Already in Powermarks.');
    } else {
      show('Added ✓');
      setTimeout(() => window.close(), 700);
    }
  } catch (err) {
    show(`Couldn't reach Powermarks (${err.message}). Run powermarks-host --install.`);
    $('add').disabled = false;
  }
});
