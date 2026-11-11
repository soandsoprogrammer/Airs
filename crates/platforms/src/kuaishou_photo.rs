use crate::kuaishou_prepare::{caption, dismiss_publish_guide, upload_timeout};
use crate::kuaishou_receipt::{AtlasIdentity, AtlasUploadObserver};
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
use publisher_core::{Asset, JobState, RunContext, UserError};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const CREATOR_ORIGIN: &str = "https://cp.kuaishou.com";
pub(crate) const PHOTO_PROBE: &str = include_str!("kuaishou_photo_page.js");

pub(super) struct PreparedAtlas {
    pub(super) identity: AtlasIdentity,
    pub(super) images: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum PageState {
    Waiting,
    SelectFile,
    Uploading,
    Ready,
    Filled,
    Failed,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct BrowserFile {
    name: String,
    size: u64,
}

#[derive(Deserialize)]
struct Observation {
    status: PageState,
    files: Option<Vec<BrowserFile>>,
    percent: Option<u8>,
    images: Option<Vec<String>>,
}

fn expression(origin: &str, action: &str, caption: &str) -> String {
    let args = serde_json::json!([origin, action, caption]);
    format!("({PHOTO_PROBE})(...{args})")
}

async fn observe(page: &Page, origin: &str, action: &str, caption: &str) -> Result<Observation> {
    Ok(page
        .evaluate_expression(expression(origin, action, caption))
        .await?
        .into_value()?)
}

async fn wait_for_upload_entry(page: &Page, origin: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if matches!(observe(page, origin, "observe", "").await, Ok(state) if state.status == PageState::SelectFile) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| UserError("未找到快手的「上传图片」入口。请在快手创作者服务平台确认当前账号可发布图文后再试。".into()))?;
    Ok(())
}

async fn select_photos(
    page: &Page,
    origin: &str,
    files: Vec<String>,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut evaluation = EvaluateParams::new(expression(origin, "input", ""));
    evaluation.return_by_value = Some(false);
    let result = page.evaluate_expression(evaluation).await?;
    let input = result
        .object()
        .object_id
        .clone()
        .ok_or_else(|| UserError("无法定位快手的图片文件输入框。".into()))?;
    let input = page
        .execute(DescribeNodeParams::builder().object_id(input).build())
        .await?
        .node
        .backend_node_id;
    let mut chooser = page.event_listener::<EventFileChooserOpened>().await?;
    page.execute(SetInterceptFileChooserDialogParams::new(true))
        .await?;
    let handoff = async {
        let point: Option<[f64; 2]> = page
            .evaluate_expression(expression(origin, "uploadButtonPoint", ""))
            .await?
            .into_value()?;
        let [x, y] = point
            .ok_or_else(|| UserError("快手的上传图片按钮暂时不可操作，请检查页面提示。".into()))?;
        page.click(Point::new(x, y)).await?;
        let event = tokio::time::timeout(Duration::from_secs(8), chooser.next())
            .await
            .ok()
            .flatten()
            .ok_or_else(|| {
                UserError("快手未打开图片选择窗口，请检查当前账号的发布权限或上传额度。".into())
            })?;
        if event.mode != FileChooserOpenedMode::SelectMultiple
            || event.backend_node_id != Some(input)
        {
            return Err(UserError("快手打开了与图文上传入口不匹配的文件选择窗口。".into()).into());
        }
        let mut request = SetFileInputFilesParams::new(files);
        request.backend_node_id = Some(input);
        page.execute(request).await?;
        Ok(())
    };
    let result: Result<()> = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(UserError("已取消快手图文准备。".into()).into()),
        result = handoff => result,
    };
    let cleanup = page
        .execute(SetInterceptFileChooserDialogParams::new(false))
        .await;
    result?;
    cleanup?;
    Ok(())
}

fn image_files(asset: &Asset) -> Result<(Vec<String>, Vec<BrowserFile>)> {
    let mut paths = Vec::with_capacity(asset.images.len());
    let mut expected = Vec::with_capacity(asset.images.len());
    for file in &asset.images {
        let name = file
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| UserError("快手无法读取图片文件名。".into()))?;
        let path = file
            .path
            .to_str()
            .ok_or_else(|| UserError("快手无法读取图片路径。".into()))?;
        paths.push(path.to_owned());
        expected.push(BrowserFile {
            name: name.to_owned(),
            size: file.size,
        });
    }
    Ok((paths, expected))
}

async fn wait_for_images(
    page: &Page,
    origin: &str,
    expected: &[BrowserFile],
    progress: &UploadProgressGuard,
) -> Result<Vec<String>> {
    let mut started = Instant::now();
    let mut last_images = None;
    let mut received = false;
    loop {
        if let Ok(observation) = observe(page, origin, "observe", "").await {
            if let Some(files) = observation.files {
                if files != expected {
                    return Err(UserError(
                        "快手读取的图片或顺序与任务素材不一致，请重新准备。".into(),
                    )
                    .into());
                }
                if !received {
                    received = true;
                    started = Instant::now();
                }
            }
            match observation.status {
                PageState::Failed => {
                    return Err(UserError(
                        "快手拒绝或未能上传图片，请检查图片格式、网络和账号发布权限。".into(),
                    )
                    .into());
                }
                PageState::Uploading => progress.set(Some(UploadProgress::Uploading {
                    percent: observation.percent,
                })),
                PageState::Ready | PageState::Filled if received => {
                    if let Some(images) = observation.images {
                        if last_images.as_ref() == Some(&images) {
                            if images.len() != expected.len() {
                                return Err(UserError(format!(
                                    "快手只上传了 {} / {} 张图片，请检查图片后重新准备。",
                                    images.len(),
                                    expected.len()
                                ))
                                .into());
                            }
                            return Ok(images);
                        }
                        last_images = Some(images);
                    }
                }
                _ => {}
            }
        }
        if !received && started.elapsed() >= Duration::from_secs(30) {
            return Err(
                UserError("未能确认快手网页已收到本地图片，请检查文件后重试。".into()).into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn fill_caption(page: &Page, origin: &str, caption: &str, images: &[String]) -> Result<()> {
    let matches = |state: Observation| {
        state.status == PageState::Filled && state.images.as_deref() == Some(images)
    };
    if observe(page, origin, "fill", caption)
        .await
        .is_ok_and(matches)
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if observe(page, origin, "verify", caption)
            .await
            .is_ok_and(matches)
        {
            return Ok(());
        }
    }
    Err(UserError("快手图片上传完成，但未能确认作品描述和图片顺序。".into()).into())
}

pub(super) async fn prepare_photos(
    page: &Page,
    ctx: &RunContext,
    progress: &UploadProgressRegistry,
) -> Result<(String, PreparedAtlas)> {
    let content = &ctx.job.content;
    let caption = caption(content)?;
    let (paths, files) = image_files(&content.asset)?;
    let progress = progress.begin(&ctx.job.id);
    let preparation = async {
        wait_for_upload_entry(page, CREATOR_ORIGIN).await?;
        content
            .asset
            .verify()
            .map_err(|error| UserError(error.to_string()))?;
        let observer = AtlasUploadObserver::start(page)
            .await
            .map_err(|_| UserError("无法确认快手的图文上传页面，请重新准备。".into()))?;
        ctx.stage(JobState::Uploading, "正在上传快手图文图片")?;
        select_photos(page, CREATOR_ORIGIN, paths, &ctx.cancel)
            .await
            .map_err(|error| {
                if error.is::<UserError>() {
                    error
                } else {
                    UserError("无法把本地图片交给快手，请检查素材和发布页面。".into()).into()
                }
            })?;
        let observe_upload = async {
            let identity =
                tokio::time::timeout(Duration::from_secs(60), observer.wait(page, files.len()))
                    .await
                    .ok()
                    .flatten()
                    .ok_or_else(|| {
                        UserError(
                            "快手未确认创建本次图片上传，请检查账号发布权限或网络后重试。".into(),
                        )
                    })?;
            Ok::<_, anyhow::Error>(identity)
        };
        let observe_images = wait_for_images(page, CREATOR_ORIGIN, &files, &progress);
        let (identity, images) = tokio::time::timeout(upload_timeout(content.asset.size), async {
            tokio::try_join!(observe_upload, observe_images)
        })
        .await
        .map_err(|_| UserError("等待快手处理图片超时，请检查网络后重新准备。".into()))??;
        content
            .asset
            .verify()
            .map_err(|error| UserError(error.to_string()))?;
        progress.set(Some(UploadProgress::FillingForm));
        dismiss_publish_guide(page, CREATOR_ORIGIN).await?;
        fill_caption(page, CREATOR_ORIGIN, &caption, &images).await?;
        Ok((caption, PreparedAtlas { identity, images }))
    };
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Err(UserError("已取消快手图文准备。".into()).into()),
        result = preparation => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{browser::BrowserSession, kuaishou_submit::click_publish_at_origin};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
    async fn ordered_photos_wait_for_upload_and_require_unchanged_public_composer() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 15\r\nConnection: close\r\n\r\n<!doctype html>";
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 4096];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(response).await;
                }
            }
        });
        let fixture = tempfile::tempdir()?;
        let first = fixture.path().join("b-first.png");
        let second = fixture.path().join("a-second.jpg");
        std::fs::write(&first, b"first fixture")?;
        std::fs::write(&second, b"second fixture")?;
        let asset = Asset::inspect_images(&[first, second])?;
        let (paths, files) = image_files(&asset)?;
        let mut browser = BrowserSession::launch(&fixture.path().join("profile")).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/article/publish/video?tabType=2"))
            .await?;
        page.set_content(r#"<meta charset="utf-8">
            <header class="header-bar"><div class="user-info-dpd">
              <div class="user-info-avatar">头像</div><div class="user-info-name">创作者</div>
            </div></header>
            <div id="root-video-publish"><div id="onvideo_creator_platform">
              <div id="form">
                <input type="file" hidden multiple accept="image/png,image/jpeg,image/webp">
                <section class="_upload-container_ysbff_12">
                  <button class="_upload-btn_ysbff_57">上传图片</button>
                </section>
              </div>
            </div></div>
            <input id="wrong-file" type="file" multiple hidden>
            <script>
              window.sendToWrongInput = true;
              window.publishClicks = 0;
              window.finishUpload = (count) => {
                document.querySelector('#preview').innerHTML = `
                  <div class="_preview_lgo7r_83"><div class="_preview-header_lgo7r_88">
                    <span>预览封面</span><span>预览作品</span>
                  </div></div>`;
                document.querySelector('#strip').innerHTML = ['b-first.png', 'a-second.jpg']
                  .slice(0, count).map((name, index) =>
                    `<div class="_upload-item_swpvr_60" style="${index ? 'display:none' : ''}">
                        <img alt="" src="https://127.0.0.1:1/${name}"></div>`).join('');
              };
              const input = document.querySelector('#form > input');
              document.querySelector('button').onclick = (event) => {
                if (event.isTrusted) {
                  (window.sendToWrongInput ? document.querySelector('#wrong-file') : input).click();
                }
              };
              input.onchange = () => {
                input.value = '';
                document.querySelector('#form').innerHTML = `
                  <div class="_section_y5j5i_1"><div class="_section-header_y5j5i_6">发布图文</div>
                    <main><section id="preview" class="_wrapper_lgo7r_1">
                      <span class="_phone-label_lgo7r_32">上传中 2/2</span>
                    </section></main>
                    <div id="work-description-edit" contenteditable="true">旧描述</div>
                    <div id="strip" class="_upload-main_swpvr_19"></div>
                    <div class="ant-radio-group">
                      <label class="ant-radio-wrapper">
                        <input class="ant-radio-input" type="radio" name="view" checked>公开 <span>（所有人可见）</span>
                      </label>
                      <label class="ant-radio-wrapper">
                        <input id="private" class="ant-radio-input" type="radio" name="view">私密 <span>（仅自己可见）</span>
                      </label>
                    </div>
                    <div class="_section-form-btns_y5j5i_21">
                      <button class="_button-primary_3a3lq_60"
                        onclick="if(event.isTrusted) window.publishClicks++">发布</button>
                    </div>
                  </div>`;
              };
            </script>"#).await?;
        let result = async {
            assert_eq!(
                observe(&page, &origin, "observe", "").await?.status,
                PageState::SelectFile
            );
            let cancel = CancellationToken::new();
            assert!(
                select_photos(&page, &origin, paths.clone(), &cancel)
                    .await
                    .is_err()
            );
            page.evaluate_expression("window.sendToWrongInput = false")
                .await?;
            select_photos(&page, &origin, paths, &cancel).await?;
            let observed = observe(&page, &origin, "observe", "").await?;
            assert_eq!(observed.files.as_deref(), Some(files.as_slice()));
            assert_eq!(observed.status, PageState::Uploading);
            assert_eq!(observed.percent, Some(100));
            let registry = UploadProgressRegistry::default();
            let progress = registry.begin("atlas-fixture");
            page.evaluate_expression("window.finishUpload(1)").await?;
            let partial = wait_for_images(&page, &origin, &files, &progress)
                .await
                .expect_err("partial upload must not become ready");
            assert!(partial.to_string().contains("1 / 2"));
            page.evaluate_expression("window.finishUpload(2)").await?;
            let images = wait_for_images(&page, &origin, &files, &progress).await?;
            assert!(images[0].ends_with("/b-first.png") && images[1].ends_with("/a-second.jpg"));
            fill_caption(&page, &origin, "本地标题\n本地描述", &images).await?;
            assert!(crate::browser::truth(&page, "window.publishClicks === 0").await?);

            let publish = |expected| {
                click_publish_at_origin(
                    &page,
                    &origin,
                    "创作者",
                    "本地标题\n本地描述",
                    Some(expected),
                )
            };
            let reordered = vec![images[1].clone(), images[0].clone()];
            assert!(publish(&reordered).await.is_err());
            page.evaluate_expression("document.querySelector('#private').checked = true")
                .await?;
            assert!(publish(&images).await.is_err());
            page.evaluate_expression("document.querySelector('input[name=view]').checked = true")
                .await?;
            publish(&images).await?;
            assert!(crate::browser::truth(&page, "window.publishClicks === 1").await?);
            Ok(())
        }
        .await;
        browser.close().await?;
        server.abort();
        result
    }
}
