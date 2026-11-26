(() => {
  // Observe only explicitly armed POST operations. No URL rewriting,
  // request replay, header collection or cookie access.
  const announceFrame = () => {
    try {
      window.webkit.messageHandlers.airsFrame.postMessage(location.href);
    } catch (_) {}
  };
  announceFrame();
  document.addEventListener('DOMContentLoaded', announceFrame, {once:true});
  addEventListener('message', event => {
    const token = event.data?.__airsFrameProbe;
    if (event.source !== parent || typeof token !== 'string' || token.length !== 36) return;
    try { window.webkit.messageHandlers.airsFrame.postMessage({token}); } catch (_) {}
  });
  const roots = new WeakMap();
  const attachShadow = Element.prototype.attachShadow;
  Element.prototype.attachShadow = function (options) {
    const root = attachShadow.apply(this, arguments);
    if (this.localName === 'xhs-publish-btn') roots.set(this, root);
    return root;
  };
  Object.defineProperty(window, '__airsTrackedShadow', {
    configurable:false, writable:false, value: host => roots.get(host)
  });
  let filter = null;
  const post = (value) => {
    try { window.webkit.messageHandlers.airsReceipt.postMessage(value); } catch (_) {}
  };
  const match = (url, method) => {
    if (!filter || method.toUpperCase() !== 'POST') return null;
    try {
      const endpoint = new URL(url, location.href);
      const key = endpoint.origin + endpoint.pathname;
      return filter.endpoints.includes(key) ? { key, document: location.href, token: filter.token } : null;
    } catch (_) { return null; }
  };
  const bodyText = (value) => {
    const result = typeof value === 'string' ? value :
      value instanceof URLSearchParams ? value.toString() : null;
    return result !== null && result.length <= 262144 ? result : null;
  };
  const readBounded = async (response) => {
    if (!response.body) return null;
    const reader = response.body.getReader();
    let total = 0;
    const parts = [];
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        total += value.byteLength;
        if (total > 262144) { reader.cancel(); return null; }
        parts.push(value);
      }
      const decoder = new TextDecoder();
      return parts.map(part => decoder.decode(part, {stream:true})).join('') + decoder.decode();
    } finally {
      reader.releaseLock();
    }
  };
  Object.defineProperty(window, '__airsArmReceipts', {
    configurable: false,
    writable: false,
    value: (token, endpoints) => { filter = {token, endpoints}; return true; }
  });

  const originalFetch = window.fetch;
  window.fetch = function (input, init) {
    const url = input instanceof Request ? input.url : String(input);
    const observation = match(url, init?.method || (input instanceof Request ? input.method : 'GET'));
    let request = Promise.resolve(bodyText(init?.body));
    if (observation && input instanceof Request && !init?.body) {
      try { request = readBounded(input.clone()).catch(() => null); } catch (_) {}
    }
    const original = originalFetch.apply(this, arguments);
    if (observation) original.then(async response => {
      try {
        const resultUrl = new URL(response.url);
        if (response.redirected || resultUrl.origin + resultUrl.pathname !== observation.key ||
            response.status < 200 || response.status >= 300) return;
        const body = await readBounded(response.clone());
        if (body != null) post({...observation, status: response.status, request: await request, body});
      } catch (_) {}
    }, () => {});
    return original;
  };

  const originalOpen = XMLHttpRequest.prototype.open;
  const originalSend = XMLHttpRequest.prototype.send;
  const pending = new WeakMap();
  XMLHttpRequest.prototype.open = function (method, url) {
    const result = originalOpen.apply(this, arguments);
    pending.set(this, { method: String(method), url: String(url) });
    return result;
  };
  XMLHttpRequest.prototype.send = function (body) {
    const entry = pending.get(this);
    const observation = entry ? match(entry.url, entry.method) : null;
    if (observation) {
      const request = bodyText(body);
      this.addEventListener('loadend', () => {
        try {
          const resultUrl = new URL(this.responseURL);
          if (resultUrl.origin + resultUrl.pathname !== observation.key || this.status < 200 || this.status >= 300) return;
          const response = this.responseType === 'json' ? JSON.stringify(this.response) :
            (this.responseType === '' || this.responseType === 'text') ? this.responseText : null;
          if (response !== null && response.length <= 262144) {
            post({...observation, status:this.status, request, body: response});
          }
        } catch (_) {}
      }, { once:true });
    }
    return originalSend.apply(this, arguments);
  };
})()
