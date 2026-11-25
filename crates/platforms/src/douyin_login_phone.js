(origin) => {
  if (location.origin !== origin || !document.body) return null;

  const visible = (element) => !!element?.getClientRects().length &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const phoneLabel = /^(?:手机(?:号码?)?(?:(?:短信)?验证码(?:验证)?|(?:短信)?验证)|短信验证码(?:验证)?)$/;
  const sendSms = '发送短信验证';
  const smsSent = '我已发送';
  const otherMethod = /^(?:手机刷脸验证|人脸(?:识别)?验证|换绑手机(?:号码?)?验证|抖音常用设备(?:扫码|验证))$/;
  const dialog = '.second-verify-panel, dialog, [role="dialog"], [aria-modal="true"]';
  const control = 'button, [role="button"], a, [tabindex]';
  const blocked = ':disabled, [disabled], [aria-disabled="true"], [inert]';
  const normalize = (label) => (label ?? '').replace(/\s+/g, '');
  const labels = new Map();
  const collect = (element, label) => {
    const kind = phoneLabel.test(label) ? 'receiveCode' :
      label === sendSms ? 'sendSms' :
      label === smsSent ? 'smsSent' :
      /^(?:身份验证|安全验证)$/.test(label) ? 'identity' :
      otherMethod.test(label) ? 'otherMethod' : null;
    if (kind && visible(element)) {
      if (!labels.has(kind)) labels.set(kind, new Set());
      labels.get(kind).add(element);
    }
  };
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node; (node = walker.nextNode());) {
    const text = normalize(node.data);
    collect(node.parentElement, text);
    // A single displayed label may be split into spans by the dialog.
    const parent = node.parentElement?.parentElement;
    if (/手机|短信|验证|人脸|身份|安全|发送/.test(text) && parent && parent.childElementCount <= 4) {
      collect(parent, normalize(parent.innerText));
    }
  }
  for (const element of document.querySelectorAll('[aria-label]')) {
    collect(element, normalize(element.getAttribute('aria-label')));
  }

  const hasLabel = (panel, kind) => [...(labels.get(kind) ?? [])].some((node) => panel.contains(node));
  const readInstructions = (panel) => {
    const readField = (prefix, pattern) => {
      const values = new Set();
      const fields = document.createTreeWalker(panel, NodeFilter.SHOW_TEXT);
      for (let node; (node = fields.nextNode());) {
        if (!prefix.test(node.data.trim()) || !visible(node.parentElement)) continue;
        let field = node.parentElement;
        for (let depth = 0; field && field !== panel && depth < 3; depth++, field = field.parentElement) {
          const text = field.innerText.trim().replace(/\s*\n\s*/g, ' ');
          const value = text.match(pattern)?.[1]?.trim();
          if (value) {
            values.add(value);
            break;
          }
        }
      }
      return values.size === 1 ? values.values().next().value : null;
    };
    const message = readField(/^(?:编辑|填写)?短信内容/, /^(?:编辑|填写)?短信内容\s*[:：]\s*([\p{L}\p{N}][\p{L}\p{N} _#*.+-]{0,47})$/u);
    const recipient = readField(/^发送[至到]/, /^发送[至到]\s*[:：]\s*(\+?[0-9]{5,15})$/);
    return message && recipient ? { message, recipient } : null;
  };
  const find = (kind, title, alternatives = []) => {
    const choices = new Map();
    for (const label of labels.get(kind) ?? []) {
      let panel = label.closest(dialog);
      if (!panel) {
        // The login dialog can be rendered without an accessible dialog role.
        for (panel = label.parentElement; panel && panel !== document.body; panel = panel.parentElement) {
          if (hasLabel(panel, title) &&
              (!alternatives.length || alternatives.some((type) => hasLabel(panel, type)))) break;
        }
      }
      if (!panel || panel === document.body || !hasLabel(panel, title)) continue;

      const button = label.closest(control);
      const target = button && panel.contains(button) ? button : label;
      if (!visible(target) || target.closest(blocked) ||
          getComputedStyle(target).pointerEvents === 'none') continue;
      const rect = label.getBoundingClientRect();
      const point = [rect.x + rect.width / 2, rect.y + rect.height / 2];
      const hit = document.elementFromPoint(...point);
      if (target.contains(hit) && !hit.closest(blocked)) choices.set(target, { point, panel });
    }
    // A text span and its enclosing row can describe the same control.
    for (const target of choices.keys()) {
      if ([...choices.keys()].some((other) => other !== target && target.contains(other))) {
        choices.delete(target);
      }
    }
    if (choices.size !== 1) return null;
    const { point, panel } = choices.values().next().value;
    return { kind, point, instructions: kind === 'smsSent' ? readInstructions(panel) : null };
  };
  return find('smsSent', 'sendSms') ??
    find('receiveCode', 'identity', ['sendSms', 'otherMethod']) ??
    find('sendSms', 'identity', ['receiveCode', 'otherMethod']);
}
