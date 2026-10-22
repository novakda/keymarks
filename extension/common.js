// Shared by the popup and the background script.
const api = globalThis.browser ?? chrome;
const HOST = 'com.pattern158.keymarks';

// Like Powermarks' Fetch: description and keywords from the page's meta tags.
// Browser-internal and store pages can't be scripted; then both are empty.
async function readPage(tabId) {
  try {
    const [{ result }] = await api.scripting.executeScript({
      target: { tabId },
      func: () => {
        const meta = (sel) => document.querySelector(sel)?.content ?? '';
        return {
          description: meta('meta[name="description" i]') || meta('meta[property="og:description"]'),
          keywords: meta('meta[name="keywords" i]'),
        };
      },
    });
    return {
      description: result.description.trim(),
      keywords: result.keywords.split(',').map((k) => k.trim()).filter(Boolean).join(' '),
    };
  } catch {
    return { description: '', keywords: '' };
  }
}
