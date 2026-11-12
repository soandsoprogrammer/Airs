(origin, oauthOrigin, frameOnly = false, selection = null) => {
  const pending = { status: 'pending' };
  const path = location.pathname.replace(/\/$/, '') || '/';
  const loginPaths = ['/', '/login.html', '/platform/login', '/platform/login-for-iframe'];
  const creatorPaths = ['/platform', '/platform/post/create', '/platform/post/list'];
  const isOauth = location.origin === oauthOrigin && path === '/connect/qrconnect';
  if (frameOnly ? !isOauth && !(location.origin === origin && path === '/platform/login-for-iframe')
    : location.origin !== origin || ![...loginPaths, ...creatorPaths].includes(path)) return pending;

  const visible = (node) => Boolean(node?.getClientRects().length) &&
    !node.closest('[hidden], [inert], [aria-hidden="true"]') &&
    (node.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const find = (scope, selector) => [...scope.querySelectorAll(selector)].filter(visible);
  const one = (nodes) => nodes.length === 1 ? nodes[0] : null;
  const label = (node) => node.innerText.trim();
  const rect = (node) => {
    const bounds = node.getBoundingClientRect();
    return {
      x: bounds.x + (frameOnly ? 0 : scrollX),
      y: bounds.y + (frameOnly ? 0 : scrollY),
      width: bounds.width,
      height: bounds.height,
    };
  };
  const qrSelector = isOauth ? '.js_normal_login img.js_qrcode_img, img.qrcode' : 'img.qrcode';
  const qrImage = (scope) => one(find(scope, qrSelector).filter((node) => {
    if (!node.complete || node.naturalWidth < 96 || node.naturalHeight < 96) return false;
    const source = new URL(node.currentSrc || node.src, location.href);
    return source.protocol === 'data:' ||
      (source.origin === oauthOrigin && source.pathname.startsWith('/connect/qrcode/'));
  }));
  if (frameOnly) {
    const scanStatus = find(document, '.js_wx_after_scan, .js_normal_login');
    if (scanStatus.some((node) => /已扫码|扫描成功|在手机上.*确认/.test(label(node)))) {
      return { status: 'confirmOnPhone' };
    }
    if (scanStatus.some((node) => /二维码已(?:过期|失效)|二维码过期/.test(label(node)))) {
      return { status: 'expired' };
    }
    const image = qrImage(document);
    if (image) return { status: 'scan', image: rect(image) };
    if (isOauth) {
      const action = (scope, selector, text) => {
        const node = one(find(scope, selector).filter((candidate) =>
          label(candidate) === text && !candidate.matches(':disabled, [aria-disabled="true"]')));
        if (!node) return null;
        const control = rect(node);
        const hit = document.elementFromPoint(control.x + control.width / 2, control.y + control.height / 2);
        return node.contains(hit) ? control : null;
      };
      const panel = one(find(document, '.js_quick_login'));
      if (panel) {
        const scan = action(panel, 'button.js_switchToNormal.web_qrcode_switch', '使用其他头像、昵称或账号');
        const quick = action(panel, 'button.js_quick_login_btn.qlogin_btn', '微信快捷登录');
        const nickname = one(find(panel, '.js_quick_login_nickname'))?.innerText.trim();
        if (quick && scan && nickname && nickname.length <= 100) {
          return { status: 'choice', nickname, quick, scan };
        }
        if (scan) return { status: 'switchToScan', control: scan };
      }
    }
    return pending;
  }

  const login = one(find(document, '.login-qrcode-wrap'));
  if (login) {
    const picker = one(find(login, '.choose-finder-area'));
    if (picker && one(find(picker, '.choose-header .choose-title').filter((node) =>
      label(node) === '选择视频号登录'))) {
      const items = find(picker, '.finder-list-wrap .finder-list .finder-item');
      const rows = items.map((item) => ({
        nickname: one(find(item, '.right-part .info .name > span'))?.innerText.trim() ?? '',
        role: one(find(item, '.role'))?.innerText.trim() ?? '',
        available: !item.matches('.baned, [aria-disabled="true"]') &&
          !item.querySelector('.right-part.blocked'),
        avatar: item.querySelector('img[src]')?.getAttribute('src') || null,
      }));
      if (!rows.length || rows.some(({ nickname }) => !nickname)) return pending;
      if (!selection) return { status: 'selectChannel', rows };
      if (rows.length === selection.rows.length && rows.every((row, i) =>
        Object.keys(row).every((key) => row[key] === selection.rows[i][key])) &&
        rows[selection.index]?.available) {
        items[selection.index].click();
        return { status: 'selectingChannel' };
      }
      return pending;
    }
    if (find(login, '.no-account-area .msg-title').some((node) => label(node) === '没有可登录的视频号')) {
      return { status: 'noAccount' };
    }
    if (find(login, '.qr-tip').some((node) => /已扫码|手机上进行确认/.test(label(node)))) {
      return { status: 'confirmOnPhone' };
    }
    if (find(login, '.mask.show .refresh-tip').some((node) => /过期|失效|重试|刷新/.test(label(node)))) {
      return { status: 'expired' };
    }
    const image = qrImage(login);
    if (image) {
      image.scrollIntoView({ block: 'center' });
      return { status: 'scan', image: rect(image) };
    }
  }
  const frame = one(find(login ?? document, 'iframe[src]').filter((node) => {
    const source = new URL(node.src);
    return (login && source.origin === oauthOrigin && source.pathname === '/connect/qrconnect') ||
      (loginPaths.includes(path) && source.origin === origin && source.pathname === '/platform/login-for-iframe');
  }));
  if (frame) {
    frame.scrollIntoView({ block: 'center' });
    const bounds = frame.getBoundingClientRect();
    const hit = document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
    if (hit === frame) return { status: 'scan', frame: rect(frame) };
  }
  if (login || !creatorPaths.includes(path)) return pending;

  const names = [...new Set(find(document, '.account-info > .name, .finder-card.finder-info .finder-nickname')
    .map(label).filter(Boolean))];
  const avatars = [...new Set(find(document, '.account-info > img.avatar, .finder-card.finder-info img.avatar')
    .map((node) => {
      const source = new URL(node.currentSrc || node.src, location.href);
      return source.protocol === 'https:' && source.pathname !== '/' ? source.origin + source.pathname : null;
    }).filter(Boolean))];
  if (names.length > 1 || (names.length === 0 && avatars.length !== 1)) return pending;
  return { status: 'creator', nickname: names[0] ?? null, avatar: avatars.length === 1 ? avatars[0] : null };
}
