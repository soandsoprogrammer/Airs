(origin, action, kind, nickname, imageCount, title, body) => {
  const route = new URL(location.href);
  if (route.origin !== origin || route.pathname !== '/publish/publish' ||
      route.searchParams.getAll('target').length !== 1 || route.searchParams.get('target') !== kind ||
      [...route.searchParams.keys()].some(key => !['from', 'target'].includes(key))) {
    return {status: 'wrongPage'};
  }

  const visible = node => !!node?.getClientRects().length &&
    getComputedStyle(node).visibility === 'visible';
  const all = (selector, root = document) => [...root.querySelectorAll(selector)].filter(visible);
  const only = nodes => nodes.length === 1 ? nodes[0] : null;
  const account = only(all('#header-area .user-info'));
  if (!account?.querySelector('img.user_avatar[src]:not([src=""])')) return {status: 'waiting'};
  if (account.querySelector('.name-box')?.innerText.trim() !== nickname) {
    return {status: 'wrongAccount'};
  }

  const composer = only(all('.publish-page-content'));
  const titleInput = composer && only(all('input[placeholder*="填写标题"]', composer));
  const editor = composer && only(all(
    '.tiptap.ProseMirror[contenteditable="true"], .tiptap-container .ProseMirror[contenteditable="true"], .ProseMirror[contenteditable="true"]:has(p[data-placeholder*="输入正文描述"])',
    composer,
  ));
  const normalize = text => text.replace(/\r\n?/g, '\n').replace(/\u00a0/g, ' ').trim();
  const matchingFields = () => titleInput?.value === title && editor && normalize(editor.innerText) === body;

  if (!titleInput || !editor) {
    const uploadRegion = '.upload-wrapper, [class^="upload-content"]';
    const input = only([...document.querySelectorAll(
      ':is(.upload-wrapper, [class^="upload-content"]) input.upload-input[type="file"]',
    )].filter(node => visible(node.closest(uploadRegion)) && !node.disabled &&
        (kind === 'image'
          ? node.multiple && /image\/|\.(jpe?g|png|webp)(?:,|$)/i.test(node.accept)
          : !node.multiple && /video\/|\.(mp4|mov)(?:,|$)/i.test(node.accept))));
    if (action === 'uploadInput') return input;
    if (all('[role="alert"], .creator-publish-toast').some(el =>
      /上传失败|格式不支持|未上传|文件过大|图片大小不超过|视频解析失败/.test(el.innerText))) {
      return {status: 'failed'};
    }
    return {status: input ? 'selectFile' : 'waiting'};
  }

  if (all('[role="dialog"][aria-modal="true"], .el-overlay-dialog, .creator-modal-style').length) {
    return {status: 'blocked'};
  }

  if (kind === 'image') {
    const items = [...composer.querySelectorAll('.img-upload-area .img-preview-area .img-container')];
    if (items.length > imageCount || items.some(item => item.querySelector('.mask.failed'))) {
      return {status: 'failed'};
    }
    if (items.length !== imageCount || items.some(item =>
      item.matches('[aria-busy="true"], .dragging-placeholder') ||
      item.querySelector('.mask.uploading, .hover-mask.prerender, .processing-container') ||
      ![...item.querySelectorAll('img')].some(image => image.complete && image.naturalWidth > 0))) {
      return {status: 'uploading'};
    }
  } else {
    const videoTitle = only(all('.video-plugin-title', composer));
    if (!videoTitle) return {status: 'waiting'};
    const section = videoTitle.parentElement;
    if (all('.fail', section).length) return {status: 'failed'};
    const uploading = only(all('.uploading .progress', section));
    if (all('.uploading', section).length) {
      const percent = uploading?.innerText.match(/(\d{1,3})\s*%/);
      return {status: 'uploading', percent: percent ? Math.min(Number(percent[1]), 100) : null};
    }
    if (all('.transcoding', section).length || !only(all('.preview-new', section))) {
      return {status: 'processing'};
    }
  }

  if (action === 'title' || action === 'body') {
    const field = action === 'title' ? titleInput : editor;
    if (action === 'title' ? titleInput.value && titleInput.value !== title : normalize(editor.innerText)) {
      return {status: 'unexpectedContent'};
    }
    field.focus();
    if (field !== document.activeElement) return {status: 'blocked'};
    if (action === 'title') titleInput.select();
    return {status: 'focused'};
  }

  if (action === 'submit' || action === 'submitControl') {
    if (!matchingFields()) return {status: 'unexpectedContent'};
    const publish = only(all('xhs-publish-btn', composer));
    if (!publish || publish.getAttribute('is-publish') === 'false' ||
        (publish.getAttribute('submit-text') || '发布').trim() !== '发布' ||
        ['submit-disabled', 'submit-loading'].some(attr =>
          publish.hasAttribute(attr) && publish.getAttribute(attr) !== 'false')) {
      return {status: 'blocked'};
    }
    const privacy = all('.permission-card-select', composer);
    if (privacy.length > 1 || privacy.some(control => {
      const value = control.querySelector('input[readonly]')?.value || control.innerText;
      return !/^公开可见/.test(value.trim());
    })) return {status: 'blocked'};
    return action === 'submitControl' ? publish : {status: 'ready'};
  }

  return {status: matchingFields() ? 'filled' : 'ready'};
}
