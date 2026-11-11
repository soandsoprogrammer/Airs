use super::CREATOR_ORIGIN;
use crate::upload_progress::{UploadProgress, UploadProgressRegistry};
use anyhow::Result;
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::{
            dom::{
                BackendNodeId, DescribeNodeParams, Node, ResolveNodeParams, SetFileInputFilesParams,
            },
            input::InsertTextParams,
        },
        js_protocol::runtime::{CallFunctionOnParams, EvaluateParams, RemoteObjectSubtype},
    },
    layout::Point,
};
use publisher_core::{Content, JobState, MediaKind, RunContext, UserError};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::Instant;

const EDITOR: &str = include_str!("xiaohongshu_editor.js");

pub(super) struct Expected {
    pub kind: MediaKind,
    pub title: String,
    pub body: String,
    pub image_count: usize,
}

impl Expected {
    pub(super) fn new(content: &Content) -> Self {
        let title = content.title.trim().to_owned();
        let mut body = normalize(&content.description);
        if !content.tags.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(
                &content
                    .tags
                    .iter()
                    .map(|tag| format!("#{tag}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        Self {
            kind: content.asset.kind,
            title,
            body,
            image_count: content.asset.images.len(),
        }
    }

    fn target(&self) -> &'static str {
        match self.kind {
            MediaKind::Video => "video",
            MediaKind::Images => "image",
        }
    }
}

pub(super) fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{a0}', " ")
        .trim()
        .to_owned()
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum State {
    WrongPage,
    WrongAccount,
    Waiting,
    Blocked,
    SelectFile,
    Uploading,
    Processing,
    Failed,
    Ready,
    Focused,
    Filled,
    UnexpectedContent,
}

#[derive(Deserialize)]
struct Observation {
    status: State,
    percent: Option<u8>,
}

pub(super) fn upload_timeout(bytes: u64) -> Duration {
    Duration::from_secs((600 + bytes.div_ceil(1024 * 1024)).min(7200))
}

fn expression(origin: &str, action: &str, nickname: &str, expected: &Expected) -> Result<String> {
    let nickname = serde_json::to_string(nickname)?;
    let title = serde_json::to_string(&expected.title)?;
    let body = serde_json::to_string(&expected.body)?;
    Ok(format!(
        "({EDITOR})({origin:?}, {action:?}, {:?}, {nickname}, {}, {title}, {body})",
        expected.target(),
        expected.image_count
    ))
}

async fn observe(
    page: &Page,
    origin: &str,
    action: &str,
    nickname: &str,
    expected: &Expected,
) -> Result<Observation> {
    Ok(page
        .evaluate_expression(expression(origin, action, nickname, expected)?)
        .await?
        .into_value()?)
}

async fn editor_node(
    page: &Page,
    origin: &str,
    action: &str,
    nickname: &str,
    expected: &Expected,
    depth: i64,
) -> Result<Node> {
    let mut request = EvaluateParams::new(expression(origin, action, nickname, expected)?);
    request.return_by_value = Some(false);
    let result = page.evaluate_expression(request).await?;
    let object = result.object();
    let Some(input) = object
        .object_id
        .as_ref()
        .filter(|_| object.subtype == Some(RemoteObjectSubtype::Node))
    else {
        return Err(UserError("小红书的发布页面已变化，请重新准备。".into()).into());
    };
    Ok(page
        .execute(
            DescribeNodeParams::builder()
                .object_id(input.clone())
                .depth(depth)
                .pierce(true)
                .build(),
        )
        .await?
        .result
        .node)
}

async fn choose_files(
    page: &Page,
    nickname: &str,
    expected: &Expected,
    paths: Vec<String>,
) -> Result<()> {
    let node = editor_node(page, CREATOR_ORIGIN, "uploadInput", nickname, expected, 0).await?;
    let mut input = SetFileInputFilesParams::new(paths);
    input.backend_node_id = Some(node.backend_node_id);
    page.execute(input).await?;
    Ok(())
}

fn check_state(status: State) -> Result<()> {
    let message = match status {
        State::WrongAccount => "小红书页面账号发生变化，请在账号管理中核对账号后重新准备。",
        State::Failed => "小红书提示素材上传失败，请检查文件格式、大小和网络后重新准备。",
        State::Blocked => "小红书发布页出现验证、弹窗或其他限制，请到创作服务平台检查后重新准备。",
        State::UnexpectedContent => "小红书页面内容与本任务不一致，请重新准备。",
        _ => return Ok(()),
    };
    Err(UserError(message.into()).into())
}

pub(super) async fn prepare(
    page: &Page,
    ctx: &RunContext,
    progress: &UploadProgressRegistry,
    nickname: &str,
    expected: &Expected,
) -> Result<()> {
    let upload_url = format!(
        "{CREATOR_ORIGIN}/publish/publish?from=homepage&target={}",
        expected.target()
    );
    page.goto(upload_url)
        .await
        .map_err(|_| UserError("无法打开小红书发布页，请检查网络后重试。".into()))?;
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            if let Ok(current) = observe(page, CREATOR_ORIGIN, "observe", nickname, expected).await
            {
                check_state(current.status)?;
                if current.status == State::SelectFile {
                    return Ok::<_, anyhow::Error>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| UserError("暂未找到小红书素材上传入口，请检查账号状态后重新准备。".into()))??;

    let asset = &ctx.job.content.asset;
    asset
        .verify()
        .map_err(|error| UserError(error.to_string()))?;
    let paths = match asset.kind {
        MediaKind::Video => vec![asset.path.as_path()],
        MediaKind::Images => asset
            .images
            .iter()
            .map(|image| image.path.as_path())
            .collect(),
    };
    let paths = paths
        .into_iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| UserError("无法读取素材路径。".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stage = match expected.kind {
        MediaKind::Video => "正在上传小红书视频",
        MediaKind::Images => "正在按顺序上传小红书图文图片",
    };
    ctx.stage(JobState::Uploading, stage)?;
    let progress = progress.begin(&ctx.job.id);
    progress.set(Some(UploadProgress::ValidatingFile));
    choose_files(page, nickname, expected, paths).await?;
    let start_deadline = Instant::now() + Duration::from_secs(60);
    tokio::time::timeout(upload_timeout(asset.size), async {
        loop {
            if let Ok(current) = observe(page, CREATOR_ORIGIN, "observe", nickname, expected).await
            {
                check_state(current.status)?;
                match current.status {
                    State::Ready | State::Filled => return Ok::<_, anyhow::Error>(()),
                    State::WrongPage => {
                        return Err(UserError(
                            "小红书已离开当前发布页，请检查登录或重新准备。".into(),
                        )
                        .into());
                    }
                    State::Processing => progress.set(Some(UploadProgress::Processing)),
                    State::Uploading => progress.set(Some(UploadProgress::Uploading {
                        percent: current.percent,
                    })),
                    State::SelectFile if Instant::now() >= start_deadline => {
                        return Err(UserError(
                            "小红书尚未开始上传，请检查文件格式或网络后重新准备。".into(),
                        )
                        .into());
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    })
    .await
    .map_err(|_| UserError("等待小红书处理素材超时，请检查素材或网络后重新准备。".into()))??;

    progress.set(Some(UploadProgress::FillingForm));
    fill_form(page, CREATOR_ORIGIN, nickname, expected).await?;
    publish_point(page, nickname, expected).await?;
    Ok(())
}

async fn fill_form(page: &Page, origin: &str, nickname: &str, expected: &Expected) -> Result<()> {
    for (field, text) in [("title", &expected.title), ("body", &expected.body)] {
        if text.is_empty() {
            continue;
        }
        if observe(page, origin, field, nickname, expected)
            .await?
            .status
            != State::Focused
        {
            return Err(UserError("小红书编辑器无法填写，请检查发布页后重试。".into()).into());
        }
        page.execute(InsertTextParams::new(text)).await?;
    }
    if observe(page, origin, "observe", nickname, expected)
        .await?
        .status
        != State::Filled
    {
        return Err(
            UserError("小红书编辑器没有保留完整标题和正文，请检查内容后重试。".into()).into(),
        );
    }
    Ok(())
}

pub(super) async fn publish_point(
    page: &Page,
    nickname: &str,
    expected: &Expected,
) -> Result<Point> {
    publish_point_at(page, CREATOR_ORIGIN, nickname, expected).await
}

async fn publish_point_at(
    page: &Page,
    origin: &str,
    nickname: &str,
    expected: &Expected,
) -> Result<Point> {
    let state = observe(page, origin, "submit", nickname, expected)
        .await?
        .status;
    check_state(state)?;
    if state != State::Ready {
        return Err(UserError("尚未确认小红书页面的素材和正文，请重新准备。".into()).into());
    }
    let description = editor_node(page, origin, "submitControl", nickname, expected, 7).await?;
    let mut buttons = Vec::new();
    for shadow in description.shadow_roots.iter().flatten() {
        collect_publish_buttons(shadow, &mut buttons);
    }
    let [button] = buttons.as_slice() else {
        return Err(UserError("小红书发布控件已变化，请到创作服务平台检查。".into()).into());
    };
    let button = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(*button)
                .build(),
        )
        .await?;
    let object = button
        .result
        .object
        .object_id
        .ok_or_else(|| UserError("小红书发布控件无法定位。".into()))?;
    let result = page.execute(
        CallFunctionOnParams::builder()
            .object_id(object)
            .function_declaration(format!(r#"function() {{
                const host = this.getRootNode().host;
                if (location.origin !== {origin:?} || location.pathname !== '/publish/publish' ||
                    !this.isConnected || host?.localName !== 'xhs-publish-btn' ||
                    this.textContent.trim() !== '发布' || this.matches(':disabled, [aria-disabled="true"], [aria-busy="true"]')) return null;
                this.scrollIntoView({{block:'center', inline:'center'}});
                const r = this.getBoundingClientRect();
                const x = r.left + r.width / 2;
                const y = r.top + r.height / 2;
                if (!r.width || !r.height || x < 0 || y < 0 || x >= innerWidth || y >= innerHeight ||
                    document.elementFromPoint(x,y) !== host || !this.contains(this.getRootNode().elementFromPoint(x,y))) return null;
                return [x,y];
            }}"#))
            .return_by_value(true)
            .build().map_err(anyhow::Error::msg)?
    ).await?.result;
    let point: Option<[f64; 2]> = result
        .result
        .value
        .and_then(|value| serde_json::from_value(value).ok());
    let [x, y] =
        point.ok_or_else(|| UserError("小红书发布按钮当前不可操作，请检查页面提示。".into()))?;
    Ok(Point::new(x, y))
}

fn collect_publish_buttons(node: &Node, found: &mut Vec<BackendNodeId>) {
    if node.local_name == "button"
        && node.attributes.as_deref().is_some_and(|attributes| {
            attributes.as_chunks::<2>().0.iter().any(|pair| {
                pair[0] == "class" && pair[1].split_whitespace().any(|class| class == "bg-red")
            })
        })
    {
        found.push(node.backend_node_id);
    }
    for child in node.children.iter().flatten() {
        collect_publish_buttons(child, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserSession;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn prepares_two_images_in_order_and_uses_the_closed_shadow_publish_button() -> Result<()>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0; 4096];
                if socket.read(&mut request).await.is_ok() {
                    let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 15\r\nConnection: close\r\n\r\n<!doctype html>").await;
                }
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/publish/publish?from=menu&target=image"))
            .await?;
        let expected = Expected {
            kind: MediaKind::Images,
            title: "两张照片的标题".into(),
            body: "第一行\n第二行 #标签".into(),
            image_count: 2,
        };
        let nickname = "小红书测试账号";
        page.set_content(r#"
            <div id="header-area"><div class="user-info">
              <img class="user_avatar" src="/avatar"><span class="name-box">小红书测试账号</span>
            </div></div>
            <div class="upload-wrapper"><input class="upload-input" type="file" accept=".jpg,.jpeg,.png,.webp" multiple hidden>上传图片</div>
            <template id="after-upload"><main class="publish-page-content">
              <div class="img-upload-area"><div class="img-preview-area">
                <div class="img-container"><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jR1cAAAAASUVORK5CYII="></div>
                <div class="img-container"><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jR1cAAAAASUVORK5CYII="></div>
              </div></div>
              <input placeholder="填写标题会有更多赞哦">
              <div class="tiptap ProseMirror" contenteditable="true" style="min-height:40px"></div>
              <div class="permission-card-select">公开可见</div>
              <xhs-publish-btn submit-text="发布" submit-disabled="false"></xhs-publish-btn>
            </main></template>
            <script>
              window.clickCount = 0;
              window.filenames = [];
              customElements.define('xhs-publish-btn', class extends HTMLElement {
                connectedCallback() {
                  const shadow = this.attachShadow({mode:'closed'});
                  shadow.innerHTML = '<button type="button" class="ce-btn white">暂存离开</button><button type="button" class="ce-btn bg-red">发布</button>';
                  shadow.querySelector('.bg-red').addEventListener('click', event => {
                    if (event.isTrusted) window.clickCount++;
                  });
                }
              });
              document.querySelector('input[type=file]').addEventListener('change', event => {
                window.filenames = [...event.target.files].map(file => file.name);
                event.target.parentElement.replaceWith(document.querySelector('template').content.cloneNode(true));
              });
            </script>
            <style>xhs-publish-btn {display:block;padding:10px}</style>
        "#).await?;
        assert_eq!(
            observe(&page, &origin, "observe", nickname, &expected)
                .await?
                .status,
            State::SelectFile
        );
        assert_eq!(
            observe(&page, CREATOR_ORIGIN, "observe", nickname, &expected)
                .await?
                .status,
            State::WrongPage
        );
        assert_eq!(
            observe(&page, &origin, "observe", "其他账号", &expected)
                .await?
                .status,
            State::WrongAccount
        );
        page.evaluate_expression(
            "document.querySelector('.upload-wrapper').className = 'upload-content-legacy';",
        )
        .await?;
        assert_eq!(
            observe(&page, &origin, "observe", nickname, &expected)
                .await?
                .status,
            State::SelectFile
        );
        let node = editor_node(&page, &origin, "uploadInput", nickname, &expected, 0).await?;
        let paths = ["z.png", "a.png"]
            .into_iter()
            .map(|name| {
                let path = profile.path().join(name);
                std::fs::write(&path, b"fixture input")?;
                Ok(path.to_string_lossy().into_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        let mut input = SetFileInputFilesParams::new(paths);
        input.backend_node_id = Some(node.backend_node_id);
        page.execute(input).await?;
        let filenames: Vec<String> = page
            .evaluate_expression("window.filenames")
            .await?
            .into_value()?;
        assert_eq!(filenames, ["z.png", "a.png"]);
        let _ = page
            .evaluate_expression(
                "Promise.all([...document.images].map(image => image.decode().catch(() => {})))",
            )
            .await?;
        assert_eq!(
            observe(&page, &origin, "observe", nickname, &expected)
                .await?
                .status,
            State::Ready
        );
        fill_form(&page, &origin, nickname, &expected).await?;
        let point = publish_point_at(&page, &origin, nickname, &expected).await?;
        page.click(point).await?;
        assert_eq!(
            page.evaluate_expression("window.clickCount")
                .await?
                .into_value::<u32>()?,
            1
        );
        let _ = page
            .evaluate_expression(
                "document.querySelector('xhs-publish-btn').setAttribute('submit-disabled','true')",
            )
            .await?;
        assert!(
            publish_point_at(&page, &origin, nickname, &expected)
                .await
                .is_err()
        );
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
