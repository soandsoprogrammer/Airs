(origin, action = 'observe', caption = '') => {
    const attemptKey = Symbol.for('social-publisher.kuaishou.atlas-upload');
    const tab = new URL(location.href).searchParams.getAll('tabType');
    const onAtlas = location.origin === origin && location.pathname === '/article/publish/video'
        && tab.length === 1 && tab[0] === '2';
    const visible = (element) => element.getClientRects().length > 0
        && getComputedStyle(element).visibility === 'visible'
        && !element.closest('[hidden], [aria-hidden="true"], [inert]');
    const find = (root, selector) => [...root.querySelectorAll(selector)].filter(visible);
    const one = (elements) => elements.length === 1 ? elements[0] : null;
    const text = (element) => element.innerText.trim();
    const host = onAtlas && one([...document.querySelectorAll('#root-video-publish #onvideo_creator_platform')]);
    if (!host || find(document, 'a.login').some((element) => text(element) === '立即登录')) {
        return action === 'input' || action === 'uploadButtonPoint' ? null : {status: 'waiting'};
    }

    const messages = () => find(document,
        '#microSupport .ant-message-error, #microSupport .ant-modal-confirm-content');
    const uploadError = (message) => /上传图片失败|图片大小超过|无效文件类型|文件读取失败|上传失败|已达今日上传作品上限|暂时无法上传/.test(message);
    const watchFiles = (input) => {
        window[attemptKey]?.observer.disconnect();
        const seen = new Map(messages().map((node) => [node, text(node)]));
        const attempt = {files: null, rejected: false};
        const rememberFiles = () => {
            if (input.files?.length) {
                attempt.files = [...input.files].map(({name, size}) => ({name, size}));
            }
        };
        input.addEventListener('input', rememberFiles, {once: true, capture: true});
        input.addEventListener('change', rememberFiles, {once: true, capture: true});
        attempt.observer = new MutationObserver(() => {
            attempt.rejected ||= messages().some((node) => {
                const message = text(node);
                return seen.get(node) !== message && uploadError(message);
            });
        });
        attempt.observer.observe(document.body, {childList: true, subtree: true, characterData: true});
        window[attemptKey] = attempt;
    };
    const result = (status, data = {}) => ({
        status, files: window[attemptKey]?.files, ...data,
    });
    const section = one(find(host, 'section[class*="_upload-container_"]'));
    const button = section && one(find(section, 'button[class*="_upload-btn_"]')
        .filter((element) => text(element) === '上传图片' && !element.disabled));
    const input = section && one([...section.parentElement.querySelectorAll(':scope > input[type="file"][multiple]')]);
    const canSelect = button && input && !input.disabled &&
        input.accept.split(',').some((type) => /^image\/(png|jpe?g|webp)$/.test(type.trim()));
    if (action === 'input') {
        if (!canSelect) return null;
        watchFiles(input);
        return input;
    }
    if (action === 'uploadButtonPoint') {
        if (!canSelect) return null;
        button.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
        const rect = button.getBoundingClientRect();
        const x = rect.left + rect.width / 2;
        const y = rect.top + rect.height / 2;
        return button.contains(document.elementFromPoint(x, y)) ? [x, y] : null;
    }
    if (window[attemptKey]?.rejected) return result('failed');
    if (canSelect) return result('selectFile');

    // In the atlas editor, all uploaded images remain in the sortable strip,
    // including items hidden behind the expand control.
    const composer = one(find(host, '[class*="_section_"]').filter((element) =>
        one(find(element, ':scope > [class*="_section-header_"]')
            .filter((header) => text(header) === '发布图文'))));
    if (!composer) return result('waiting');
    const preview = one(find(composer, 'main > section[class*="_wrapper_"]'));
    if (!preview) return result('waiting');
    if (find(preview, '[class*="_phone-fail_"] > span').some((node) => text(node) === '上传失败')) {
        return result('failed');
    }
    const progress = one(find(preview, '[class*="_phone-label_"]'));
    const count = progress && /^上传中\s*(\d+)\s*\/\s*(\d+)$/.exec(text(progress));
    if (count) {
        const done = Number(count[1]);
        const total = Number(count[2]);
        const percent = total > 0 && done <= total ? Math.floor(100 * done / total) : null;
        return result('uploading', {percent});
    }
    const readyHeader = one(find(preview, '[class*="_preview_"] > [class*="_preview-header_"]'));
    const ready = readyHeader && ['预览封面', '预览作品'].every((label) =>
        one(find(readyHeader, ':scope > span').filter((node) => text(node) === label)));
    const editor = one(find(composer, '#work-description-edit[contenteditable="true"]'));
    const strip = one(find(composer, '[class*="_upload-main_"]'));
    if (!ready || !editor || !strip) return result('waiting');
    const images = [...strip.querySelectorAll('[class*="_upload-item_"] > img')]
        .map((image) => image.src);
    if (!images.length || images.some((url) => !url.startsWith('https://'))) return result('waiting');
    if (action === 'fill') {
        editor.focus();
        const range = document.createRange();
        range.selectNodeContents(editor);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        document.execCommand('insertText', false, caption);
    }
    return result(text(editor).replace(/\u00a0/g, ' ') === caption ? 'filled' : 'ready', {images});
}
