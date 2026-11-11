(origin, fileSelector, expectedCount, action, title, description) => {
  const state = (status, details = {}) => ['input', 'tabPoint'].includes(action) ? null : ({ status, ...details });
  if (location.origin !== origin || !document.body) return state('wrongPage');

  const path = location.pathname.replace(/\/$/, '');
  const query = new URLSearchParams(location.search);
  const upload = path === '/creator-micro/content/upload';
  const composer = [
    '/creator-micro/content/post/image', '/creator-micro/content/publish-media/image-text',
  ].includes(path) && query.get('type') === 'new' && query.get('media_type') === 'image' &&
    !['iid', 'aweme_id', 'item_id'].some((key) => query.has(key));

  const visible = (element) => !!element && element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const enabled = (element) => !element.closest(':disabled, [disabled], [readonly], [aria-disabled="true"]');
  const present = (selector, root = document) => [...root.querySelectorAll(selector)].filter(visible);
  const alerts = present('[role="alert"], [aria-live="assertive"], .semi-toast, .dy-creator-content-toast');
  if (present('img[alt*="扫码登录"], input[placeholder*="请输入手机号"]').length ||
      present('dialog, [role="dialog"], [aria-modal="true"]').some((dialog) =>
        /扫码登录|请先登录|请重新登录/.test(dialog.innerText))) return state('needsLogin');
  if (!upload && !composer) return state('wrongPage');

  const failure = /上传失败|上传错误|不支持的文件格式|存在类型不匹配的文件|图片(?:最多|尺寸最大限制)|图片压缩失败/;
  if (alerts.some((alert) => failure.test(alert.innerText)) ||
      present('[class*="upload-error-"], [class*="upload-opacity-"]').length) return state('failed');

  if (upload) {
    const inputs = [...document.querySelectorAll(fileSelector)].filter((input) =>
      enabled(input) && visible(input.closest('[class*="container-drag-"]')));
    const input = inputs.length === 1 ? inputs[0] : null;
    if (action === 'input') return input;
    if (action === 'tabPoint') {
      if (inputs.length) return null;
      const tabs = present('[class*="tab-container-"] > [class*="tab-item-"], [role="tab"]').filter(
        (tab) => enabled(tab) && tab.innerText.trim() === '发布图文'
      );
      if (tabs.length !== 1) return null;
      const [tab] = tabs;
      tab.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      const bounds = tab.getBoundingClientRect();
      const point = [bounds.left + bounds.width / 2, bounds.top + bounds.height / 2];
      return tab.contains(document.elementFromPoint(...point)) ? point : null;
    }
    return state(input ? 'selectFiles' : 'waiting');
  }
  if (action === 'input' || action === 'tabPoint') return null;

  // Both photo composers share the same image grid. The count includes
  // unfinished uploads, so require each card's success controls and preview.
  const counters = present('[class*="info-"]').filter((element) =>
    /^已添加\s*\d+\s*张图片$/.test(element.innerText.trim()));
  if (counters.length !== 1) return state('uploading');
  const [counter] = counters;
  const count = Number(counter.innerText.match(/\d+/)[0]);
  if (count !== expectedCount) return state('countMismatch');
  const grid = counter.previousElementSibling;
  if (!visible(grid)) return state('uploading');
  const cards = [...grid.children].filter((card) =>
    visible(card) && present(':scope > [class*="img-"]', card).length === 1);
  if (cards.length !== expectedCount || cards.some((card) =>
    present('[class*="upload-"], [role="progressbar"]', card).length ||
    !card.querySelector(':scope > [class*="operator-button-"]'))) return state('uploading');

  const picture = (element) => {
    const img = element.querySelector('img');
    const background = getComputedStyle(element).backgroundImage.match(/^url\("(.+)"\)$/);
    const source = img?.currentSrc || background?.[1];
    if (!source) return null;
    const url = new URL(source, location.href);
    if (url.protocol === 'blob:' && url.origin === origin) return url.href;
    return ['https:', 'http:'].includes(url.protocol) ? url.origin + url.pathname : null;
  };
  const sources = cards.map((card) => picture(present(':scope > [class*="img-"]', card)[0]));
  const covers = present('[class*="mycard-"] > [class*="cover-"]');
  const cover = covers.length === 1 ? picture(covers[0]) : null;
  if (sources.some((source) => !source) || !cover) return state('processing');

  const titles = present('input[placeholder*="添加作品标题"], input[placeholder*="填写作品标题"]').filter(enabled);
  const descriptions = present('div.zone-container[contenteditable="true"], [data-zone-container][contenteditable="true"]')
    .filter(enabled);
  if (titles.length !== 1 || descriptions.length !== 1) return state('editorMissing');
  const [titleField] = titles;
  const [editor] = descriptions;
  const normalize = (text) => text.replace(/\r\n?/g, '\n').replace(/\u00a0/g, ' ').trim();
  if (action === 'fill') {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(titleField, title);
    titleField.dispatchEvent(new Event('input', { bubbles: true }));
    titleField.dispatchEvent(new Event('change', { bubbles: true }));
    if (normalize(editor.innerText) !== normalize(description)) {
      editor.focus();
      const range = document.createRange();
      range.selectNodeContents(editor);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.execCommand(description ? 'insertText' : 'delete', false, description);
    }
    editor.blur();
    if (titleField.value !== title || normalize(editor.innerText) !== normalize(description)) {
      return state('contentChanged');
    }
  }
  return state('ready', {
    photos: { sources, cover },
    title: titleField.value,
    description: normalize(editor.innerText),
  });
}
