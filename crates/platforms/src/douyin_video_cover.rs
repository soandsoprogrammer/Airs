use anyhow::Result;
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::dom::{DescribeNodeParams, SetFileInputFilesParams},
        js_protocol::runtime::EvaluateParams,
    },
    layout::Point,
};
use publisher_core::{DouyinVideoCovers, ImageFile, UserError};
use std::time::Duration;
use tokio::time::{Instant, sleep};

const COVER_PAGE: &str = include_str!("douyin_video_cover.js");
const CONTROL_WAIT: Duration = Duration::from_secs(12);
const UPLOAD_WAIT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const CONTROL_RETRY_INTERVAL: Duration = Duration::from_millis(1_500);
const CONTROL_CLICK_LIMIT: u8 = 3;

#[derive(Clone, Copy)]
enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    fn key(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Landscape => "landscape",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Portrait => "竖版",
            Self::Landscape => "横版",
        }
    }

    fn error(self, reason: &str) -> anyhow::Error {
        UserError(format!("抖音{}封面{reason}", self.label())).into()
    }
}

#[derive(Default)]
pub(super) struct VideoCoverSnapshot {
    portrait: Option<String>,
    landscape: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum Observation {
    Open,
    Active,
    Closed,
    Waiting,
    Rejected {
        reason: Rejection,
    },
    Hover {
        point: (f64, f64),
    },
    AdjustCrop {
        point: (f64, f64),
    },
    Point {
        point: (f64, f64),
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default, rename = "directionBound")]
        direction_bound: bool,
    },
    Sources {
        sources: Vec<String>,
    },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum Rejection {
    Page,
    MultipleEditors,
    OtherDialog,
    CoverSuggestion,
    MultipleUploaders,
    MultipleCropSelectors,
    Resolution,
    Format,
    PlatformAlert,
    #[serde(other)]
    Unknown,
}

fn expression(
    origin: &str,
    action: &str,
    orientation: Orientation,
    baseline: &[String],
    direction_bound: bool,
) -> String {
    let args = serde_json::json!([origin, action, orientation.key(), baseline, direction_bound]);
    format!("({COVER_PAGE})(...{args})")
}

async fn observe(
    page: &Page,
    origin: &str,
    action: &str,
    orientation: Orientation,
    baseline: &[String],
    direction_bound: bool,
) -> Result<Observation> {
    let observation = page
        .evaluate_expression(expression(
            origin,
            action,
            orientation,
            baseline,
            direction_bound,
        ))
        .await
        .and_then(|value| value.into_value::<Observation>().map_err(Into::into))
        .map_err(|_| orientation.error("页面无法核对，请重新准备作品。"))?;
    if let Observation::Rejected { reason } = observation {
        let stage = match action {
            "open" => "打开编辑器",
            "tab" => "切换方向",
            "upload-pane" => "查找上传控件",
            "preview" => "读取上传前预览",
            "finish" => "处理已选图片",
            "confirm" => "确认保存",
            _ => "核对发布页预览",
        };
        let detail = match reason {
            Rejection::Page => "发现页面已离开支持的抖音视频编辑地址",
            Rejection::MultipleEditors => "检测到多个编辑器，无法确定当前封面",
            Rejection::OtherDialog => "发现另一个弹窗挡住了编辑器",
            Rejection::CoverSuggestion => "无法核对抖音的横竖封面建议弹窗",
            Rejection::MultipleUploaders => "找到多个上传控件，无法确定图片会上传到哪里",
            Rejection::MultipleCropSelectors => "找到多个裁剪方向控件，无法确认封面方向",
            Rejection::Resolution => "收到抖音提示：图片分辨率过低",
            Rejection::Format => "收到抖音提示：图片格式不支持",
            Rejection::PlatformAlert => "收到抖音的封面上传或处理错误提示",
            Rejection::Unknown => "无法确认抖音页面状态",
        };
        return Err(orientation.error(&format!("在{stage}时{detail}。")));
    }
    Ok(observation)
}

async fn click(page: &Page, orientation: Orientation, (x, y): (f64, f64)) -> Result<()> {
    page.click(Point { x, y })
        .await
        .map_err(|_| orientation.error("控件点击失败，请重新准备作品。"))?;
    Ok(())
}

async fn activate_control(
    page: &Page,
    origin: &str,
    orientation: Orientation,
    action: &str,
    direction_bound: bool,
) -> Result<bool> {
    let mut clicks = 0;
    let mut retry_at = Instant::now();
    let mut hover_point = None;
    let mut clicked_direction = false;
    let activation = async {
        loop {
            match observe(page, origin, action, orientation, &[], direction_bound).await? {
                Observation::Open | Observation::Active => return Ok(clicked_direction),
                Observation::Point {
                    point,
                    direction_bound,
                    ..
                } if clicks < CONTROL_CLICK_LIMIT && Instant::now() >= retry_at => {
                    click(page, orientation, point).await?;
                    clicked_direction = direction_bound;
                    clicks += 1;
                    retry_at = Instant::now() + CONTROL_RETRY_INTERVAL;
                    hover_point = None;
                }
                Observation::Hover { point } if hover_point != Some(point) => {
                    page.move_mouse(Point {
                        x: point.0,
                        y: point.1,
                    })
                    .await
                    .map_err(|_| orientation.error("无法打开编辑控件，请重新准备作品。"))?;
                    hover_point = Some(point);
                }
                _ => {}
            }
            sleep(POLL_INTERVAL).await;
        }
    };
    tokio::time::timeout(CONTROL_WAIT, activation)
        .await
        .map_err(|_| {
            let reason = match (action, clicks) {
                ("open", 0) => "的编辑入口不可用，请到抖音创作者中心核对。",
                ("open", _) => "的编辑入口已点击，但抖音没有打开编辑器。",
                ("upload-pane", _) => "编辑器中没有可用的「上传封面」入口。",
                _ => "编辑器已打开，但未能切换到对应的封面方向。",
            };
            orientation.error(reason)
        })?
}

async fn wait_for_sources(
    page: &Page,
    origin: &str,
    orientation: Orientation,
    action: &str,
    direction_bound: bool,
) -> Result<Vec<String>> {
    let observation = async {
        loop {
            if let Observation::Sources { sources } =
                observe(page, origin, action, orientation, &[], direction_bound).await?
            {
                return Ok(sources);
            }
            sleep(POLL_INTERVAL).await;
        }
    };
    tokio::time::timeout(CONTROL_WAIT, observation)
        .await
        .map_err(|_| orientation.error("预览未就绪，请重新准备作品。"))?
}

async fn upload(
    page: &Page,
    origin: &str,
    orientation: Orientation,
    image: &ImageFile,
    direction_bound: bool,
) -> Result<()> {
    image
        .verify()
        .map_err(|_| orientation.error("本地文件已变化或无法读取，请重新选择封面。"))?;
    let path = image
        .path
        .to_str()
        .ok_or_else(|| orientation.error("文件路径无法读取，请重新选择封面。"))?;
    let failure = || orientation.error("的上传控件不可用，请到创作者中心核对。");
    let mut request = EvaluateParams::new(expression(
        origin,
        "input",
        orientation,
        &[],
        direction_bound,
    ));
    request.return_by_value = Some(false);
    let result = page
        .evaluate_expression(request)
        .await
        .map_err(|_| failure())?;
    let input = result.object().object_id.clone().ok_or_else(failure)?;
    let node = page
        .execute(DescribeNodeParams::builder().object_id(input).build())
        .await
        .map_err(|_| failure())?
        .result
        .node;
    let mut params = SetFileInputFilesParams::new(vec![path.to_owned()]);
    params.backend_node_id = Some(node.backend_node_id);
    page.execute(params)
        .await
        .map_err(|_| orientation.error("无法交给抖音上传，请重新准备作品。"))?;
    Ok(())
}

async fn apply_one(
    page: &Page,
    origin: &str,
    orientation: Orientation,
    image: &ImageFile,
) -> Result<String> {
    let existing = wait_for_sources(page, origin, orientation, "composer", false).await?;
    let direction_bound = activate_control(page, origin, orientation, "open", false).await?;
    activate_control(page, origin, orientation, "tab", direction_bound).await?;
    activate_control(page, origin, orientation, "upload-pane", direction_bound).await?;
    let preview = wait_for_sources(page, origin, orientation, "preview", direction_bound).await?;
    upload(page, origin, orientation, image, direction_bound).await?;

    let upload_finished = async {
        let mut adjusted_crop = false;
        loop {
            match observe(
                page,
                origin,
                "finish",
                orientation,
                &preview,
                direction_bound,
            )
            .await?
            {
                Observation::AdjustCrop { point } if !adjusted_crop => {
                    click(page, orientation, point).await?;
                    adjusted_crop = true;
                }
                Observation::Point { point, sources, .. } => {
                    click(page, orientation, point).await?;
                    return Ok::<_, anyhow::Error>(sources);
                }
                _ => {}
            }
            sleep(POLL_INTERVAL).await;
        }
    };
    let uploaded_sources = tokio::time::timeout(UPLOAD_WAIT, upload_finished)
        .await
        .map_err(|_| orientation.error("未显示已上传的图片，请检查封面格式后重新准备。"))??;

    let deadline = Instant::now() + CONTROL_WAIT;
    let mut confirmed = false;
    while Instant::now() < deadline {
        match observe(page, origin, "confirm", orientation, &[], direction_bound).await? {
            Observation::Point { point, .. } if !confirmed => {
                click(page, orientation, point).await?;
                confirmed = true;
            }
            Observation::Closed => {
                if let Observation::Sources { sources } =
                    observe(page, origin, "composer", orientation, &[], false).await?
                {
                    let changes = sources
                        .into_iter()
                        .filter(|source| !existing.contains(source))
                        .collect::<Vec<_>>();
                    let mut matching = changes
                        .iter()
                        .filter(|source| uploaded_sources.contains(source));
                    if let Some(applied) = matching.next() {
                        if matching.next().is_none() {
                            return Ok(applied.clone());
                        }
                    } else if let [applied] = changes.as_slice() {
                        // Finishing a crop can upload a second resource. Require
                        // the edited preview and an unambiguous saved cover for
                        // this orientation even when their URIs differ.
                        return Ok(applied.clone());
                    }
                }
            }
            _ => {}
        }
        sleep(POLL_INTERVAL).await;
    }
    Err(orientation.error("未能确认已应用，发布已停在提交前。"))
}

pub(super) async fn apply(
    page: &Page,
    origin: &str,
    covers: &DouyinVideoCovers,
) -> Result<VideoCoverSnapshot> {
    let mut snapshot = VideoCoverSnapshot::default();
    if let Some(portrait) = &covers.portrait {
        snapshot.portrait = Some(apply_one(page, origin, Orientation::Portrait, portrait).await?);
    }
    if let Some(landscape) = &covers.landscape {
        snapshot.landscape =
            Some(apply_one(page, origin, Orientation::Landscape, landscape).await?);
    }
    verify(page, origin, &snapshot).await?;
    Ok(snapshot)
}

pub(super) async fn verify(page: &Page, origin: &str, snapshot: &VideoCoverSnapshot) -> Result<()> {
    for (orientation, expected) in [
        (Orientation::Portrait, &snapshot.portrait),
        (Orientation::Landscape, &snapshot.landscape),
    ] {
        if let Some(expected) = expected {
            let actual = observe(page, origin, "composer", orientation, &[], false).await?;
            if !matches!(actual, Observation::Sources { sources } if sources.contains(expected)) {
                return Err(orientation.error("未能再次核对，请重新准备作品。"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{BrowserSession, truth};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn both_cover_orientations_use_the_actual_upload_and_recheck_saved_preview() -> Result<()>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((mut connection, _)) = listener.accept().await {
                let mut request = [0_u8; 2048];
                if connection.read(&mut request).await.is_ok() {
                    let _ = connection
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                }
            }
        });
        let directory = tempfile::tempdir()?;
        let portrait = directory.path().join("portrait.png");
        let landscape = directory.path().join("landscape.png");
        let image = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jXioAAAAASUVORK5CYII=")?;
        std::fs::write(&portrait, &image)?;
        std::fs::write(&landscape, &image)?;
        let mut browser = BrowserSession::launch(directory.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/creator-micro/content/post/video"))
            .await?;
        page.set_content(r#"
            <meta charset="utf-8">
            <style>
              .coverControls-fixture { display:flex; gap:16px; }
              .cover-fixture { position:relative; width:72px; height:96px; cursor:pointer; }
              .landscape-cover-fixture { width:112px; height:84px; }
              .background-fixture { position:absolute; inset:0; background-size:cover; background-color:#ddd; }
              .edit-fixture { display:none; position:absolute; inset:auto 0 0; }
              .cover-fixture:hover .edit-fixture { display:block; }
              .container-fixture { position:relative; }
              .upload-fixture { position:absolute; inset:0; opacity:0; }
            </style>
            <div class="coverControls-fixture">
              <div class="coverControl-fixture">
                <div class="cover-fixture" id="portrait-card">
                  <div class="background-fixture" id="portrait" aria-hidden="true"></div>
                  <span class="edit-fixture">编辑封面</span>
                </div>
                <span class="cover-tip-fixture">竖封面3:4</span>
              </div>
              <div class="coverControl-fixture">
                <div class="cover-fixture landscape-cover-fixture" id="landscape-card">
                  <div class="background-fixture" id="landscape"></div>
                  <span class="edit-fixture">编辑封面</span>
                </div>
                <span class="cover-tip-fixture">横封面4:3</span>
              </div>
            </div>
            <section class="dy-creator-content-modal" role="dialog" id="editor" hidden>
              <div class="steps-fixture">
                <div class="step-fixture step-active-fixture" id="portrait-tab"><span>设置竖封面</span></div>
                <div class="step-fixture" id="landscape-tab"><span>设置横封面</span></div>
              </div>
              <div class="semi-upload">
                <span>生成参考图</span>
                <input class="semi-upload-hidden-input" id="reference" type="file" hidden>
              </div>
              <div class="coverSelect-fixture">
                <div role="tab" id="frames-tab">选取封面</div>
                <div role="tab" id="upload-tab">上传封面</div>
                <div class="container-fixture" id="upload-pane" hidden>
                  <div class="upload-tips-fixture">上传封面</div>
                  <div class="upload-fixture semi-upload">
                    <span class="semi-upload-drag-area-main-text">点击上传文件或拖拽到此处</span>
                    <input class="semi-upload-hidden-input" id="upload" type="file" hidden>
                    <img data-cover-preview id="preview" width="60" height="60">
                    <canvas id="vertical_coverCanvas" width="64" height="88"></canvas>
                  </div>
                  <div class="bg-fixture" id="uploaded-background" style="width:60px;height:60px"></div>
                </div>
              </div>
              <button id="footer-switch">设置横封面</button>
              <button id="done" disabled>完成</button>
            </section>
            <section class="dy-creator-content-modal" role="dialog" id="standalone" hidden>
              <div class="coverSelect-standalone">
                <div class="tabItem-fixture" id="standalone-tab">上传封面</div>
                <div class="semi-upload" id="standalone-upload" hidden>
                  <span class="semi-upload-drag-area-main-text">点击上传或将图片拖拽到此处</span>
                  <input class="semi-upload-hidden-input" id="standalone-input" type="file" hidden>
                </div>
                <div class="uploadCrop-fixture" id="standalone-crop" hidden>
                  <div class="directionSelect-fixture">
                    <div class="selectItem-fixture" id="standalone-portrait">竖封面</div>
                    <div class="selectItem-fixture selected-fixture" id="standalone-landscape">横封面</div>
                  </div>
                  <canvas id="standalone-preview" width="64" height="88"></canvas>
                  <button id="standalone-done" disabled>完成</button>
                </div>
              </div>
            </section>
            <section class="semi-modal-content" role="alertdialog" id="confirm" hidden>
              确定完成？<button id="confirm-button">确定</button>
            </section>
            <section class="dy-creator-content-modal-confirm" id="suggest" hidden>
              <div class="dy-creator-content-modal">
                <div class="dy-creator-content-modal-content verticalSupoortDualCoverModal-fixture" role="dialog" aria-modal="true">
                  <p class="title-fixture">设置横封面获更多流量</p>
                  <button id="skip-suggestion">暂不设置</button>
                  <button id="suggestion-switch">设置横封面</button>
                </div>
              </div>
            </section>
            <div id="platform-error" role="alert" hidden>图片分辨率过低</div>
            <button id="publish">发布</button>
            <script>
              const get = (id) => document.getElementById(id);
              window.uploads = [];
              window.referenceUploads = 0;
              window.publishClicks = 0;
              window.openClicks = 0;
              window.openedCards = [];
              window.uploadTabClicks = 0;
              window.directionClicks = [];
              window.skippedSuggestions = 0;
              window.useStandalone = false;
              window.standaloneSaved = false;
              window.cropDirection = 'landscape';
              window.footerSwitchClicks = 0;
              window.saved = [];
              let current = 'portrait';
              let pending = null;
              const select = (orientation) => {
                current = orientation;
                pending = null;
                get('done').disabled = true;
                get('upload').value = '';
                get('preview').removeAttribute('src');
                get('uploaded-background').style.backgroundImage = 'none';
                get('preview').hidden = orientation === 'portrait';
                get('vertical_coverCanvas').hidden = orientation !== 'portrait';
                get('upload-pane').hidden = true;
                for (const item of ['portrait', 'landscape']) {
                  get(item + '-tab').classList.toggle('step-active-fixture', item === orientation);
                }
              };
              for (const card of document.querySelectorAll('.cover-fixture')) {
                card.addEventListener('click', (event) => {
                  // The creator page can ignore its first click while it renders.
                  if (!event.isTrusted || ++window.openClicks === 1) return;
                  window.openedCards.push(card.id);
                  if (window.useStandalone) {
                    get('standalone').hidden = false;
                    return;
                  }
                  select('portrait');
                  get('editor').hidden = false;
                });
              }
              get('upload-tab').addEventListener('click', (event) => {
                if (!event.isTrusted) return;
                window.uploadTabClicks++;
                get('upload-pane').hidden = false;
              });
              for (const item of ['portrait', 'landscape']) {
                get(item + '-tab').addEventListener('click', () => {
                  window.directionClicks.push(item);
                  select(item);
                });
              }
              get('reference').addEventListener('change', () => window.referenceUploads++);
              get('footer-switch').addEventListener('click', () => window.footerSwitchClicks++);
              get('publish').addEventListener('click', () => window.publishClicks++);
              get('upload').addEventListener('change', (event) => {
                pending = event.target.files[0];
                window.uploads.push([current, pending.name]);
                get('done').disabled = true;
                const preview = get('preview');
                preview.onload = () => {
                  get('uploaded-background').style.backgroundImage = `url("${preview.src}")`;
                  if (current === 'portrait') {
                    const canvas = get('vertical_coverCanvas');
                    canvas.getContext('2d').drawImage(preview, 0, 0, canvas.width, canvas.height);
                  }
                  get('done').disabled = false;
                };
                preview.src = URL.createObjectURL(pending);
              });
              const save = () => {
                // The applied asset is represented by a different URL after cropping.
                get(current).style.backgroundImage = `url("${URL.createObjectURL(pending)}")`;
                window.saved.push(current);
                get('editor').hidden = true;
              };
              get('done').addEventListener('click', () => {
                if (current === 'landscape') {
                  get('editor').hidden = true;
                  get('confirm').hidden = false;
                } else {
                  get('suggest').hidden = false;
                }
              });
              get('skip-suggestion').addEventListener('click', () => {
                get('suggest').hidden = true;
                window.skippedSuggestions++;
                save();
              });
              get('suggestion-switch').addEventListener('click', () => window.footerSwitchClicks++);
              get('confirm-button').addEventListener('click', () => {
                get('confirm').hidden = true;
                save();
              });
              get('standalone-tab').addEventListener('click', () => {
                get('standalone-upload').hidden = false;
              });
              get('standalone-portrait').addEventListener('click', () => {
                window.cropDirection = 'portrait';
                get('standalone-portrait').classList.add('selected-fixture');
                get('standalone-landscape').classList.remove('selected-fixture');
              });
              get('standalone-input').addEventListener('change', (event) => {
                const file = event.target.files[0];
                const image = new Image();
                image.onload = () => {
                  const canvas = get('standalone-preview');
                  canvas.getContext('2d').drawImage(image, 0, 0, canvas.width, canvas.height);
                  get('standalone-done').disabled = false;
                };
                image.src = URL.createObjectURL(file);
                get('standalone-upload').hidden = true;
                get('standalone-crop').hidden = false;
                get('standalone-done').onclick = () => {
                  get('portrait').style.backgroundImage = `url("${URL.createObjectURL(file)}")`;
                  get('standalone').hidden = true;
                  window.standaloneSaved = file.name === 'portrait.png' &&
                    window.cropDirection === 'portrait';
                };
              });
            </script>
        "#).await?;
        let covers = DouyinVideoCovers {
            portrait: Some(ImageFile::inspect(&portrait)?),
            landscape: Some(ImageFile::inspect(&landscape)?),
        };
        let snapshot = apply(&page, &origin, &covers).await?;
        assert!(truth(&page, r#"
            JSON.stringify(window.uploads) === '[["portrait","portrait.png"],["landscape","landscape.png"]]' &&
            window.saved.join(',') === 'portrait,landscape' &&
            window.openClicks === 3 &&
            window.openedCards.join(',') === 'portrait-card,landscape-card' &&
            window.uploadTabClicks === 2 &&
            window.directionClicks.join(',') === 'landscape' &&
            window.skippedSuggestions === 1 &&
            window.footerSwitchClicks === 0 &&
            window.referenceUploads === 0 &&
            window.publishClicks === 0
        "#).await?);
        verify(&page, &origin, &snapshot).await?;
        page.evaluate_expression(
            "document.getElementById('landscape').style.backgroundImage = 'none'",
        )
        .await?;
        assert!(verify(&page, &origin, &snapshot).await.is_err());
        page.evaluate_expression("window.useStandalone = true")
            .await?;
        let portrait_only = DouyinVideoCovers {
            portrait: covers.portrait,
            landscape: None,
        };
        let snapshot = apply(&page, &origin, &portrait_only).await?;
        assert!(
            truth(
                &page,
                "window.standaloneSaved && window.publishClicks === 0"
            )
            .await?
        );
        verify(&page, &origin, &snapshot).await?;
        page.evaluate_expression("document.getElementById('platform-error').hidden = false")
            .await?;
        let rejected = observe(&page, &origin, "finish", Orientation::Portrait, &[], true)
            .await
            .err()
            .expect("a platform resolution error must block cover preparation");
        assert_eq!(
            rejected.to_string(),
            "抖音竖版封面在处理已选图片时收到抖音提示：图片分辨率过低。"
        );
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
