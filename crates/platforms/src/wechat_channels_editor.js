(origin, action, caption, shortTitle, snapshot) => {
  const result = (status) => ({ status });
  const path = (url) => url.pathname.replace(/\/$/, '');
  const editorPath = '/platform/post/create';
  if (location.origin !== origin) return result('wrongPage');
  const pagePath = path(location);
  const atHome = pagePath === '/platform';
  if (pagePath !== editorPath && !(action === 'open' && atHome)) return result('wrongPage');

  // Wujie can redirect getRootNode() to its hidden execution iframe.
  const rootOf = (node) => Node.prototype.getRootNode.call(node);
  const getOwner = Object.getOwnPropertyDescriptor(Node.prototype, 'ownerDocument').get;
  const owner = (node) => getOwner.call(node);
  const parent = (node) => node.parentElement ?? rootOf(node).host ??
    owner(node).defaultView?.frameElement;
  const visible = (element) => {
    if (!element.getClientRects().length ||
        element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) === false) return false;
    for (let node = element; node; node = parent(node)) {
      const style = owner(node).defaultView.getComputedStyle(node);
      if (node.matches('[hidden], [aria-hidden="true"], [inert]') ||
          style.display === 'none' || style.visibility !== 'visible' || style.opacity === '0') {
        return false;
      }
    }
    return true;
  };

  // The home card and the editor mount in separate Wujie apps.
  const appId = atHome ? 'postCard' : 'content';
  const appPaths = atHome ? ['/micro/content/iframe/post-card.html'] :
    ['/micro/content', '/micro/content/post/create', editorPath];
  const scopes = [document];
  for (let index = 0; index < scopes.length; index++) {
    const scope = scopes[index];
    for (const host of scope.querySelectorAll(`wujie-app[data-wujie-id="${appId}"]`)) {
      if (host.shadowRoot) scopes.push(host.shadowRoot);
    }
    for (const frame of scope.querySelectorAll('iframe')) {
      if (!visible(frame)) continue;
      try {
        const child = frame.contentDocument;
        const url = new URL(child?.location?.href ?? 'about:blank');
        const appFrame = url.origin === origin && appPaths.includes(path(url));
        const wujieFrame = frame.getAttribute('data-wujie-id') === appId &&
          (url.href === 'about:blank' || (url.origin === origin && path(url) === '/empty.html'));
        if (child && (appFrame || wujieFrame)) {
          scopes.push(child);
        }
      } catch {
        // Login, preview and unrelated frames are outside the editor scope.
      }
    }
  }
  const find = (scope, selector) => [...scope.querySelectorAll(selector)].filter(visible);
  const one = (items) => items.length === 1 ? items[0] : null;
  const label = (node) => (node.getAttribute('aria-label') || node.innerText || '').trim();
  const available = (element) => !element.closest(
    ':disabled, [disabled], [aria-disabled="true"], [aria-busy="true"], .weui-desktop-btn_disabled, .weui-desktop-btn_loading'
  ) && owner(element).defaultView.getComputedStyle(element).pointerEvents !== 'none';
  const buttons = (scope, name) => find(scope, 'button').filter((button) => label(button) === name);
  const point = (element) => {
    if (!element || !available(element)) return result('blocked');
    element.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' });
    const bounds = element.getBoundingClientRect();
    let x = bounds.left + bounds.width / 2;
    let y = bounds.top + bounds.height / 2;
    let target = element;
    let scope = rootOf(target);
    while (true) {
      const hit = scope.elementFromPoint?.(x, y);
      if (!hit || !target.contains(hit)) return result('blocked');
      if (scope === document) return { status: 'point', point: [x, y] };
      if (scope.host) {
        target = scope.host;
      } else {
        target = scope.defaultView?.frameElement;
        if (!target || !target.offsetWidth || !target.offsetHeight) return result('blocked');
        const frame = target.getBoundingClientRect();
        x = frame.left + (target.clientLeft + x) * frame.width / target.offsetWidth;
        y = frame.top + (target.clientTop + y) * frame.height / target.offsetHeight;
      }
      scope = rootOf(target);
    }
  };

  const dialogs = scopes.flatMap((scope) => find(scope,
    'dialog[open], [role="dialog"], [aria-modal="true"], .weui-desktop-dialog__wrp, .weui-desktop-dialog'
  ));
  const blocked = dialogs.length > 0;
  if (action === 'publish-followup') {
    const prompts = scopes.flatMap((scope) => find(scope, '.intercept-original-dialog'))
      .filter((dialog) => find(dialog, '.weui-desktop-dialog__title, [role="heading"], h2, h3')
        .some((heading) => label(heading) === '声明原创的视频有机会获得广告分成'));
    const prompt = one(prompts);
    if (!prompt) return result('waiting');
    // Only the explicit continue action on the originality prompt belongs
    // to the existing publish confirmation.
    return point(one(buttons(prompt, '直接发表')));
  }
  if (action === 'open' && atHome) {
    if (blocked) return result('waiting');
    const entries = scopes.flatMap((scope) => buttons(scope, '发表视频')).filter((button) =>
      button.classList.contains('weui-desktop-btn'));
    return entries.length === 1 && available(entries[0]) ? point(entries[0]) : result('waiting');
  }

  const editor = one(scopes.flatMap((scope) => find(scope, '.post-create-wrap'))
    .filter((root) => find(root, '.form-btns').length === 1));
  if (!editor) return action === 'input' ? null : result(action === 'open' ? 'editorLoading' : 'waiting');
  if (blocked) return action === 'input' ? null : result('blocked');
  if (find(editor, '.no-permission-content').length) return result('blocked');
  const description = one(find(editor, '.post-desc-box .input-editor[contenteditable]')
    .filter((field) => field.isContentEditable && !field.closest('.post-desc-box.disabled')));
  const shortTitles = find(editor,
    '.short-title-wrap input[placeholder="填写短标题有机会获得更多流量"]');
  const short = one(shortTitles);
  const normalize = (value) => value.replace(/\r\n?/g, '\n').replace(/\u00a0/g, ' ')
    .replace(/\u200b/g, '').trim();
  const captionValue = description ? normalize(description.innerText) : null;
  const uploading = find(editor, '.media-status-wrap');
  const completed = find(editor, '.post-media-preview-wrap .post-media-wrap');
  const uploadBlocked = find(editor, '.post-upload-wrap .disabled-mask').length > 0;
  const inputs = [...editor.querySelectorAll('.post-upload-wrap .upload-wrap input[type="file"][accept]')]
    .filter((input) => visible(input.closest('.upload-wrap')) &&
      !input.matches(':disabled, [disabled], [aria-disabled="true"]') && !input.files?.length &&
      input.accept.split(',').some((part) => /^video\/(?:\*|[a-z\d.+-]+)$/i.test(part.trim())));
  const input = one(inputs);
  const fresh = description && captionValue === '' && !short?.value && !uploading.length && !completed.length;
  if (action === 'input') return fresh && !uploadBlocked ? input : null;
  if (action === 'open') {
    if (uploadBlocked || inputs.length > 1 || shortTitles.length > 1) return result('blocked');
    if (description && !fresh) return result('contentChanged');
    return fresh && input ? result('inputReady') : result('editorLoading');
  }

  const alerts = scopes.flatMap((scope) => find(scope,
    '[role="alert"], [aria-live="assertive"], .weui-desktop-toast, .common-toptip:has(.tip-icon.error, .tip-icon.warn)'
  ));
  const uploadError = (text) =>
    /(?:上传|转码|处理)(?:视频|文件)?失败|网络出错，请重新上传|只支持上传视频|无法(?:读取|解析)视频|视频文件解析失败|不支持此视频格式|文件格式不支持|视频时长需|小时以内的视频|视频宽高比需|文件大小超过限制/.test(text);
  if (find(editor, '.media-status-content .status-msg.error').length ||
      alerts.some((alert) => uploadError(alert.innerText))) return result('uploadFailed');
  const progress = find(editor, '.media-status-content .upload-progress');
  if (progress.length) {
    const value = one(progress)?.querySelector('[role="progressbar"], .ant-progress-text');
    const raw = value?.getAttribute('aria-valuenow') ?? value?.innerText?.trim();
    const match = /^(\d+(?:\.\d+)?)\s*%?$/.exec(raw ?? '');
    const percent = match ? Math.floor(Number(match[1])) : null;
    return { status: 'uploading', percent: percent !== null && percent <= 100 ? percent : null };
  }
  if (find(editor, '.media-status-content .status-msg.default, .media-status-wrap .loading-mask')
    .length) return result('processing');
  const uploaded = one(completed);
  if (!uploaded || uploading.length) return result('waiting');
  if (find(uploaded, '.video-cannot-play-text').length) return result('previewUnavailable');
  const video = one(find(uploaded, '.media-full-screen-wrap .video-content > video#fullScreenVideo'));
  const media = video?.currentSrc || video?.src;
  if (!media || !/^(?:https:\/\/|blob:)/.test(media)) return result('waiting');
  if (action === 'observe') return result('editorReady');
  if (!description || shortTitles.length > 1) return result('blocked');

  const expectedCaption = normalize(caption);
  if (action === 'fill') {
    if ((captionValue && captionValue !== expectedCaption) ||
        (short?.value && short.value !== shortTitle)) return result('contentChanged');
    if (captionValue !== expectedCaption) {
      if (!available(description) || point(description).status !== 'point') return result('blocked');
      const doc = owner(description);
      const win = doc.defaultView;
      description.focus();
      const clipboard = new win.DataTransfer();
      clipboard.setData('text/plain', caption);
      const defaultPaste = description.dispatchEvent(new win.ClipboardEvent('paste', {
        clipboardData: clipboard, bubbles: true, cancelable: true, composed: true,
      }));
      if (defaultPaste) doc.execCommand('insertText', false, caption);
      description.blur();
    }
    if (short && shortTitle && short.value !== shortTitle) {
      if (!available(short) || short.readOnly || point(short).status !== 'point') return result('blocked');
      const win = owner(short).defaultView;
      short.focus();
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, 'value').set.call(short, shortTitle);
      short.dispatchEvent(new win.Event('input', { bubbles: true, composed: true }));
      short.blur();
    }
    return result('filled');
  }

  if (captionValue !== expectedCaption || (short && short.value !== shortTitle)) {
    return result('contentChanged');
  }
  const descriptionText = description.innerText.replace(/\r\n?/g, '\n');
  if (snapshot && (media !== snapshot.media || descriptionText !== snapshot.description ||
      (short?.value ?? null) !== snapshot.shortTitle)) {
    return result('mediaChanged');
  }
  const settings = [...editor.querySelectorAll('input[type="radio"]:checked, [role="radio"][aria-checked="true"]')]
    .map((node) => node.closest('label') ?? node).filter(visible);
  if (settings.some((node) => /^(?:定时|定时发表|私密|仅自己可见|仅我可见)$/
    .test(label(node)))) return result('settingsChanged');
  if (find(editor, '[aria-invalid="true"], .error-title, .length-error').length) {
    return result('blocked');
  }
  const footer = one(find(editor, '.form-btns'));
  const submit = footer && one(buttons(footer, '发表'));
  if (!submit || !available(submit)) return result('publishUnavailable');
  if (action === 'publish') return point(submit);
  if (action === 'verify') return {
    status: 'ready', media, description: descriptionText, shortTitle: short?.value ?? null,
  };
  return result('blocked');
}
