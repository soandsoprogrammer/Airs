use crate::{
    upload_progress::{UploadProgress, UploadProgressGuard},
    wechat_channels::CREATOR_ORIGIN,
};
use anyhow::Result;
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::dom::{DescribeNodeParams, SetFileInputFilesParams},
        js_protocol::runtime::EvaluateParams,
    },
    layout::Point,
};
use publisher_core::{Content, MediaKind, UserError};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::{sleep, timeout};

const EDITOR: &str = include_str!("wechat_channels_editor.js");
const EDITOR_WAIT: Duration = Duration::from_secs(45);
const FORM_WAIT: Duration = Duration::from_secs(10);
const FOLLOWUP_WAIT: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_millis(250);

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditorSnapshot {
    media: String,
    description: String,
    short_title: Option<String>,
}

impl EditorSnapshot {
    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    pub(crate) fn short_title(&self) -> Option<&str> {
        self.short_title.as_deref()
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum Observation {
    Waiting,
    EditorLoading,
    WrongPage,
    Blocked,
    InputReady,
    Uploading { percent: Option<u8> },
    Processing,
    UploadFailed,
    PreviewUnavailable,
    EditorReady,
    Filled,
    ContentChanged,
    MediaChanged,
    SettingsChanged,
    PublishUnavailable,
    Ready(EditorSnapshot),
    Point { point: (f64, f64) },
}

impl Observation {
    fn error(&self) -> anyhow::Error {
        let message = match self {
            Self::WrongPage => "已离开视频号发表页，请检查登录后重新准备作品。",
            Self::Blocked => "视频号发表页有弹窗、遮挡或待处理项，请先在视频号助手核对。",
            Self::UploadFailed => "视频号提示视频上传或处理失败，请检查视频格式和网络后重试。",
            Self::PreviewUnavailable => {
                "视频号显示视频已上传，但当前浏览器无法预览。请在视频号助手中核对视频格式。"
            }
            Self::ContentChanged => "视频号发表页的文案与任务不一致，请重新准备作品。",
            Self::MediaChanged => "视频号发表页的素材或文案已变化，请重新准备作品。",
            Self::SettingsChanged => {
                "视频号发表页的发表时间或可见范围已变化，请先在视频号助手核对。"
            }
            Self::PublishUnavailable => "视频号的「发表」按钮不可用，请先在视频号助手核对。",
            _ => "视频号尚未确认视频和文案已就绪，请重新准备作品。",
        };
        UserError(message.into()).into()
    }
}

struct Caption {
    text: String,
    short_title: String,
}

impl Caption {
    fn new(content: &Content) -> Self {
        let title = content.title.trim();
        let description = content.description.trim();
        let topics = content
            .tags
            .iter()
            .map(|tag| tag.trim().trim_start_matches('#').trim())
            .filter(|tag| !tag.is_empty())
            .map(|tag| format!("#{tag}"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = [title, description, &topics]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        // This optional field has a narrower character set than the caption.
        let short_title = if (6..=16).contains(&title.chars().count())
            && title.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || ('一'..='龥').contains(&character)
                    || "℃《》\"“”:：+?？% ".contains(character)
            }) {
            title.to_owned()
        } else {
            String::new()
        };
        Self { text, short_title }
    }
}

pub(crate) fn validate_content(content: &Content) -> Result<()> {
    let units: usize = Caption::new(content)
        .text
        .encode_utf16()
        .map(|unit| if unit <= 0xff { 1 } else { 2 })
        .sum();
    if units > 2_000 {
        return Err(UserError("视频号文案超过长度上限，请缩短标题、正文或话题。".into()).into());
    }
    Ok(())
}

fn expression(
    origin: &str,
    action: &str,
    caption: Option<&Caption>,
    snapshot: Option<&EditorSnapshot>,
) -> String {
    let arguments = serde_json::json!([
        origin,
        action,
        caption.map(|value| value.text.as_str()).unwrap_or_default(),
        caption
            .map(|value| value.short_title.as_str())
            .unwrap_or_default(),
        snapshot,
    ]);
    format!("({EDITOR})(...{arguments})")
}

async fn observe(
    page: &Page,
    origin: &str,
    action: &str,
    caption: Option<&Caption>,
    snapshot: Option<&EditorSnapshot>,
) -> Result<Observation> {
    page.evaluate_expression(expression(origin, action, caption, snapshot))
        .await
        .and_then(|value| value.into_value().map_err(Into::into))
        .map_err(|_| UserError("无法读取视频号发表页，请重新准备作品。".into()).into())
}

async fn click(page: &Page, (x, y): (f64, f64), error: &'static str) -> Result<()> {
    page.click(Point { x, y })
        .await
        .map_err(|_| UserError(error.into()))?;
    Ok(())
}

pub(crate) async fn open_editor(page: &Page) -> Result<()> {
    open_editor_at(page, CREATOR_ORIGIN).await
}

async fn open_editor_at(page: &Page, origin: &str) -> Result<()> {
    let mut clicked = false;
    let mut editor_seen = false;
    let wait = async {
        loop {
            match observe(page, origin, "open", None, None).await? {
                Observation::InputReady => return Ok(()),
                Observation::Point { point } if !clicked => {
                    click(page, point, "无法打开视频号的「发表视频」入口。").await?;
                    clicked = true;
                }
                Observation::EditorLoading => editor_seen = true,
                Observation::Waiting | Observation::Point { .. } => {}
                other => return Err(other.error()),
            }
            sleep(POLL).await;
        }
    };
    timeout(EDITOR_WAIT, wait).await.map_err(|_| {
        let message = if editor_seen {
            "视频号发表页未显示可用的视频上传控件，请重新准备。"
        } else if clicked {
            "已点击「发表视频」，视频号助手未打开发表页，请重新准备。"
        } else {
            "视频号助手首页未显示可用的「发表视频」入口，请重新准备。"
        };
        UserError(message.into())
    })?
}

async fn select_video(page: &Page, origin: &str, content: &Content) -> Result<()> {
    if content.asset.kind != MediaKind::Video {
        return Err(UserError("视频号任务需要一段视频素材。".into()).into());
    }
    content
        .asset
        .verify()
        .map_err(|error| UserError(error.to_string()))?;
    let path = content
        .asset
        .path
        .to_str()
        .ok_or_else(|| UserError("无法读取视频素材路径，请重新选择视频。".into()))?;
    let unavailable = || UserError("视频号的视频上传控件不可用，请重新准备。".into());
    let mut params = EvaluateParams::new(expression(origin, "input", None, None));
    params.return_by_value = Some(false);
    let selection = page
        .evaluate_expression(params)
        .await
        .map_err(|_| unavailable())?;
    let object = selection
        .object()
        .object_id
        .clone()
        .ok_or_else(unavailable)?;
    let node = page
        .execute(DescribeNodeParams::builder().object_id(object).build())
        .await
        .map_err(|_| unavailable())?
        .result
        .node;
    let mut files = SetFileInputFilesParams::new(vec![path.to_owned()]);
    files.backend_node_id = Some(node.backend_node_id);
    page.execute(files)
        .await
        .map_err(|_| UserError("无法把本地视频交给视频号上传，请重新准备。".into()))?;
    Ok(())
}

async fn wait_for_upload(
    page: &Page,
    origin: &str,
    limit: Duration,
    progress: &UploadProgressGuard,
) -> Result<()> {
    let wait = async {
        loop {
            match observe(page, origin, "observe", None, None).await? {
                Observation::EditorReady => return Ok(()),
                Observation::Waiting => {
                    progress.set(Some(UploadProgress::Uploading { percent: None }));
                }
                Observation::Uploading { percent } => {
                    progress.set(Some(UploadProgress::Uploading { percent }));
                }
                Observation::Processing => progress.set(Some(UploadProgress::Processing)),
                other => return Err(other.error()),
            }
            sleep(POLL).await;
        }
    };
    timeout(limit, wait)
        .await
        .map_err(|_| UserError("未能在时限内确认视频号上传完成并生成可核对的视频预览。".into()))?
}

pub(crate) async fn upload_and_fill(
    page: &Page,
    content: &Content,
    limit: Duration,
    progress: &UploadProgressGuard,
) -> Result<EditorSnapshot> {
    upload_and_fill_at(page, CREATOR_ORIGIN, content, limit, progress).await
}

async fn upload_and_fill_at(
    page: &Page,
    origin: &str,
    content: &Content,
    limit: Duration,
    progress: &UploadProgressGuard,
) -> Result<EditorSnapshot> {
    validate_content(content)?;
    progress.set(Some(UploadProgress::Uploading { percent: None }));
    select_video(page, origin, content).await?;
    wait_for_upload(page, origin, limit, progress).await?;
    content
        .asset
        .verify()
        .map_err(|error| UserError(error.to_string()))?;
    progress.set(Some(UploadProgress::FillingForm));
    let caption = Caption::new(content);
    let outcome = observe(page, origin, "fill", Some(&caption), None).await?;
    if !matches!(outcome, Observation::Filled) {
        return Err(outcome.error());
    }
    let wait = async {
        loop {
            match observe(page, origin, "verify", Some(&caption), None).await? {
                Observation::Ready(snapshot) => return Ok(snapshot),
                Observation::ContentChanged
                | Observation::Waiting
                | Observation::PublishUnavailable => {}
                other => return Err(other.error()),
            }
            sleep(POLL).await;
        }
    };
    timeout(FORM_WAIT, wait)
        .await
        .map_err(|_| UserError("未能核对视频号的文案或「发表」按钮，请重新准备。".into()))?
}

pub(crate) async fn preflight(
    page: &Page,
    content: &Content,
    snapshot: &EditorSnapshot,
) -> Result<()> {
    let caption = Caption::new(content);
    let outcome = observe(
        page,
        CREATOR_ORIGIN,
        "verify",
        Some(&caption),
        Some(snapshot),
    )
    .await?;
    if matches!(outcome, Observation::Ready(_)) {
        Ok(())
    } else {
        Err(outcome.error())
    }
}

pub(crate) async fn click_publish(
    page: &Page,
    content: &Content,
    snapshot: &EditorSnapshot,
) -> Result<()> {
    click_publish_at(page, CREATOR_ORIGIN, content, snapshot).await
}

async fn click_publish_at(
    page: &Page,
    origin: &str,
    content: &Content,
    snapshot: &EditorSnapshot,
) -> Result<()> {
    let caption = Caption::new(content);
    let outcome = observe(page, origin, "publish", Some(&caption), Some(snapshot)).await?;
    let Observation::Point { point } = outcome else {
        return Err(outcome.error());
    };
    const UNCERTAIN: &str = "无法确认视频号是否收到发表操作，请到视频号助手核对。";
    click(page, point, UNCERTAIN).await?;

    // Some accounts see the originality prompt as part of this same action.
    // The caller's receipt check remains responsible for the publication result.
    let followup = async {
        loop {
            match observe(page, origin, "publish-followup", None, None).await? {
                Observation::Point { point } => return click(page, point, UNCERTAIN).await,
                Observation::WrongPage => return Ok(()),
                _ => sleep(POLL).await,
            }
        }
    };
    timeout(FOLLOWUP_WAIT, followup).await.unwrap_or(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{browser::BrowserSession, upload_progress::UploadProgressRegistry};
    use publisher_core::Asset;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn video_editor_checks_the_uploaded_video_caption_and_both_click_targets() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            const BODY: &str = "<!doctype html><meta charset=utf-8><body></body>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                BODY.len()
            );
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0; 4096];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        let profile = tempfile::tempdir()?;
        let asset = profile.path().join("clip.mp4");
        std::fs::write(&asset, b"offline fixture")?;
        let content = Content {
            asset: Asset::inspect(&asset)?,
            title: "一段测试视频".into(),
            description: "第一行\n第二行".into(),
            tags: vec!["#测试".into()],
            douyin_video_covers: Default::default(),
        };
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/platform"))
            .await?;
        page.set_content(r#"
          <meta charset="utf-8">
          <button class="weui-desktop-btn" id="hidden-entry" hidden>发表视频</button>
          <wujie-app data-wujie-id="postCard"></wujie-app>
          <script>
            window.result = { open: [], hiddenEntry: 0, files: [], decoy: 0, submit: [], continue: [], original: 0 };
            document.getElementById('hidden-entry').onclick = () => window.result.hiddenEntry++;
            const createRuntimeFrame = (doc, appId) => {
              const runtime = doc.createElement('iframe');
              runtime.hidden = true;
              runtime.setAttribute('data-wujie-id', appId);
              doc.body.append(runtime);
              return runtime;
            };
            const patchWujieRoot = (shadow, runtimeDocument) => {
              for (const element of shadow.querySelectorAll('*')) {
                element.getRootNode = () => runtimeDocument;
              }
            };
            const postCard = document.querySelector('wujie-app[data-wujie-id="postCard"]');
            const homeRuntime = createRuntimeFrame(document, 'postCard');
            homeRuntime.contentDocument.body.innerHTML = '<button class="weui-desktop-btn">发表视频</button>';
            homeRuntime.contentDocument.querySelector('button').onclick = () => window.result.hiddenEntry++;
            const home = postCard.attachShadow({mode:'open'});
            home.innerHTML = '<div><button class="weui-desktop-btn" id="open">发表视频</button></div>';
            patchWujieRoot(home, homeRuntime.contentDocument);
            home.getElementById('open').onclick = (event) => {
              window.result.open.push(event.isTrusted);
              postCard.remove();
              homeRuntime.remove();
              history.replaceState({}, '', '/platform/post/create');
              const frame = document.createElement('iframe');
              frame.style.cssText = 'width:600px;height:420px;border:3px solid #000';
              frame.src = '/micro/content/post/create';
              frame.onload = () => {
                const runtime = createRuntimeFrame(frame.contentDocument, 'content');
                const host = frame.contentDocument.createElement('wujie-app');
                host.setAttribute('data-wujie-id', 'content');
                frame.contentDocument.body.append(host);
                const root = host.attachShadow({ mode:'open' });
                window.fixture = root;
                root.innerHTML = `
                  <div class="post-create-wrap">
                    <div class="post-upload-wrap">
                      <div class="upload-wrap">视频<input type="file" accept="video/*" id="video" hidden></div>
                      <div class="upload-wrap">封面<input type="file" accept="image/*" id="image" hidden></div>
                    </div>
                    <div class="media-status-wrap" hidden>
                      <div class="media-video-wrap"><video id="previewVideo" class="media-video" width="80" height="60"></video></div>
                      <div class="media-status-content"><div class="upload-progress"><span role="progressbar" aria-valuenow="99">99%</span></div></div>
                    </div>
                    <div class="post-desc-box"><div class="input-editor" contenteditable="true" style="min-height:80px;border:1px solid #000"></div></div>
                    <div class="short-title-wrap"><input placeholder="填写短标题有机会获得更多流量"></div>
                    <div class="form-btns"><button>保存草稿</button><button id="publish">发表</button></div>
                  </div>`;
                patchWujieRoot(root, runtime.contentDocument);
                const get = (id) => root.getElementById(id);
                get('image').onchange = () => window.result.decoy++;
                get('video').onchange = (event) => {
                  const file = event.target.files[0];
                  window.result.files.push(file.name);
                  const video = get('previewVideo');
                  video.src = URL.createObjectURL(file);
                  Object.defineProperties(video, {readyState:{value:1}, videoWidth:{value:1920}});
                  root.querySelector('.media-status-wrap').hidden = false;
                };
                window.completeFixtureUpload = () => {
                  const temporary = root.querySelector('.media-status-wrap');
                  const source = get('previewVideo').src;
                  const done = frame.contentDocument.createElement('div');
                  done.className = 'post-media-preview-wrap';
                  done.innerHTML = `<div class="post-media-wrap">
                    <div class="full-screen-wrap"><div class="media-full-screen-wrap">
                      <video hidden></video>
                      <div class="video-content"><video id="fullScreenVideo" width="80" height="60"></video></div>
                    </div></div>
                  </div>`;
                  const video = done.querySelector('#fullScreenVideo');
                  video.src = source;
                  Object.defineProperties(video, {
                    currentSrc:{get:() => video.src}, readyState:{value:1}, videoWidth:{value:1920},
                  });
                  temporary.replaceWith(done);
                  patchWujieRoot(root, runtime.contentDocument);
                };
                get('publish').onclick = (event) => {
                  window.result.submit.push(event.isTrusted);
                  const modal = frame.contentDocument.createElement('div');
                  modal.className = 'intercept-original-dialog';
                  modal.style.cssText = 'position:fixed;inset:20px;background:white;z-index:9';
                  modal.setAttribute('role', 'dialog');
                  modal.innerHTML = `<h2 class="weui-desktop-dialog__title">声明原创的视频有机会获得广告分成</h2>
                    <button id="continue">直接发表</button><button id="declare">声明原创</button>`;
                  root.append(modal);
                  patchWujieRoot(root, runtime.contentDocument);
                  get('continue').onclick = (next) => {
                    window.result.continue.push(next.isTrusted);
                    modal.remove();
                  };
                  get('declare').onclick = () => window.result.original++;
                };
              };
              document.body.append(frame);
            };
          </script>
        "#).await?;
        open_editor_at(&page, &origin).await?;
        select_video(&page, &origin, &content).await?;
        let progress = UploadProgressRegistry::default().begin("fixture");
        assert!(matches!(
            observe(&page, &origin, "observe", None, None).await?,
            Observation::Uploading { percent: Some(99) },
        ));
        page.evaluate_expression(
            "window.fixture.querySelector('[role=progressbar]').setAttribute('aria-valuenow', '100')",
        )
        .await?;
        assert!(matches!(
            observe(&page, &origin, "observe", None, None).await?,
            Observation::Uploading { percent: Some(100) },
        ));
        page.evaluate_expression("window.fixture.querySelector('.upload-progress').remove()")
            .await?;
        assert!(
            matches!(
                observe(&page, &origin, "observe", None, None).await?,
                Observation::Waiting | Observation::Processing,
            ),
            "the transient local video preview must not mark the upload complete"
        );
        page.evaluate_expression("window.completeFixtureUpload()")
            .await?;
        wait_for_upload(&page, &origin, Duration::from_secs(2), &progress).await?;
        page.evaluate_expression("window.fixture.getElementById('video').value = ''")
            .await?;
        assert!(
            matches!(
                observe(&page, &origin, "open", None, None).await?,
                Observation::ContentChanged,
            ),
            "an empty caption and file input must not disguise an existing completed draft"
        );
        let caption = Caption::new(&content);
        assert!(matches!(
            observe(&page, &origin, "fill", Some(&caption), None).await?,
            Observation::Filled,
        ));
        let Observation::Ready(snapshot) =
            observe(&page, &origin, "verify", Some(&caption), None).await?
        else {
            anyhow::bail!("the fixture caption or media was not ready");
        };
        assert_eq!(
            snapshot.description(),
            "一段测试视频\n第一行\n第二行\n#测试"
        );
        assert_eq!(snapshot.short_title(), Some("一段测试视频"));
        page.evaluate_expression(
            "window.fixture.getElementById('fullScreenVideo').src = URL.createObjectURL(new Blob(['other video']))",
        )
        .await?;
        assert!(
            click_publish_at(&page, &origin, &content, &snapshot)
                .await
                .is_err()
        );
        page.evaluate_expression(format!(
            "window.fixture.getElementById('fullScreenVideo').src = {}",
            serde_json::to_string(&snapshot.media)?,
        ))
        .await?;
        page.evaluate_expression(
            r#"window.fixture.querySelector('.input-editor').innerText = '不同文案'"#,
        )
        .await?;
        assert!(
            click_publish_at(&page, &origin, &content, &snapshot)
                .await
                .is_err()
        );
        page.evaluate_expression(format!(
            "window.fixture.querySelector('.input-editor').innerText = {}",
            serde_json::to_string(snapshot.description())?,
        ))
        .await?;
        page.evaluate_expression(
            r#"
            const overlay = document.createElement('div');
            overlay.id = 'obstruction';
            overlay.style.cssText = 'position:fixed;inset:0;z-index:9999';
            document.body.append(overlay);
        "#,
        )
        .await?;
        assert!(
            click_publish_at(&page, &origin, &content, &snapshot)
                .await
                .is_err()
        );
        page.evaluate_expression("document.getElementById('obstruction').remove()")
            .await?;
        click_publish_at(&page, &origin, &content, &snapshot).await?;
        let checks: serde_json::Value = page
            .evaluate_expression("window.result")
            .await?
            .into_value()?;
        assert_eq!(
            checks,
            serde_json::json!({
                "open": [true], "hiddenEntry": 0, "files": ["clip.mp4"], "decoy": 0,
                "submit": [true], "continue": [true], "original": 0,
            })
        );
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
