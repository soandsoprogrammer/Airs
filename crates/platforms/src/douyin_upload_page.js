(creatorOrigin, retryInputOnly = false) => {
  const observation = (status, percent = null, failure = null) => retryInputOnly
    ? null : { status, percent, ...(failure ? { failure } : {}) };
  if (location.origin !== creatorOrigin || !document.body) return observation('waiting');

  const path = location.pathname.replace(/\/$/, '');
  const composer = path === '/creator-micro/content/publish' ||
    path === '/creator-micro/content/post/video';
  const uploadPage = path === '/creator-micro/content/upload';
  const visible = (element) => !!element && element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);

  // Read text from visible nodes so a hidden status inside a visible card cannot
  // turn into completion evidence through the card's aggregate text.
  const textParts = (root, ignored = '') => {
    const parts = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let node; (node = walker.nextNode());) {
      const text = node.data.trim();
      if (text && visible(node.parentElement) && !node.parentElement.closest('aside, [role="note"]') &&
          (!ignored || !node.parentElement.closest(ignored))) {
        parts.push(text);
      }
    }
    return parts;
  };
  const hasMessage = (roots, pattern, ignored) => roots.some((root) =>
    textParts(root, ignored).some((part) => part.split(/\n/).some((line) => pattern.test(line.trim()))));

  const dialogs = [...document.querySelectorAll('dialog, [role="dialog"], [aria-modal="true"]')]
    .filter(visible);
  const loginMessage = /^(?:扫码登录|验证码登录|请先登录|请重新登录|登录抖音创作者中心)/;
  if (hasMessage(dialogs, loginMessage) ||
      [...document.querySelectorAll('img[alt*="扫码登录"]')].some(visible) ||
      (!composer && !uploadPage &&
        hasMessage([...document.querySelectorAll('h1, h2, h3')].filter(visible), loginMessage))) {
    return observation('needsLogin');
  }
  if (!composer && !uploadPage) return observation('waiting');

  const widgets = [...document.querySelectorAll('[class*="long-card"], [class*="progress-div"]')]
    .filter((element) => visible(element) && [...element.classList].some((name) =>
      name.startsWith('long-card') || name.startsWith('progress-div')));
  const alerts = [...document.querySelectorAll('[role="alert"], [aria-live="assertive"]')]
    .filter(visible);
  const statusRoots = [...widgets, ...alerts];
  const disabledSelector = ':disabled, [disabled], [aria-disabled="true"]';
  const ignoredStatus = '.progress-p, [class*="progress-p-"], .dy-creator-content-modal, .semi-modal';
  const uploadFailure = /^(?:视频)?上传失败(?:$|[\s，,:：。！!])/;

  // The failed video card contains a regular replacement file input. Its
  // wrapper is visible, but the platform hides the input with CSS.
  const retryControls = composer ? [...document.querySelectorAll(
    '.upload-card-IBCFN_ .progress-div > div.upload-btn-PdfuUv.progress-sub-ITjyPa',
  )].filter((control) => visible(control) &&
    textParts(control, ignoredStatus).join('').replace(/\s+/g, '') === '上传失败，重新上传') : [];
  const lines = (roots) => roots.flatMap((root) => textParts(root, ignoredStatus)
    .flatMap((text) => text.split(/\n/).map((line) => line.trim())));
  const failureText = [...lines(widgets), ...lines(alerts).filter((line) =>
    retryControls.length || /^(?:视频|文件|上传失败|不支持(?:该|此)?视频格式)/.test(line))];
  const hasFailure = (pattern) => failureText.some((line) =>
    pattern.test(line.replace(/^(?:视频)?上传失败[\s，,:：。！!]+/, '')));
  const fileFailure = hasFailure(
    /^(?:(?:视频(?:的)?(?:文件|格式|编码|分辨率|时长|大小|码率)|文件(?:格式|大小)).{0,40}(?:不支持|不受支持|不符合|不正确|不合法|超出|超过|过大|过小|过高|过低|限制)|不支持(?:该|此)?视频格式|请上传(?:符合要求|支持格式)的?视频)/
  );
  const processingFailure = hasFailure(
    /^(?:视频(?:文件)?)?(?:解码|转码|解析|处理)(?:视频|文件)?(?:失败|异常)(?:$|[\s，,:：。！!])/
  );
  const networkFailure = hasFailure(
    /^(?:网络(?:异常|错误|连接失败|连接超时|不稳定|已断开)|连接超时|请求超时)(?:$|[\s，,:：。！!])/
  );
  const serviceFailure = hasFailure(
    /^(?:服务器(?:繁忙|异常|错误)|服务(?:器)?暂不可用|系统(?:繁忙|异常))(?:$|[\s，,:：。！!])/
  );
  const failure = fileFailure ? 'file' : processingFailure ? 'processing'
    : networkFailure ? 'network' : serviceFailure ? 'service' : null;

  if (retryControls.length === 1 && !fileFailure && !processingFailure && !dialogs.length) {
    const control = retryControls[0];
    const input = control.querySelectorAll(
      ':scope > input[type="file"].upload-btn-input-UY_qeY[name="upload-btn"]',
    );
    if (input.length === 1 && !control.closest(`${disabledSelector}, [aria-busy="true"]`) &&
        getComputedStyle(control).pointerEvents !== 'none' &&
        !input[0].matches(`${disabledSelector}, [multiple], [directory], [webkitdirectory]`) &&
        !input[0].files?.length &&
        input[0].accept.split(',').some((item) => /^video\/(?:\*|[\w.+-]+)$/i.test(item.trim()))) {
      return retryInputOnly ? input[0] : observation('retryable', null, failure);
    }
  }
  if (retryInputOnly) return null;
  const alertFailure = /^视频上传失败(?:$|[\s，,:：。！!])|^上传失败(?:[。！!])?$|^上传失败[，,:：]\s*(?:请(?:稍后)?重试|请重新上传|网络|服务器|系统)/;
  if (retryControls.length || failure || hasMessage(widgets, uploadFailure, ignoredStatus) ||
      hasMessage(alerts, alertFailure, ignoredStatus)) return observation('failed', null, failure);

  const decoding = composer && hasMessage(
    [...document.querySelectorAll('.upload-card-IBCFN_ .encoding-jbSjaQ .loading-M_XtMr')],
    /^视频解码中(?:\.{3}|…)?$/,
  );
  if (decoding || hasMessage(statusRoots, /^(?:视频)?(?:正在处理|处理中|正在转码|转码中)/)) {
    return observation('processing');
  }

  const progressSelector = 'progress, [role="progressbar"]';
  const progressBars = [...new Set(widgets.flatMap((widget) => [
    ...(widget.matches(progressSelector) ? [widget] : []),
    ...widget.querySelectorAll(progressSelector),
  ]))].filter((bar) => visible(bar) && !bar.closest('aside, [role="note"]'));
  const numericAttribute = (element, attribute, defaultValue = null) => {
    const text = element.getAttribute(attribute)?.trim();
    if (text === undefined) return defaultValue;
    if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(text)) return null;
    const value = Number(text);
    return Number.isFinite(value) ? value : null;
  };
  const legacyPercentages = progressBars.map((bar) => {
    const native = bar instanceof HTMLProgressElement;
    const value = numericAttribute(bar, native ? 'value' : 'aria-valuenow');
    const min = native ? 0 : numericAttribute(bar, 'aria-valuemin', 0);
    const max = numericAttribute(bar, native ? 'max' : 'aria-valuemax', native ? 1 : 100);
    if (value === null || min === null || max === null || max <= min || value < min || value > max) {
      return null;
    }
    if (value === max) return 100;
    const percent = (value - min) * 100 / (max - min);
    if (!Number.isFinite(percent) || percent >= 100) return null;
    // Avoid displaying 28% when a normalized .29 bar evaluates to 28.999999999999996.
    const integer = Math.round(percent);
    return integer < 100 && Math.abs(percent - integer) <= Number.EPSILON * percent
      ? integer : percent;
  });
  const activeProgress = legacyPercentages.some((percent) => percent !== null && percent < 100) ||
    progressBars.some((bar) => bar instanceof HTMLProgressElement && !bar.hasAttribute('value'));

  // The newer composer exposes progress as visible text instead of a progressbar.
  const composerProgressPrefixes = [
    'text-', 'upload-progress-inner-', 'upload-progress-', 'uploading-container-', 'upload-card-',
  ];
  const composerLabels = composer ? document.querySelectorAll(
    '[class*="upload-card-"] [class*="upload-progress-inner-"] > span[class*="text-"]',
  ) : [];
  const composerPercentages = [...composerLabels].flatMap((label) => {
    if (!visible(label)) return [];
    let element = label;
    for (const prefix of composerProgressPrefixes) {
      if (!element || ![...element.classList].some((name) => name.startsWith(prefix))) return [];
      element = element.parentElement;
    }
    const match = /^(\d+(?:\.\d+)?)\s*[%％]$/.exec(textParts(label).join(''));
    const percent = match ? Number(match[1]) : NaN;
    return percent <= 100 ? [percent] : [];
  });

  // Prefer the composer's displayed number. With conflicting or incomplete
  // readings in the selected source, report the phase without a number.
  const samples = composerPercentages.length > 0 ? composerPercentages : legacyPercentages;
  const [first] = samples;
  const percent = first != null && samples.every((sample) => sample === first) ? Math.floor(first) : null;
  if (activeProgress || composerPercentages.some((sample) => sample < 100) ||
      hasMessage(statusRoots, /^(?:视频)?(?:正在上传|上传中)/)) {
    return observation('uploading', percent);
  }

  const widgetControls = widgets.flatMap((widget) =>
    [widget, ...widget.querySelectorAll('div, span, button, [role="button"]')]);
  const buttonSelector = 'button, [role="button"]';
  const composerButtons = composer ? [...document.querySelectorAll(buttonSelector)] : [];
  const reupload = [...widgetControls, ...composerButtons].some((control) => {
    if (!visible(control) || control.closest(`aside, [role="note"], ${disabledSelector}`)) return false;
    const button = control.matches(buttonSelector);
    const ignored = button ? disabledSelector : `p, ${disabledSelector}`;
    const label = (control.getAttribute('aria-label') || textParts(control, ignored).join('')).trim();
    return /^重新上传(?:视频)?$/.test(label);
  });
  const complete = reupload || hasMessage(statusRoots, /^(?:视频)?上传(?:成功|完成)[。！!]?$/);
  if (complete) {
    const title = document.querySelector('input[placeholder*="填写作品标题"]');
    const description = document.querySelector('div.zone-container[contenteditable="true"]');
    return observation(visible(title) && !title.matches(':disabled, [readonly]') && visible(description)
      ? 'ready' : 'editorMissing');
  }

  return progressBars.length > 0 || composerPercentages.length > 0
    ? observation('uploading', percent) : observation('waiting');
}
