(creatorOrigin, action = 'observe', caption = '') => {
    const attemptKey = Symbol.for('social-publisher.kuaishou.upload');
    const result = (status, percent = null) => ({status, percent});
    const tab = new URL(location.href).searchParams.getAll('tabType');
    const expectedPage = location.origin === creatorOrigin
        && location.pathname === '/article/publish/video'
        && (tab.length === 0 || (tab.length === 1 && tab[0] === '1'));
    const visible = (element) => {
        const style = getComputedStyle(element);
        return element.getClientRects().length > 0 && style.visibility === 'visible'
            && !element.closest('[hidden], [aria-hidden="true"], [inert]');
    };
    const text = (element) => element.innerText.trim();
    const find = (root, selector) => [...root.querySelectorAll(selector)].filter(visible);
    const one = (elements) => elements.length === 1 ? elements[0] : null;

    const root = expectedPage && one(find(document,
        '#root-video-publish #onvideo_creator_platform #joyride-wrapper'));
    if (!root || find(document, 'a.login').some((element) => text(element) === '立即登录')) {
        return action === 'input' || action === 'uploadButtonPoint' ? null : result('waiting');
    }

    const rejection = (message) => {
        const known = [
            '账号异常暂时无法上传视频',
            '账号未绑定手机号，请绑定后再上传视频',
            '暂时无法上传视频，请稍后再试',
            '无法上传视频，请稍后再试',
            '已达今日上传作品上限',
            '读取视频文件信息出现错误，请更换上传的视频文件',
            '选择视频大小超过最大限制，请重新选择视频文件',
            '选择视频时长超过最大限制，请重新选择视频文件',
        ];
        return known.includes(message) || /^请上传 [a-z\d、, ]{1,80} 格式的视频$/i.test(message);
    };
    const uploadMessages = () => find(document,
        '#microSupport .ant-message-error, #microSupport .ant-modal-confirm-content');
    const watchFileSelection = (input) => {
        window[attemptKey]?.observer.disconnect();
        const initial = new Map(uploadMessages().map((message) => [message, text(message)]));
        const attempt = {fileBytes: null, rejection: null, observer: null};
        const rememberFile = () => {
            if (input.files?.length === 1) attempt.fileBytes = input.files[0].size;
        };
        input.addEventListener('input', rememberFile, {capture: true, once: true});
        input.addEventListener('change', rememberFile, {capture: true, once: true});
        attempt.observer = new MutationObserver(() => {
            for (const node of uploadMessages()) {
                const message = text(node);
                if (initial.get(node) !== message && rejection(message)) {
                    attempt.rejection = message;
                    attempt.observer.disconnect();
                    break;
                }
            }
        });
        attempt.observer.observe(document.body, {childList: true, subtree: true, characterData: true});
        window[attemptKey] = attempt;
    };
    const observed = (status, percent = null) => {
        const attempt = window[attemptKey];
        return {...result(status, percent), fileBytes: attempt?.fileBytes, rejection: attempt?.rejection};
    };

    const editor = one(find(root, '#work-description-edit[contenteditable="true"]'));
    const fileCards = find(root, '[class*="_edit-file-container_"]');
    const card = one(fileCards);
    const uploadButton = one(find(root, 'button[class*="_upload-btn_"]')
        .filter((element) => !element.disabled && text(element) === '上传视频'));
    const fileInput = one([...root.querySelectorAll(':scope > input[type="file"]')]);
    const acceptsVideo = fileInput?.accept.split(',').map((value) => value.trim().toLowerCase())
        .some((value) => value.startsWith('video/') || /^\.(mp4|mov|m4v|webm)$/.test(value));
    const canSelect = uploadButton && fileCards.length === 0 && !editor
        && fileInput && !fileInput.disabled && !fileInput.multiple && acceptsVideo;
    if (action === 'input') {
        if (!canSelect) return null;
        watchFileSelection(fileInput);
        return fileInput;
    }
    if (action === 'uploadButtonPoint') {
        if (!canSelect) return null;
        uploadButton.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
        const rect = uploadButton.getBoundingClientRect();
        const x = rect.left + rect.width / 2;
        const y = rect.top + rect.height / 2;
        return uploadButton.contains(document.elementFromPoint(x, y)) ? [x, y] : null;
    }
    if (canSelect) return observed('selectFile');

    const preview = one(find(root, 'main#preview-tours'));
    const phone = preview && one(find(preview, '[class*="_phone-item_"]'));
    const phoneFailure = phone && find(phone, '[class*="_phone-fail_"] > span')
        .some((element) => ['上传失败', '合成失败'].includes(text(element)));
    const phoneLabel = phone && one(find(phone, 'span[class*="_phone-label_"]'));
    const legacyStatus = card && one(find(card, '[class*="_upload-status_"]'));
    const legacyLabel = legacyStatus && one(find(legacyStatus, 'span[class*="_upload-status-text_"]'));
    const label = phoneLabel ? text(phoneLabel) : legacyLabel && text(legacyLabel);
    if (phoneFailure || label === '上传失败') return observed('failed');
    if (label === '合成中') return observed('processing');
    if (label === '上传中') {
        const percentage = phoneLabel
            ? one(find(phone, '.ant-progress-text'))
            : one(find(legacyStatus, 'span[class*="_upload-status-percent_"]'));
        const value = percentage && /^(\d{1,3})\s*%$/.exec(text(percentage));
        const percent = value && Number(value[1]);
        return observed('uploading', percent !== null && percent <= 100 ? percent : null);
    }

    // The current video editor renders these tabs only after upload processing succeeds.
    const previewHeader = preview && one(find(preview, 'div[class*="_preview_"] > div[class*="_preview-header_"]'));
    const readyPreview = previewHeader && ['预览封面', '预览作品'].every((tabLabel) =>
        one(find(previewHeader, ':scope > span').filter((element) => text(element) === tabLabel)));
    if (!readyPreview && label !== '上传成功') return observed('waiting');

    const publishButton = one(find(root, '[class*="_edit-section-btns_"] > [class*="_button-primary_"]')
        .filter((element) => text(element) === '发布'));
    if (!editor || !publishButton) return observed('editorMissing');
    if (action === 'fill') {
        editor.focus();
        const range = document.createRange();
        range.selectNodeContents(editor);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        document.execCommand('insertText', false, caption);
    }
    if (action === 'fill' || action === 'verify') {
        return observed(text(editor).replace(/\u00a0/g, ' ') === caption ? 'filled' : 'ready');
    }
    return observed('ready');
}
