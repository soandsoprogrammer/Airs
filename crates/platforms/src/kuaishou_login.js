(creatorOrigin, passportOrigin, openQr, roleClick) => {
  const visible = (element) => !!element && element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const oneVisible = (root, selector) => {
    const elements = [...root.querySelectorAll(selector)].filter(visible);
    return elements.length === 1 ? elements[0] : null;
  };
  const text = (element) => (element?.innerText ?? '').trim();
  const enabled = (element) => visible(element) &&
    !element.closest(':disabled, [disabled], [aria-disabled="true"], [inert], .is-disabled') &&
    getComputedStyle(element).pointerEvents !== 'none';
  const path = location.pathname.replace(/\/$/, '');
  const pending = { status: 'pending' };
  if (location.origin === creatorOrigin) {
    if (!['/profile', '/article/publish/video'].includes(path)) return pending;
    const prompt = oneVisible(document, 'a.login');
    if (text(prompt) === '立即登录') return { status: 'loggedOut' };
    const roleDialog = oneVisible(document, '.role-selector .el-dialog, .role-selector.el-dialog');
    if (roleDialog && text(oneVisible(roleDialog, 'p.tip')) === '请选择您要登录的角色') {
      const roleSelection = { status: 'roleSelection' };
      if (!roleClick) return roleSelection;
      const { x, y, width, height } = roleClick;
      if (innerWidth !== width || innerHeight !== height) return roleSelection;
      const hit = document.elementFromPoint(x, y);
      const control = hit?.closest('.role .role-item, .confirm__btn');
      if (!enabled(control) || !roleDialog.contains(control)) return roleSelection;
      const isOption = control.matches('.role-item') && text(oneVisible(control, '.radio__content__name'));
      const isConfirm = control === oneVisible(roleDialog, '.confirm__btn') && text(control) === '确定';
      if (!isOption && !isConfirm) return roleSelection;
      control.click();
      return { status: 'roleActionApplied' };
    }
    const header = oneVisible(document, '.header-bar .user-info-dpd');
    if (!header || !oneVisible(header, '.user-info-avatar')) return pending;
    const nickname = text(oneVisible(header, '.user-info-name'));
    if (!nickname || nickname.length > 128 || nickname === 'TA') return pending;
    return { status: 'creator', nickname };
  }
  if (location.origin !== passportOrigin || path !== '/pc/account/login' ||
      new URLSearchParams(location.search).get('sid') !== 'kuaishou.web.cp.api') return pending;

  const form = oneVisible(document, 'main#login-form');
  if (!form) return pending;
  const qrPanel = oneVisible(form, 'div.qr-login');
  if (qrPanel) {
    const label = text(qrPanel);
    if (/二维码已过期|二维码已失效/.test(label)) return { status: 'expired' };
    if (/扫码成功|请在手机上确认/.test(label)) return { status: 'confirmOnPhone' };
    const codes = form.querySelectorAll("div.qr-login img[alt='qrcode']");
    if (codes.length !== 1) return pending;
    const code = codes[0];
    const size = code.naturalWidth;
    const png = code.currentSrc;
    if (!visible(code) || !code.complete || size < 21 || size > 512 || size !== code.naturalHeight ||
        !png.startsWith('data:image/png;base64,') || png.length > 1_400_000) return pending;

    // Inspect the image itself. The passport can position it outside a narrow browser viewport.
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = size;
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    if (!ctx) return pending;
    ctx.fillStyle = '#fff';
    ctx.fillRect(0, 0, size, size);
    ctx.drawImage(code, 0, 0);
    const pixels = ctx.getImageData(0, 0, size, size).data;
    let dark = 0;
    let light = 0;
    for (let i = 0; i < pixels.length; i += 4) {
      if (Math.max(pixels[i], pixels[i + 1], pixels[i + 2]) < 128) dark++;
      if (Math.min(pixels[i], pixels[i + 1], pixels[i + 2]) > 192) light++;
    }
    const minimum = size * size / 20;
    return dark >= minimum && light >= minimum ? { status: 'qr', png } : pending;
  }
  const switcher = oneVisible(form, 'div.platform-switch');
  if (!enabled(switcher) || text(switcher) !== '扫码登录') return pending;
  if (!openQr) return { status: 'qrSwitch' };
  switcher.click();
  return { status: 'qrOpened' };
}
