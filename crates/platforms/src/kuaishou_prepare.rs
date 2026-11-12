use crate::upload_progress::{UploadProgress, UploadProgressGuard, UploadProgressRegistry};
use anyhow::Result;
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::{
            dom::{DescribeNodeParams, SetFileInputFilesParams},
            page::{
                EventFileChooserOpened, FileChooserOpenedMode, SetInterceptFileChooserDialogParams,
            },
        },
        js_protocol::runtime::EvaluateParams,
    },
    layout::Point,
};
use futures::StreamExt;
use publisher_core::{Content, JobState, RunContext, UserError};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const CREATOR_ORIGIN: &str = "https://cp.kuaishou.com";
const SCRIPT: &str = include_str!("kuaishou_prepare_page.js");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum PageState {
    Waiting,
    SelectFile,
    Uploading,
    Processing,
    Failed,
    EditorMissing,
    Ready,
    Filled,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    status: PageState,
    percent: Option<u8>,
    file_bytes: Option<u64>,
    rejection: Option<String>,
}

pub(crate) fn upload_timeout(bytes: u64) -> Duration {
    Duration::from_secs((600 + bytes.div_ceil(1024 * 1024)).min(2 * 60 * 60))
}

pub(crate) async fn dismiss_publish_guide(page: &Page, origin: &str) -> Result<()> {
    let origin = serde_json::to_string(origin)?;
    let expression = format!(
        r#"(() => {{
            if (location.origin !== {origin} || location.pathname !== '/article/publish/video') return [];
            const dialogs = [...document.querySelectorAll('[id^="react-joyride-step-"] [role="alertdialog"]')]
                .filter(node => node.getClientRects().length > 0 && getComputedStyle(node).visibility === 'visible');
            if (dialogs.length !== 1) return [];
            const controls = [...dialogs[0].querySelectorAll('[data-action="skip"], [data-action="close"]')]
                .filter(node => node.getClientRects().length > 0 && getComputedStyle(node).visibility === 'visible'
                    && !node.closest(':disabled, [disabled], [aria-disabled="true"], [hidden], [inert]'));
            const skip = controls.filter(node => node.dataset.action === 'skip');
            const choices = skip.length ? skip : controls;
            if (choices.length !== 1) return [];
            const button = choices[0];
            const rect = button.getBoundingClientRect();
            const x = rect.left + rect.width / 2;
            const y = rect.top + rect.height / 2;
            return button.contains(document.elementFromPoint(x, y)) ? [x, y] : [];
        }})()"#
    );
    let point = page
        .evaluate_expression(expression)
        .await?
        .into_value::<Vec<f64>>()?;
    if let [x, y] = point.as_slice() {
        page.click(Point::new(*x, *y)).await?;
    }
    Ok(())
}

pub(crate) fn caption(content: &Content) -> Result<String> {
    let title = content.title.trim();
    let description = content.description.trim();
    let mut caption = title.to_owned();
    if !description.is_empty() && description != title {
        caption.push('\n');
        caption.push_str(description);
    }
    if !content.tags.is_empty() {
        caption.push('\n');
        caption.push_str(
            &content
                .tags
                .iter()
                .map(|tag| format!("#{tag}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let caption = caption.replace("\r\n", "\n").replace(['\r', '\u{a0}'], " ");
    if caption.encode_utf16().count() > 500 {
        return Err(
            UserError("快手作品描述最多 500 字，请缩短标题、描述或标签后再准备。".into()).into(),
        );
    }
    Ok(caption)
}

fn expression(origin: &str, action: &str, caption: &str) -> Result<String> {
    let origin = serde_json::to_string(origin)?;
    let action = serde_json::to_string(action)?;
    let caption = serde_json::to_string(caption)?;
    Ok(format!("({SCRIPT})({origin}, {action}, {caption})"))
}

async fn observe(page: &Page, origin: &str, action: &str, caption: &str) -> Result<Observation> {
    Ok(page
        .evaluate_expression(expression(origin, action, caption)?)
        .await?
        .into_value()?)
}

async fn select_video(
    page: &Page,
    origin: &str,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| UserError("无法读取素材路径。".into()))?;
    let mut evaluation = EvaluateParams::new(expression(origin, "input", "")?);
    evaluation.return_by_value = Some(false);
    let result = page.evaluate_expression(evaluation).await?;
    let input = result
        .object()
        .object_id
        .clone()
        .ok_or_else(|| UserError("无法定位快手的视频文件输入框。".into()))?;
    let input = page
        .execute(DescribeNodeParams::builder().object_id(input).build())
        .await?
        .node
        .backend_node_id;
    let mut chooser = page.event_listener::<EventFileChooserOpened>().await?;
    let handoff = async {
        page.execute(SetInterceptFileChooserDialogParams::new(true))
            .await?;
        let point: Option<[f64; 2]> = page
            .evaluate_expression(expression(origin, "uploadButtonPoint", "")?)
            .await?
            .into_value()?;
        let [x, y] = point
            .ok_or_else(|| UserError("快手的上传视频按钮暂时不可操作，请检查页面提示。".into()))?;
        page.click(Point::new(x, y)).await?;
        let event = tokio::time::timeout(Duration::from_secs(8), async {
            let mut ticker = tokio::time::interval(Duration::from_millis(200));
            loop {
                tokio::select! {
                    event = chooser.next() => return event.ok_or_else(|| UserError("快手的文件选择窗口已关闭。".into())),
                    _ = ticker.tick() => {
                        if let Ok(observation) = observe(page, origin, "observe", "").await
                            && let Some(reason) = observation.rejection
                        {
                            return Err(UserError(format!("快手：{reason}")));
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| UserError("快手未打开视频文件选择窗口，请在创作者服务平台查看页面提示。".into()))??;
        if event.mode != FileChooserOpenedMode::SelectSingle || event.backend_node_id != Some(input)
        {
            return Err(UserError("快手打开了与视频上传入口不匹配的文件选择窗口。".into()).into());
        }
        let mut request = SetFileInputFilesParams::new(vec![path.to_owned()]);
        request.backend_node_id = Some(input);
        page.execute(request).await?;
        Ok(())
    };
    let result: Result<()> = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(UserError("已取消快手视频准备。".into()).into()),
        result = handoff => result,
    };
    let cleanup = page
        .execute(SetInterceptFileChooserDialogParams::new(false))
        .await;
    result?;
    cleanup?;
    Ok(())
}

async fn wait_until_selectable(page: &Page, origin: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if matches!(observe(page, origin, "observe", "").await, Ok(state) if state.status == PageState::SelectFile) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| UserError("未找到快手的视频上传入口。请检查账号登录或发布页面。".into()))?;
    Ok(())
}

async fn wait_for_upload(
    page: &Page,
    origin: &str,
    expected_bytes: u64,
    limit: Duration,
    progress: &UploadProgressGuard,
) -> Result<()> {
    let mut last = PageState::Waiting;
    let started = Instant::now();
    let mut editor_wait = None;
    let mut received_file = false;
    let wait = tokio::time::timeout(limit, async {
        loop {
            if let Ok(observation) = observe(page, origin, "observe", "").await {
                if let Some(reason) = observation.rejection {
                    return Err(UserError(format!("快手：{reason}")));
                }
                if let Some(bytes) = observation.file_bytes {
                    if bytes != expected_bytes {
                        return Err(UserError("浏览器读取的视频大小与素材不一致，请检查磁盘连接或重新导入。".into()));
                    }
                    received_file = true;
                }
                match observation.status {
                    PageState::Ready => return Ok(()),
                    PageState::Failed => {
                        return Err(UserError("快手页面提示视频上传失败。".into()));
                    }
                    PageState::Uploading => progress.set(Some(UploadProgress::Uploading {
                        percent: observation.percent,
                    })),
                    PageState::Processing => progress.set(Some(UploadProgress::Processing)),
                    PageState::EditorMissing => {
                        progress.set(Some(UploadProgress::EditorMissing));
                        let since = editor_wait.get_or_insert_with(Instant::now);
                        if since.elapsed() >= Duration::from_secs(60) {
                            return Err(UserError(
                                "快手显示视频上传成功，未找到作品描述编辑区或发布表单。".into(),
                            ));
                        }
                    }
                    _ => {}
                }
                if !matches!(
                    observation.status,
                    PageState::Waiting | PageState::SelectFile
                ) {
                    last = observation.status;
                }
            }
            if last == PageState::Waiting {
                if received_file {
                    progress.set(Some(UploadProgress::ValidatingFile));
                    if started.elapsed() >= Duration::from_secs(75) {
                        return Err(UserError("快手网页已收到视频，但迟迟未开始上传。请重试，或在快手创作者服务平台查看提示。".into()));
                    }
                } else if started.elapsed() >= Duration::from_secs(30) {
                    return Err(UserError("未能确认快手网页收到视频，请检查素材路径和磁盘连接后重试。".into()));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await;
    match wait {
        Ok(result) => Ok(result?),
        Err(_) => Err(UserError(
            match last {
                PageState::EditorMissing => {
                    "快手显示视频上传成功，未找到作品描述编辑区或发布表单。"
                }
                _ => "等待快手上传超时，未能确认视频处理完成。",
            }
            .into(),
        )
        .into()),
    }
}

async fn fill_caption(page: &Page, origin: &str, caption: &str) -> Result<()> {
    let filled = matches!(observe(page, origin, "fill", caption).await, Ok(state) if state.status == PageState::Filled);
    if filled {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if matches!(observe(page, origin, "verify", caption).await, Ok(state) if state.status == PageState::Filled)
        {
            return Ok(());
        }
    }
    Err(UserError("快手已显示上传成功，但未能确认作品描述已填写。".into()).into())
}

pub(crate) async fn prepare_video(
    page: &Page,
    ctx: &RunContext,
    progress: &UploadProgressRegistry,
) -> Result<String> {
    let content = &ctx.job.content;
    let caption = caption(content)?;
    let limit = upload_timeout(content.asset.size);
    let progress = progress.begin(&ctx.job.id);
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err(UserError("已取消快手视频准备。".into()).into()),
        result = wait_until_selectable(page, CREATOR_ORIGIN) => result?,
    }
    content
        .asset
        .verify()
        .map_err(|e| UserError(e.to_string()))?;
    ctx.stage(JobState::Uploading, "正在向快手网页交接视频并等待上传")?;
    select_video(page, CREATOR_ORIGIN, &content.asset.path, &ctx.cancel)
        .await
        .map_err(|error| {
            if error.is::<UserError>() {
                error
            } else {
                UserError("无法把本地视频交给快手，请检查素材和发布页面。".into()).into()
            }
        })?;
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Err(UserError("已取消快手视频准备。".into()).into()),
        result = async {
            wait_for_upload(page, CREATOR_ORIGIN, content.asset.size, limit, &progress).await?;
            content.asset.verify().map_err(|e| UserError(e.to_string()))?;
            progress.set(Some(UploadProgress::FillingForm));
            dismiss_publish_guide(page, CREATOR_ORIGIN).await?;
            fill_caption(page, CREATOR_ORIGIN, &caption).await
        } => result,
    }?;
    Ok(caption)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserSession;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
    async fn selects_local_video_and_prepares_caption_without_publishing() -> Result<()> {
        const HTML: &str = r#"<!doctype html><meta charset="utf-8">
            <div id="root-video-publish"><div id="onvideo_creator_platform">
                <div id="joyride-wrapper">
                    <input type="file" accept="video/*" hidden>
                    <button class="_upload-btn_1j3uy_87">上传视频</button>
                    <div id="upload"></div>
                </div>
            </div></div>
            <input id="other-file" type="file" hidden>
            <div id="microSupport"><div class="ant-message-error">暂时无法上传视频，请稍后再试</div></div>
            <script>
                window.publishClicks = 0;
                window.uploadMode = 'quota';
                window.trustedUploadClicks = 0;
                const input = document.querySelector('input[type=file]');
                const otherInput = document.querySelector('#other-file');
                document.querySelector('button').addEventListener('click', event => {
                    if (!event.isTrusted) return;
                    window.trustedUploadClicks++;
                    if (window.uploadMode === 'quota') {
                        const modal = document.createElement('div');
                        modal.className = 'ant-modal-confirm-content';
                        modal.textContent = '已达今日上传作品上限';
                        document.querySelector('#microSupport').append(modal);
                        return;
                    }
                    if (window.uploadMode === 'other') {
                        otherInput.click();
                    } else {
                        input.click();
                    }
                });
                input.addEventListener('change', () => {
                    if (input.files[0]?.name !== 'fixture.mp4') return;
                    input.value = '';
                    document.querySelector('button').remove();
                    document.querySelector('#upload').innerHTML = `
                        <div class="_edit-file-container_iuuoa_7">
                            <div class="_upload-status_iuuoa_72">
                                <span id="upload-label" class="_upload-status-text_iuuoa_94">上传中</span>
                                <span id="upload-percent" class="_upload-status-percent_iuuoa_97">47%</span>
                            </div>
                        </div>
                        <div id="work-description-edit" contenteditable="true">旧描述</div>
                        <div class="_edit-section-btns_ql0z6_118">
                            <div class="_button-primary_3a3lq_60" onclick="window.publishClicks++">发布</div>
                        </div>`;
                });
            </script>"#;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{HTML}",
                HTML.len()
            );
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 4096];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("fixture.mp4");
        std::fs::write(&path, b"loopback test fixture")?;
        let mut browser = BrowserSession::launch(&fixture.path().join("profile")).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/article/publish/video?tabType=1"))
            .await?;

        let result = async {
            let cancel = CancellationToken::new();
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::SelectFile);
            assert!(select_video(&page, "https://foreign.invalid", &path, &cancel).await.is_err());
            let error = select_video(&page, &origin, &path, &cancel)
                .await.expect_err("发布额度提示必须阻止文件交接");
            assert_eq!(error.to_string(), "快手：已达今日上传作品上限");
            assert!(observe(&page, &origin, "observe", "").await?.file_bytes.is_none());

            page.evaluate_expression("document.querySelector('.ant-modal-confirm-content').remove(); window.uploadMode = 'other';").await?;
            let error = select_video(&page, &origin, &path, &cancel)
                .await.expect_err("文件不得交给其他输入框");
            assert_eq!(error.to_string(), "快手打开了与视频上传入口不匹配的文件选择窗口。");
            assert_eq!(page.evaluate_expression("document.querySelector('#other-file').files.length").await?.into_value::<u32>()?, 0);

            page.evaluate_expression("window.uploadMode = 'video';").await?;
            select_video(&page, &origin, &path, &cancel).await?;
            assert_eq!(page.evaluate_expression("window.trustedUploadClicks").await?.into_value::<u32>()?, 3);
            let observation = observe(&page, &origin, "observe", "").await?;
            assert_eq!(observation.status, PageState::Uploading);
            assert_eq!(observation.percent, Some(47));
            assert_eq!(observation.file_bytes, Some(std::fs::metadata(&path)?.len()));
            assert!(observation.rejection.is_none());

            page.evaluate_expression("document.getElementById('upload-percent').textContent = '100%'").await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Uploading);
            page.evaluate_expression("document.getElementById('upload-label').textContent = '上传成功'; document.getElementById('upload-label').hidden = true;").await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Waiting);
            page.evaluate_expression("document.getElementById('upload-label').hidden = false;").await?;
            let content = Content {
                asset: publisher_core::Asset::inspect(&path)?,
                title: "示例标题".into(),
                description: "示例说明".into(),
                tags: vec!["示例话题".into()],
                douyin_video_covers: Default::default(),
            };
            let caption = caption(&content)?;
            assert_eq!(caption, "示例标题\n示例说明\n#示例话题");
            page.evaluate_expression(r#"
                const guide = document.createElement('div');
                guide.id = 'react-joyride-step-0';
                guide.innerHTML = `<div role="alertdialog" style="position:fixed;inset:0;background:white">
                    <button data-action="skip" onclick="if(event.isTrusted) this.closest('#react-joyride-step-0').remove()">跳过</button>
                </div>`;
                document.body.append(guide);
            "#).await?;
            dismiss_publish_guide(&page, "https://foreign.invalid").await?;
            assert!(crate::browser::truth(&page, "!!document.querySelector('#react-joyride-step-0')").await?);
            dismiss_publish_guide(&page, &origin).await?;
            assert!(crate::browser::truth(&page, "!document.querySelector('#react-joyride-step-0')").await?);
            fill_caption(&page, &origin, &caption).await?;
            assert_eq!(page.evaluate_expression("window.publishClicks").await?.into_value::<u32>()?, 0);

            // The current video page mounts the phone preview in the editor, not the
            // legacy file card. Keep the publish action unclicked in both layouts.
            page.evaluate_expression(r#"
                document.querySelector('[class*="_edit-file-container_"]').remove();
                const preview = document.createElement('main');
                preview.id = 'preview-tours';
                preview.innerHTML = `<section class="_wrapper_1eni7_1">
                    <div class="_phone_1eni7_12"><div id="phone-upload" class="_phone-item_1eni7_42">
                        <div class="ant-progress ant-progress-circle">
                            <span class="ant-progress-text">63%</span>
                        </div>
                        <span id="phone-upload-status" class="_phone-label_1eni7_34">上传中</span>
                    </div></div>
                </section>`;
                document.getElementById('upload').append(preview);
            "#).await?;
            let phone_progress = observe(&page, &origin, "observe", "").await?;
            assert_eq!(phone_progress.status, PageState::Uploading);
            assert_eq!(phone_progress.percent, Some(63));
            assert_eq!(phone_progress.file_bytes, Some(content.asset.size));

            page.evaluate_expression("document.querySelector('.ant-progress-text').textContent = '100%'").await?;
            let phone_progress = observe(&page, &origin, "observe", "").await?;
            assert_eq!(phone_progress.status, PageState::Uploading);
            assert_eq!(phone_progress.percent, Some(100));

            page.evaluate_expression("document.getElementById('phone-upload-status').textContent = '合成中'").await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Processing);
            page.evaluate_expression(r#"
                document.getElementById('phone-upload').innerHTML = `
                    <span class="_phone-fail_1eni7_52"><span>上传失败</span><span>刷新</span></span>`;
            "#).await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Failed);
            page.evaluate_expression("document.querySelector('[class*=_phone-fail_] > span').textContent = '合成失败'").await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Failed);

            page.evaluate_expression(r#"
                document.querySelector('#preview-tours > section').innerHTML = `
                    <div class="_preview_1eni7_87"><div id="preview-tabs" class="_preview-header_1eni7_92">
                        <span class="_tab_1eni7_101">预览封面</span>
                    </div></div>`;
            "#).await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Waiting);
            page.evaluate_expression(r#"
                document.getElementById('preview-tabs').insertAdjacentHTML('beforeend',
                    '<span class="_tab_1eni7_101">预览作品</span>');
                document.getElementById('work-description-edit').hidden = true;
            "#).await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::EditorMissing);
            page.evaluate_expression("document.getElementById('work-description-edit').hidden = false").await?;
            assert_eq!(observe(&page, &origin, "observe", "").await?.status, PageState::Ready);
            fill_caption(&page, &origin, "新版预览准备完成").await?;
            assert_eq!(page.evaluate_expression("window.publishClicks").await?.into_value::<u32>()?, 0);

            page.evaluate_expression(r#"
                const toast = document.createElement('div');
                toast.className = 'ant-message-error';
                toast.textContent = '账号未绑定手机号，请绑定后再上传视频';
                document.querySelector('#microSupport').append(toast);
                setTimeout(() => toast.remove(), 0);
            "#).await?;
            let progress = UploadProgressRegistry::default().begin("fixture");
            let error = wait_for_upload(&page, &origin, content.asset.size, Duration::from_secs(2), &progress)
                .await.expect_err("应保留一闪而过的上传拒绝提示");
            assert_eq!(error.to_string(), "快手：账号未绑定手机号，请绑定后再上传视频");
            Ok::<_, anyhow::Error>(())
        }.await;
        browser.close().await?;
        server.abort();
        result
    }
}
