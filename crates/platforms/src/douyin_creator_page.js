(creatorOrigin, fileSelector, photoSelector) => {
  const empty = {
    authenticated: false, needsInteraction: false, nickname: null, uploadReady: false, photoUploadReady: false,
    canVerifyIdentity: false, qrStatus: null
  };
  if (location.origin !== creatorOrigin || !document.body) return empty;

  const path = location.pathname.replace(/\/$/, '');
  const home = path === '/creator-micro/home';
  const upload = path === '/creator-micro/content/upload';
  const creator = path === '/creator-micro' || path.startsWith('/creator-micro/');
  const text = document.body.innerText;

  const visible = (element) =>
    element?.getClientRects().length > 0 && getComputedStyle(element).visibility === 'visible';
  // These messages can replace the QR or overlay an image left in the DOM.
  if (/二维码(?:已)?(?:失效|过期)/.test(text)) {
    return { ...empty, needsInteraction: true, qrStatus: 'expired' };
  }
  if (/安全验证|身份验证|短信验证|手机验证|滑动滑块/.test(text)) {
    return { ...empty, needsInteraction: true, qrStatus: 'verifyInApp' };
  }
  if (/扫码成功|扫描成功|已扫码|手机端确认|在手机上确认|在抖音\s*App\s*中确认|请确认登录/.test(text)) {
    return { ...empty, needsInteraction: true, qrStatus: 'confirmOnPhone' };
  }
  if (/扫码失败|登录失败|登录异常|登录已取消|已取消登录/.test(text)) {
    return { ...empty, needsInteraction: true, qrStatus: 'failed' };
  }

  const blockers = 'input[placeholder*="验证码"], input[placeholder*="手机号"], input[autocomplete="one-time-code"], img[alt*="扫码登录"]';
  if ([...document.querySelectorAll(blockers)].some(visible) ||
      [...document.querySelectorAll('dialog, [role="dialog"], [aria-modal="true"]')].some(
        (element) => visible(element) && /请登录|扫码登录|登录验证|安全验证|手机验证/.test(element.innerText ?? '')
      )) return { ...empty, needsInteraction: true };

  // The sign-in screen may be reached via a redirect outside the creator routes.
  const loginScreen = /扫码登录|验证码登录|登录抖音创作者中心/.test(text);
  const unknown = { ...empty, needsInteraction: loginScreen, canVerifyIdentity: creator && !loginScreen };
  if (!home && !upload) {
    return unknown;
  }

  const validNickname = (element) => {
    const text = visible(element) ? (element.innerText ?? '').trim() : '';
    return text && text.length <= 128 && !/[\n\r|｜]/.test(text) &&
      !/^(抖音号|登录|扫码|关注\s*\d|粉丝\s*\d|获赞\s*\d)/.test(text) ? text : null;
  };
  const selected = [...document.querySelectorAll('[role="menuitemradio"][aria-checked="true"] [class*="account-name"]')]
    .map(validNickname).find(Boolean);
  const photoTab = upload && new URLSearchParams(location.search).get('default-tab') === '3';
  const photoUploadReady = upload && !!photoSelector &&
    [...document.querySelectorAll(photoSelector)].filter((input) =>
      !input.disabled && visible(input.closest('[class*="container-drag-"]'))).length === 1;
  const uploadReady = upload && !photoTab && !photoUploadReady && !!document.querySelector(fileSelector);
  if (selected || uploadReady || photoUploadReady) {
    return { ...empty, authenticated: true, nickname: selected ?? null, uploadReady, photoUploadReady };
  }
  if (!home) return unknown;

  // Login can redirect here instead of to the upload form. The displayed
  // account ID is positive evidence; the URL by itself is not.
  const idLabel = /^抖音号\s*[:：]\s*[a-z0-9_.-]{2,64}$/i;
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node; (node = walker.nextNode());) {
    if (!node.data.includes('抖音号')) continue;
    let field = node.parentElement;
    for (let depth = 0; field && depth < 3; depth++, field = field.parentElement) {
      if (!visible(field) || !idLabel.test((field.innerText ?? '').trim())) continue;
      let sibling = field.previousElementSibling;
      while (sibling && (!visible(sibling) || /^[\s|｜]*$/.test(sibling.innerText ?? ''))) {
        sibling = sibling.previousElementSibling;
      }
      return { ...empty, authenticated: true, nickname: validNickname(sibling) };
    }
  }
  return unknown;
}
