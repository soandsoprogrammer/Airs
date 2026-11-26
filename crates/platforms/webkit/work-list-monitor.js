// Observe the normal, read-only work-list request issued by the official page.
// Never replay a request, collect credentials, or retain the full response.
(criteria, matchWorks) => {
  if (location.origin !== 'https://creator.douyin.com' ||
      location.pathname.replace(/\/$/, '') !== '/creator-micro/content/manage') return;
  let matches = null;
  const endpoint = url => {
    try {
      const value = new URL(url, location.href);
      return value.origin === location.origin &&
        value.pathname.replace(/\/$/, '') === '/janus/douyin/creator/pc/work_list';
    } catch (_) { return false; }
  };
  const limit = 6 * 1024 * 1024;
  const record = value => {
    const result = matchWorks(criteria, value);
    if (result !== null) matches = result;
  };
  const read = async response => {
    if (!response.body) return;
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let total = 0, text = '';
    try {
      while (true) {
        const {value, done} = await reader.read();
        if (done) break;
        total += value.byteLength;
        if (total > limit) { await reader.cancel(); return; }
        text += decoder.decode(value, {stream:true});
      }
      record(JSON.parse(text + decoder.decode()));
    } catch (_) {
    } finally { reader.releaseLock(); }
  };
  Object.defineProperty(window, '__airsDouyinWorkList', {
    configurable: false, writable: false, value: () => matches
  });
  const fetchOriginal = window.fetch;
  window.fetch = function(input, init) {
    const expect = endpoint(input instanceof Request ? input.url : String(input));
    const response = fetchOriginal.apply(this, arguments);
    if (expect) response.then(value => {
      if (value.ok && endpoint(value.url)) read(value.clone()).catch(() => {});
    }, () => {});
    return response;
  };
  const xhrOpen = XMLHttpRequest.prototype.open;
  const xhrSend = XMLHttpRequest.prototype.send;
  const pending = new WeakMap();
  XMLHttpRequest.prototype.open = function(method, url) {
    const result = xhrOpen.apply(this, arguments);
    pending.set(this, endpoint(String(url)));
    return result;
  };
  XMLHttpRequest.prototype.send = function() {
    if (pending.get(this)) this.addEventListener('load', () => {
      if (this.status < 200 || this.status >= 300 || !endpoint(this.responseURL)) return;
      try {
        if (this.responseType === 'json') record(this.response);
        else if ((!this.responseType || this.responseType === 'text') && this.responseText.length <= limit)
          record(JSON.parse(this.responseText));
      } catch (_) {}
    }, {once:true});
    return xhrSend.apply(this, arguments);
  };
}
