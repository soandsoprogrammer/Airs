(origin, nickname, caption, images, confirmation, loginProbe, prepareProbe, photoProbe) => {
  const fail = (status) => ({ status });
  const tabs = new URL(location.href).searchParams.getAll('tabType');
  const atlas = images !== null;
  const correctTab = atlas ? tabs.length === 1 && tabs[0] === '2'
    : (tabs.length === 0 || (tabs.length === 1 && tabs[0] === '1'));
  if (location.origin !== origin || location.pathname !== '/article/publish/video' ||
      !correctTab) {
    return fail('wrongPage');
  }

  const account = loginProbe(origin, '', false, null);
  if (account.status !== 'creator' || account.nickname !== nickname) return fail('accountChanged');

  const visible = (element) => element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const find = (root, selector) => [...root.querySelectorAll(selector)].filter(visible);
  const dialogs = find(document, 'dialog, [role="dialog"], [aria-modal="true"], .ant-modal');
  if ((!confirmation && dialogs.length) ||
      find(document, '[aria-invalid="true"]').length) return fail('blocked');

  const preparation = (atlas ? photoProbe : prepareProbe)(origin, 'verify', caption);
  if (preparation.status === 'ready') return fail('contentChanged');
  if (preparation.status !== 'filled') return fail('uploadNotReady');
  if (atlas && (preparation.images?.length !== images.length ||
      preparation.images.some((url, index) => url !== images[index]))) return fail('imagesChanged');

  const roots = atlas
    ? find(document, '#root-video-publish #onvideo_creator_platform [class*="_section_"]')
      .filter((node) => find(node, ':scope > [class*="_section-header_"]')
        .some((header) => header.innerText.trim() === '发布图文'))
    : find(document, '#root-video-publish #onvideo_creator_platform #joyride-wrapper');
  if (roots.length !== 1) return fail('publishUnavailable');
  const root = roots[0];
  if (find(root, '.ant-select-selection-placeholder').some((element) =>
      element.innerText.trim() === '请选择作者声明')) return fail('declarationRequired');

  const radios = find(root, '.ant-radio-group label.ant-radio-wrapper');
  const radioText = (label) => label.innerText.replace(/\s+/g, '');
  const expectedSelection = (options, expected, required = false) => {
    const labels = radios.filter((label) => options.includes(radioText(label)));
    if (!labels.length) return !required;
    const selected = labels.filter((label) => label.querySelector('input.ant-radio-input')?.checked);
    return selected.length === 1 && expected.includes(radioText(selected[0]));
  };
  const publicOptions = ['所有人可见', '公开（所有人可见）', '公开'];
  const privateOptions = ['好友可见', '仅自己可见', '私密（仅自己可见）', '私密'];
  if (!expectedSelection(['立即发布', '定时发布'], ['立即发布']) ||
      !expectedSelection([...publicOptions, ...privateOptions], publicOptions, atlas)) {
    return fail('publishOptionsChanged');
  }

  let controls;
  if (confirmation) {
    if (!dialogs.length) return fail('noConfirmation');
    const confirmDialogs = find(document, '.ant-modal-confirm-centered .ant-modal-confirm');
    if (confirmDialogs.length !== 1) return fail('blocked');
    const modal = confirmDialogs[0];
    if (dialogs.some((dialog) => !dialog.contains(modal) && !modal.contains(dialog))) {
      return fail('blocked');
    }
    const titles = find(modal, '.ant-modal-confirm-title').map((title) => title.innerText.trim());
    const namedConfirmation = titles.length === 1 && ['确认发布', '发布确认'].includes(titles[0]);
    controls = find(modal, 'button.ant-btn-primary').filter((element) => {
      const label = element.innerText.replace(/\s+/g, '');
      return label === '确认发布' || (namedConfirmation && ['确认', '确定'].includes(label));
    });
  } else {
    const buttonScope = atlas ? '_section-form-btns_' : '_edit-section-btns_';
    controls = find(root, `[class*="${buttonScope}"] > [class*="_button-primary_"]`)
      .filter((element) => element.innerText.trim() === '发布');
  }
  if (controls.length !== 1) return fail('publishUnavailable');
  const button = controls[0];
  if (button.closest(':disabled, [disabled], [aria-disabled="true"], [aria-busy="true"], [class*="_disabled_"]') ||
      getComputedStyle(button).pointerEvents === 'none') return fail('publishUnavailable');

  button.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' });
  const rect = button.getBoundingClientRect();
  const x = rect.left + rect.width / 2;
  const y = rect.top + rect.height / 2;
  if (!button.contains(document.elementFromPoint(x, y))) return fail('blocked');
  return { status: 'ready', point: [x, y] };
}
