'use strict';

const PREFIX = 'data:text/html;charset=utf-8,';
const escape = value => String(value).replace(/[&<>"']/g, character => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[character]);
function documentBody(url) {
  const value = url.split('#', 1)[0];
  if (!value.startsWith(PREFIX)) return null;
  try { return decodeURIComponent(value.slice(PREFIX.length)); } catch { return null; }
}
function sameDocument(error, url) { return error.pageUrl === url || documentBody(url) === error.html; }
function managedErrorForURL(target, url) {
  if (typeof url !== 'string') return null;
  if (target.loadError && sameDocument(target.loadError, url)) return target.loadError;
  const direct = target.errorPages.get(url);
  if (direct || !url.startsWith(PREFIX)) return direct || null;
  for (const error of target.errorPages.values()) if (sameDocument(error, url)) return error;
  return null;
}
function displayPage(target) {
  const url = target.contents.getURL();
  const error = managedErrorForURL(target, url) || target.loadError;
  return error ? { url: error.failedUrl, title: error.title } : { url, title: target.contents.getTitle() };
}
function mapHistory(target, history) {
  return { ...history, entries: history.entries.map(entry => {
    const error = managedErrorForURL(target, entry.url);
    return error ? { ...entry, url: error.failedUrl, title: error.title } : entry;
  }) };
}
function createErrorDocument({ kind, failedUrl, errorCode, errorDescription }) {
  const host = new URL(failedUrl).hostname || failedUrl;
  const title = kind === 'crash' ? 'This page crashed' : "This site can't be reached";
  const summary = kind === 'crash' ? `${host} crashed unexpectedly` : `${host} could not be loaded`;
  const code = String(errorDescription || `ERR_FAILED (${errorCode})`).trim().toUpperCase();
  const literal = JSON.stringify(failedUrl).replace(/[<>&\u2028\u2029]/g, value => `\\u${value.charCodeAt(0).toString(16).padStart(4, '0')}`);
  const html = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta name="color-scheme" content="light dark"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${escape(title)}</title>
<style>
html{-webkit-text-size-adjust:100%;font-size:125%}body{margin:0;background:#fff;color:rgb(95,99,104);font:70% system-ui,sans-serif;overflow-wrap:break-word}main{box-sizing:border-box;line-height:1.6em;margin:20vh auto 0;max-width:600px;width:100%}h1{color:rgb(32,33,36);font-size:1.2em;font-weight:500;line-height:1.2em;margin:0 0 12px}p{margin:0}nav{display:flex;gap:8px;margin-top:51px;justify-content:${kind === 'crash' ? 'flex-end' : 'flex-start'}}button{display:inline-flex;align-items:center;justify-content:center;background:rgb(26 28 31/5%);border:1px solid transparent;border-radius:8px;box-sizing:border-box;color:#1a1c1f;cursor:pointer;font:500 13px/18px system-ui;min-height:28px;padding:0 8px}button:hover,button:active{background:rgb(26 28 31/10%)}.code{font-size:.8em;margin-top:24px;text-transform:uppercase}details{margin-top:18px}summary{cursor:pointer}@media(prefers-color-scheme:dark){body{background:rgb(32,33,36);color:rgb(154,160,166)}h1{color:rgb(154,160,166)}button{background:rgb(255 255 255/5%);color:#fff}button:hover,button:active{background:rgb(255 255 255/10%)}}@media(max-width:700px){main{padding:0 10%}}@media(max-width:420px){main{margin:7vh auto 12px;padding:0 24px}nav{margin-top:30px}h1{font-size:1.5em;margin-bottom:8px}}
</style></head><body><main><h1>${escape(title)}</h1><p>${escape(summary)}</p>${kind === 'crash' ? '' : `<details><summary>Checking the connection, proxy, firewall, and DNS configuration</summary><p>Check that your internet connection is available and that this address can be reached.</p></details><div class="code">${escape(code)}</div>`}<nav><button id="reload" type="button">Reload</button></nav></main><script>document.getElementById('reload').addEventListener('click',()=>{location.href=${literal}});</script></body></html>`;
  return Object.freeze({ kind, failedUrl, title, errorCode, errorDescription: code, html, pageUrl: PREFIX + encodeURIComponent(html) });
}
function ignoredLoadFailure(code, description, mainFrame) { return !mainFrame || code === -3 || ['ERR_ABORTED', 'ERR_BLOCKED_BY_CLIENT'].includes(String(description).trim().toUpperCase()); }
function aborted(error) { return /ERR_ABORTED|\(-3\) loading '|Navigation failed with error code -3\./.test(String(error?.message)); }
module.exports = { managedErrorForURL, displayPage, mapHistory, createErrorDocument, ignoredLoadFailure, aborted };
