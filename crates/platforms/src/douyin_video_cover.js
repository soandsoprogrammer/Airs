(origin, action, orientation, baseline = [], directionBound = false) => {
  const rejected = (reason) => ({ status: 'rejected', reason });
  const waiting = () => ({ status: 'waiting' });
  if (location.origin !== origin ||
      !['/creator-micro/content/publish', '/creator-micro/content/post/video']
        .includes(location.pathname.replace(/\/$/, '')) ||
      !['portrait', 'landscape'].includes(orientation)) return rejected('page');

  const rendered = (element) => Boolean(element?.getClientRects().length) &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [inert]');
  const visible = (element) => rendered(element) &&
    !element.closest('[aria-hidden="true"]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const available = (element) => !element.closest(
    ':disabled, [disabled], [aria-disabled="true"], [aria-busy="true"], .semi-button-disabled, [class^="disabled-"], [class*=" disabled-"]'
  ) && getComputedStyle(element).pointerEvents !== 'none';
  const hasClass = (element, prefix) => [...element.classList].some((name) => name.startsWith(prefix));
  const label = (element) => (element.getAttribute('aria-label') || element.innerText || '').trim();
  const find = (scope, selector) => [...scope.querySelectorAll(selector)].filter(visible);
  const one = (items) => items.length === 1 ? items[0] : null;
  const suggestionSelector = '[class*="verticalSupoortDualCoverModal-"]';
  const suggestions = find(document, suggestionSelector)
    .filter((node) => hasClass(node, 'verticalSupoortDualCoverModal-'));
  const suggestionRoots = new Set(suggestions.map((node) =>
    node.closest('.dy-creator-content-modal')));
  const point = (element, status = 'point') => {
    if (!element || !available(element)) return waiting();
    element.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' });
    const bounds = element.getBoundingClientRect();
    const x = bounds.left + bounds.width / 2;
    const y = bounds.top + bounds.height / 2;
    return element.contains(document.elementFromPoint(x, y)) ? { status, point: [x, y] } : waiting();
  };
  const exactControls = (scope, labels) => {
    const nodes = find(scope, 'button, [role="button"], [role="tab"], [tabindex], span, div')
      .filter((element) => labels.includes(label(element)))
      .filter((element) => ![...element.children].some((child) =>
        visible(child) && labels.includes(label(child))));
    return [...new Set(nodes.map((node) =>
      node.closest('button, [role="button"], [role="tab"]') ?? node))];
  };
  const portraitLabels = ['设置竖封面', '竖封面', '竖版封面', '竖版'];
  const landscapeLabels = ['设置横封面', '横封面', '横版封面', '横版'];
  const labels = orientation === 'portrait' ? portraitLabels : landscapeLabels;
  const orientationName = orientation === 'portrait' ? 'vertical' : 'horizontal';
  const canvasId = `#${orientationName}_coverCanvas`;
  const directionLabels = [...portraitLabels, ...landscapeLabels];
  const editorHeaders = (scope) => find(scope, '[class*="steps-"], [class*="single-title-"]')
      .filter((node) => hasClass(node, 'steps-') || (hasClass(node, 'single-title-') &&
        !node.closest('[class*="steps-"]')));
  const editorTabs = (scope) => {
    const headers = editorHeaders(scope);
    if (!headers.length) return exactControls(scope, directionLabels)
      .filter((node) => !node.closest(`[class*="uploadCrop-"], ${suggestionSelector}`));
    return headers.flatMap((header) => hasClass(header, 'single-title-')
      ? [header].filter((node) => directionLabels.includes(label(node)))
      : exactControls(header, directionLabels));
  };
  const modals = find(document, '.dy-creator-content-modal').filter((modal) =>
    !suggestionRoots.has(modal) && (
      editorHeaders(modal).length ||
      find(modal, '[class*="coverSelect-"]').some((node) => hasClass(node, 'coverSelect-')) ||
      (modal.querySelector('.semi-upload-drag-area-main-text, [class*="upload-tips-"]') &&
        exactControls(modal, ['完成']).length)
    ));
  if (modals.length > 1) return rejected('multipleEditors');
  const modal = modals[0];
  const alerts = find(document,
    '[role="alert"], [aria-live="assertive"], .semi-toast-content, .dy-creator-content-toast-content');
  for (const element of alerts) {
    const message = element.innerText;
    if (/分辨率过低/.test(message)) return rejected('resolution');
    if (/不支持的图片格式/.test(message)) return rejected('format');
    if (/封面.{0,20}(?:上传失败|处理失败|不合规|不支持)|上传封面失败/.test(message)) {
      return rejected('platformAlert');
    }
  }

  // The CDN may use another size template for the same stored image.
  const sourceKey = (value) => {
    try {
      const url = new URL(value, location.href);
      if (url.protocol === 'blob:' && url.origin === origin) return url.href;
      if (url.protocol !== 'https:' || url.pathname === '/') return null;
      return url.origin + url.pathname.replace(/~tplv-[^/]*$/, '');
    } catch {
      return null;
    }
  };
  const source = (element) => {
    if (element instanceof HTMLCanvasElement && element.width && element.height) {
      try {
        const sample = document.createElement('canvas');
        sample.width = 8;
        sample.height = 8;
        const context = sample.getContext('2d');
        if (!context) return null;
        context.drawImage(element, 0, 0, 8, 8);
        const pixels = context.getImageData(0, 0, 8, 8).data;
        if (!pixels.some((value, index) => index % 4 === 3 && value)) return null;
        const digest = pixels.reduce((value, byte) => Math.imul(value ^ byte, 16777619), 2166136261);
        return `canvas:${digest >>> 0}`;
      } catch {
        return null;
      }
    }
    if (element instanceof HTMLImageElement && element.complete && element.naturalWidth > 0) {
      return sourceKey(element.currentSrc || element.src);
    }
    const background = getComputedStyle(element).backgroundImage;
    const match = background.match(/^url\(["']?([^"']+?)["']?\)$/);
    return match ? sourceKey(match[1]) : null;
  };
  const orientationHint = (element) => {
    const name = [
      element.getAttribute('data-cover-orientation'),
      element.getAttribute('aria-label'),
      element.getAttribute('title'),
      element.className,
    ].filter((part) => typeof part === 'string').join(' ');
    if (/portrait|vertical|竖(?:版)?封面/i.test(name)) return 'portrait';
    if (/landscape|horizontal|横(?:版)?封面/i.test(name) || hasClass(element, 'hcover-')) {
      return 'landscape';
    }
    return null;
  };
  const aspectOrientation = (element) => {
    const bounds = element.getBoundingClientRect();
    if (bounds.width > bounds.height * 1.1) return 'landscape';
    if (bounds.height > bounds.width * 1.1) return 'portrait';
    return null;
  };
  const officialCover = (node) => {
    const parent = node.parentElement;
    return parent && hasClass(node, 'cover-') &&
      (hasClass(parent, 'coverControl-') || hasClass(parent, 'coverContainer-')) &&
      [...node.children].some((child) => hasClass(child, 'background-'));
  };
  const cardOrientation = (card) => {
    if (!officialCover(card)) return orientationHint(card);
    const wrapper = card.parentElement;
    const tip = hasClass(wrapper, 'coverControl-') && [...wrapper.querySelectorAll('[class*="cover-tip-"]')]
      .find((node) => hasClass(node, 'cover-tip-') && !card.contains(node));
    if (tip && /^竖封面\s*3\s*[:：]\s*4$/.test(tip.textContent.trim())) return 'portrait';
    if (tip && /^横封面\s*4\s*[:：]\s*3$/.test(tip.textContent.trim())) return 'landscape';
    return orientationHint(card) ?? aspectOrientation(card);
  };
  const orientationOf = (element, cover) => {
    const cardDirection = cardOrientation(cover);
    if (cardDirection) return cardDirection;
    for (let current = element; current && cover.contains(current); current = current.parentElement) {
      const hint = orientationHint(current);
      if (hint) return hint;
    }
    if (element instanceof HTMLImageElement && element.naturalWidth !== element.naturalHeight) {
      return element.naturalWidth > element.naturalHeight ? 'landscape' : 'portrait';
    }
    return aspectOrientation(element);
  };
  const coverRoots = () => find(document,
    '[data-cover-orientation], [class*="cover-"], [class*="coverHorizontal"], [class*="coverVertical"]'
  ).filter((node) => !node.closest('.dy-creator-content-modal, [role="dialog"], [class*="recommendCover"], .xgplayer') &&
    (node.hasAttribute('data-cover-orientation') ||
      [...node.classList].some((name) =>
        /^(?:(?:portrait|landscape|vertical|horizontal)-)?cover(?:-|$|Horizontal|Vertical)/.test(name))));
  const composerSources = () => {
    const roots = coverRoots();
    const cards = roots.filter(officialCover);
    const nodes = cards.length ? cards.filter((card) => cardOrientation(card) === orientation)
      .flatMap((card) => [card, ...[...card.children].filter((media) => hasClass(media, 'background-'))
        .flatMap((media) => [media, ...media.querySelectorAll('img')]).filter(rendered)])
      : roots.flatMap((cover) => [cover, ...find(cover, 'img')]
        .filter((element) => orientationOf(element, cover) === orientation));
    return [...new Set(nodes.map(source).filter(Boolean))].sort();
  };
  if (action === 'composer') {
    return modal || find(document, '[role="dialog"], [aria-modal="true"]').length
      ? waiting() : { status: 'sources', sources: composerSources() };
  }
  if (action === 'open') {
    if (modal) return { status: 'open' };
    if (find(document, '[role="dialog"], [aria-modal="true"]').length) {
      return rejected('otherDialog');
    }
    const cards = coverRoots().filter(officialCover);
    if (cards.length) {
      const card = one(cards.filter((node) => cardOrientation(node) === orientation));
      // Some layouts share a single entry and let the modal select the direction.
      const result = point(card ?? (cards.length === 1 ? cards[0] : null));
      if (card && result.status === 'point' && hasClass(card.parentElement, 'coverControl-')) {
        result.directionBound = true;
      }
      return result;
    }
    const coverLabels = ['编辑封面', '选择封面', '设置封面', '设置视频封面'];
    const trigger = one(exactControls(document, coverLabels)
      .filter((node) => !node.closest('[class*="recommendCover"]')));
    if (trigger) return point(trigger);
    const hover = one(coverRoots().filter((cover) =>
      coverLabels.some((text) => cover.textContent.includes(text))));
    return point(hover, 'hover');
  }
  if (action === 'confirm') {
    if (suggestions.length) {
      const suggestion = one(suggestions);
      if (!suggestion || !find(suggestion, 'p[class*="title-"]').some((title) =>
        /^设置[横竖]封面获更多流量$/.test(label(title)))) return rejected('coverSuggestion');
      return point(one(exactControls(suggestion, ['暂不设置'])));
    }
    const confirmation = one(find(document,
      '.semi-modal-content, .dy-creator-content-modal-content, [role="alertdialog"]')
      .filter((dialog) => dialog !== modal && !dialog.contains(modal) &&
        /是否确认应用此封面|是否(?:仍然|仍要)完成|(?:横|竖)(?:版)?封面缺失|未设置(?:横|竖)(?:版)?封面|确定完成[？?]/
          .test(dialog.innerText)));
    if (confirmation) return point(one(exactControls(confirmation,
      ['确定', '确认', '仍然完成', '仍要完成', '继续'])));
    return modal ? waiting() : { status: 'closed' };
  }
  if (!modal) return waiting();

  const tabs = editorTabs(modal);
  const tab = one(tabs.filter((node) => labels.includes(label(node))));
  const tabControl = tab?.closest('[role="tab"], .semi-tabs-tab, [class*="step-"]') ?? tab;
  const tabState = tabControl?.getAttribute('aria-selected') ??
    tabControl?.getAttribute('aria-pressed') ?? tabControl?.getAttribute('data-state');
  const stepActive = tabControl && hasClass(tabControl, 'step-')
    ? hasClass(tabControl, 'step-active-') : null;
  const selectPane = one(find(modal, '[class*="coverSelect-"]')
    .filter((node) => hasClass(node, 'coverSelect-')));
  const visibleCanvas = one(find(modal, '#vertical_coverCanvas, #horizontal_coverCanvas'));
  const active = stepActive ?? (tabState != null
    ? ['true', 'active'].includes(tabState)
    : Boolean(tabControl && [...tabControl.classList].some((name) =>
      /^(?:semi-tabs-tab-active|tab-active|tabActive|active|selected)(?:-|$)/.test(name))) ||
      visibleCanvas?.matches(canvasId) || (tabs.length === 1 && tab === tabs[0]) ||
      Boolean(directionBound && selectPane && !tabs.length));
  if (action === 'tab') return active ? { status: 'active' } : point(tabControl);
  const compactAreas = find(modal, '[class*="upload-tips-"]').filter((node) =>
    hasClass(node, 'upload-tips-') && label(node) === '上传封面').map((node) => node.parentElement);
  const dragAreas = find(selectPane ?? modal, '.semi-upload:has(.semi-upload-drag-area-main-text)')
    .filter((area) => !/生成参考图|AI参考图/.test(area.innerText));
  const uploaders = [...new Set([
    ...dragAreas,
    ...compactAreas.flatMap((area) => [...area.querySelectorAll('.semi-upload')]
      .filter((node) => rendered(node) && (hasClass(node, 'upload-') ||
        (area.contains(node.parentElement) && hasClass(node.parentElement, 'upload-'))))),
  ])];
  const dragArea = one(uploaders);
  const fileInput = dragArea && available(dragArea) ? one([...dragArea.querySelectorAll(
    'input[type="file"].semi-upload-hidden-input:not([disabled])'
  )]) : null;
  if (action === 'input') {
    return active ? fileInput : null;
  }
  if (!active) return waiting();
  if (action === 'upload-pane') {
    if (fileInput) return { status: 'active' };
    if (uploaders.length > 1) return rejected('multipleUploaders');
    if (!selectPane) return waiting();
    const tabItems = find(selectPane, '[class*="tabItem-"]')
      .filter((node) => hasClass(node, 'tabItem-'));
    return point(one(tabItems.length ? tabItems.filter((node) => label(node) === '上传封面')
      : exactControls(selectPane, ['上传封面'])));
  }

  // Limit evidence to the upload widget and the selected cover preview. The
  // recommendation strip and AI reference image upload are separate controls.
  const selectedPanel = tabControl?.getAttribute('aria-controls');
  const panel = selectedPanel && document.getElementById(selectedPanel);
  const scope = visible(panel) && modal.contains(panel) ? panel : modal;
  const coverCanvas = scope.querySelector(canvasId);
  const uploadCrop = one(find(selectPane ?? scope, '[class*="uploadCrop-"]')
    .filter((node) => hasClass(node, 'uploadCrop-')));
  const evidenceNodes = [
    ...(dragArea ? [dragArea, ...find(dragArea, 'img, [style*="background-image"]')] : []),
    ...compactAreas.flatMap((area) => find(area, '[class*="bg-"]')
      .filter((node) => hasClass(node, 'bg-'))),
    ...(uploadCrop ? find(uploadCrop, 'canvas, img, [style*="background-image"]') : []),
    ...find(scope, '[class*="cover-preview"], [class*="coverPreview"], [data-cover-preview]')
      .filter((node) => !node.closest('[class*="recommendCover"], [class*="recommend-cover"], [class*="reference"]'))
      .flatMap((node) => [node, ...find(node, 'img')]),
  ];
  if (visible(coverCanvas)) evidenceNodes.push(coverCanvas);
  const sources = [...new Set(evidenceNodes.map(source).filter(Boolean))].sort();
  if (action === 'preview') return { status: 'sources', sources };
  if (action === 'finish') {
    if (!sources.some((item) => !baseline.includes(item)) ||
        find(scope, '.semi-upload-file-card-uploading, [role="progressbar"], [aria-busy="true"]').length) {
      return waiting();
    }
    const cropSelectors = uploadCrop ? find(uploadCrop, '[class*="directionSelect-"]')
      .filter((node) => hasClass(node, 'directionSelect-')) : [];
    if (cropSelectors.length > 1) return rejected('multipleCropSelectors');
    if (cropSelectors.length === 1) {
      const options = find(cropSelectors[0], '[class*="selectItem-"]').filter((node) =>
        hasClass(node, 'selectItem-') && ['竖封面', '横封面'].includes(label(node)));
      const selected = one(options.filter((node) => hasClass(node, 'selected-')));
      const requested = one(options.filter((node) =>
        label(node) === (orientation === 'portrait' ? '竖封面' : '横封面')));
      if (!selected || !requested) return waiting();
      if (selected !== requested) return point(requested, 'adjustCrop');
    }
    const done = point(one(exactControls(uploadCrop ?? modal, ['完成'])));
    if (done.status === 'point') done.sources = sources.filter((item) => !baseline.includes(item));
    return done;
  }
  return rejected('unknown');
}
