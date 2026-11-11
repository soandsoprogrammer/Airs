(origin, selectField = false) => {
  const visible = node => !!node?.getClientRects().length &&
    getComputedStyle(node).visibility === 'visible';
  const creatorRoute = ['/new/home', '/publish/publish'].includes(location.pathname);
  const loginRoute = ['/', '/login', '/login/'].includes(location.pathname);
  if (location.origin !== origin) {
    return selectField ? false : {
      status: location.origin === 'https://www.xiaohongshu.com' &&
        location.pathname === '/website-login/error' ? 'restricted' : 'pending',
    };
  }

  if (selectField) {
    const field = document.activeElement;
    if (!loginRoute || !field?.matches(
      '.sso-login-wrapper input[placeholder="手机号"], .sso-login-wrapper input[placeholder="验证码"]'
    ) || !visible(field) || field.disabled || field.readOnly) return false;
    field.select();
    return true;
  }
  if (creatorRoute) {
    const accounts = [...document.querySelectorAll('#header-area .user-info')].filter(visible);
    if (accounts.length === 1 && accounts[0].querySelector('img.user_avatar[src]:not([src=""])')) {
      const nickname = accounts[0].querySelector('.name-box')?.innerText.trim();
      if (nickname && nickname.length <= 128) return {status: 'creator', nickname};
    }
  }
  if (loginRoute) {
    const boxes = [...document.querySelectorAll('.login-box-container')].filter(visible);
    if (boxes.length === 1) {
      const [box] = boxes;
      const headings = [...box.querySelectorAll('div, span, p')].filter(node =>
        !node.children.length && visible(node) && node.textContent.trim() === 'APP扫一扫登录');
      if (headings.length === 1) {
        const expired = [...box.querySelectorAll('div, span, p, button')].some(node =>
          !node.children.length && visible(node) && /^二维码(?:已)?(?:过期|失效)$/.test(node.textContent.trim()));
        const images = [...box.querySelectorAll('img')].filter(image => {
          const rect = image.getBoundingClientRect();
          return visible(image) && image.complete && image.naturalWidth > 0 &&
            rect.width >= 100 && rect.width <= 512 && Math.abs(rect.width - rect.height) <= 4 &&
            rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight;
        });
        const rect = images.length === 1 ? images[0].getBoundingClientRect() : null;
        const image = rect && {x: rect.left + scrollX, y: rect.top + scrollY, width: rect.width, height: rect.height};
        return {status: 'scan', expired, image};
      }
      if (headings.length > 1) return {status: 'pending'};
      const switches = [...box.querySelectorAll('img.css-wemwzq')].filter(visible);
      const toggle = switches.length === 1 ? switches[0] : null;
      const rect = toggle?.getBoundingClientRect();
      let switchPoint = null;
      if (rect) {
        for (const [left, top] of [[0.5, 0.5], [0.25, 0.25], [0.15, 0.25]]) {
          const x = rect.left + left * rect.width;
          const y = rect.top + top * rect.height;
          if (document.elementFromPoint(x, y) === toggle) {
            switchPoint = [x, y];
            break;
          }
        }
      }
      if (visible(box.querySelector('.sso-login-wrapper'))) {
        return {status: 'login', switchAvailable: !!toggle, switchPoint};
      }
    }
    if ([...document.querySelectorAll('.sso-login-wrapper')].some(visible)) {
      return {status: 'login', switchAvailable: false, switchPoint: null};
    }
  }
  return {status: 'pending'};
}
