use crate::kuaishou::photo::PHOTO_PROBE;
use anyhow::Result;
use chromiumoxide::{Page, layout::Point};
use publisher_core::UserError;
use serde::Deserialize;
use std::time::Duration;

const CREATOR_ORIGIN: &str = "https://cp.kuaishou.com";
const PUBLISH_TARGET: &str = include_str!("kuaishou_submit.js");
const LOGIN_PROBE: &str = include_str!("kuaishou_login.js");
const PREPARE_PROBE: &str = include_str!("kuaishou_prepare_page.js");

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum PublishCheck {
    Ready { point: (f64, f64) },
    NoConfirmation,
    WrongPage,
    AccountChanged,
    Blocked,
    UploadNotReady,
    ContentChanged,
    ImagesChanged,
    DeclarationRequired,
    PublishOptionsChanged,
    PublishUnavailable,
}

async fn inspect_control(
    page: &Page,
    origin: &str,
    nickname: &str,
    caption: &str,
    images: Option<&[String]>,
    confirmation: bool,
) -> Result<Option<Point>> {
    let args = serde_json::json!([origin, nickname, caption, images, confirmation]);
    let expression = format!(
        "({PUBLISH_TARGET})(...{args}, ({LOGIN_PROBE}), ({PREPARE_PROBE}), ({PHOTO_PROBE}))"
    );
    let check = page
        .evaluate_expression(expression)
        .await
        .and_then(|result| result.into_value::<PublishCheck>().map_err(Into::into))
        .map_err(|_| UserError("无法核对快手发布页，请重新准备作品。".into()))?;
    let message = match check {
        PublishCheck::Ready { point: (x, y) } => return Ok(Some(Point { x, y })),
        PublishCheck::NoConfirmation => return Ok(None),
        PublishCheck::WrongPage => "当前页面不是快手作品编辑页，请重新准备作品。",
        PublishCheck::AccountChanged => {
            "无法确认快手发布页使用的是本次准备的账号，请检查登录状态。"
        }
        PublishCheck::Blocked => "快手发布页有待处理的提示、必填项或遮挡，请先在平台核对。",
        PublishCheck::UploadNotReady => "快手尚未确认素材上传完成，请重新准备作品。",
        PublishCheck::ContentChanged => "快手编辑页的作品描述与任务记录不一致，请重新准备作品。",
        PublishCheck::ImagesChanged => "快手编辑页的图片或顺序已变化，请重新准备图文。",
        PublishCheck::DeclarationRequired => {
            "快手要求填写作者声明，请在快手创作者服务平台核对并选择。"
        }
        PublishCheck::PublishOptionsChanged => {
            "快手发布页的可见范围或发布时间发生变化，请在平台核对。"
        }
        PublishCheck::PublishUnavailable => {
            "未找到唯一可用的「发布」按钮，请先在快手创作者服务平台核对。"
        }
    };
    Err(UserError(message.into()).into())
}

async fn inspect_target(
    page: &Page,
    origin: &str,
    nickname: &str,
    caption: &str,
    images: Option<&[String]>,
) -> Result<Point> {
    inspect_control(page, origin, nickname, caption, images, false)
        .await?
        .ok_or_else(|| UserError("未找到快手发布按钮，请重新准备。".into()).into())
}

pub(crate) async fn preflight(
    page: &Page,
    nickname: &str,
    caption: &str,
    images: Option<&[String]>,
) -> Result<()> {
    inspect_target(page, CREATOR_ORIGIN, nickname, caption, images).await?;
    Ok(())
}

pub(crate) async fn click_publish(
    page: &Page,
    nickname: &str,
    caption: &str,
    images: Option<&[String]>,
) -> Result<()> {
    click_publish_at_origin(page, CREATOR_ORIGIN, nickname, caption, images).await?;
    confirm_publish_at_origin(page, CREATOR_ORIGIN, nickname, caption, images).await;
    Ok(())
}

async fn confirm_publish_at_origin(
    page: &Page,
    origin: &str,
    nickname: &str,
    caption: &str,
    images: Option<&[String]>,
) {
    // Direct publishing uses the same receipt observer, so the confirmation
    // check is bounded and never repeats the initial Publish click.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match inspect_control(page, origin, nickname, caption, images, true).await {
                Ok(Some(point)) => {
                    let _ = page.click(point).await;
                    break;
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(_) => break,
            }
        }
    })
    .await;
}

pub(crate) async fn click_publish_at_origin(
    page: &Page,
    origin: &str,
    nickname: &str,
    caption: &str,
    images: Option<&[String]>,
) -> Result<()> {
    let point = inspect_target(page, origin, nickname, caption, images).await?;
    page.click(point)
        .await
        .map_err(|_| UserError("无法确认快手是否收到发布操作，请到创作者服务平台核对。".into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserSession;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn publish_click_checks_account_caption_upload_and_unique_uncovered_button() -> Result<()>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 15\r\nConnection: close\r\n\r\n<!doctype html>";
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0; 2048];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(RESPONSE).await;
                }
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/article/publish/video?tabType=1"))
            .await?;
        page.set_content(
            r#"<meta charset="utf-8">
              <header class="header-bar"><div class="user-info-dpd">
                <div class="user-info-avatar">头像</div><div class="user-info-name">创作者</div>
              </div></header>
              <div id="root-video-publish"><div id="onvideo_creator_platform"><div id="joyride-wrapper">
                <main id="preview-tours"><div class="_preview_1eni7_92"><div class="_preview-header_1eni7_92">
                  <span>预览封面</span><span id="works-preview">预览作品</span>
                </div></div></main>
                <div id="work-description-edit" contenteditable="true">正确描述</div>
                <span id="declaration" class="ant-select-selection-placeholder" hidden>请选择作者声明</span>
                <div class="ant-radio-group">
                  <label class="ant-radio-wrapper"><input class="ant-radio-input" type="radio" name="visibility" checked>所有人可见</label>
                  <label class="ant-radio-wrapper"><input id="private" class="ant-radio-input" type="radio" name="visibility">仅自己可见</label>
                </div>
                <div style="height:1200px"></div>
                <div id="actions" class="_edit-section-btns_fixture"><div id="publish" class="_button-primary_3a3lq_60">发布</div></div>
              </div></div></div>
              <div id="cover" hidden style="position:fixed;inset:0;z-index:999"></div>
              <div id="confirm-wrap" class="ant-modal-confirm-centered" hidden
                   style="position:fixed;top:40px;left:40px;z-index:1000;background:white">
                <div class="ant-modal ant-modal-confirm" role="dialog">
                  <div class="ant-modal-confirm-title">提示</div>
                  <button id="confirm" class="ant-btn-primary" onclick="if(event.isTrusted) {
                    window.confirmClicks++; document.querySelector('#confirm-wrap').hidden=true;
                  }">确定</button>
                </div>
              </div>
              <script>
                window.publishClicks = [];
                window.confirmClicks = 0;
                document.querySelector('#publish').addEventListener('click',
                  event => window.publishClicks.push(event.isTrusted));
              </script>"#,
        )
        .await?;
        assert!(
            inspect_target(&page, CREATOR_ORIGIN, "创作者", "正确描述", None)
                .await
                .is_err()
        );
        assert!(
            inspect_target(&page, &origin, "另一个账号", "正确描述", None)
                .await
                .is_err()
        );
        assert!(
            inspect_target(&page, &origin, "创作者", "其他描述", None)
                .await
                .is_err()
        );
        for (change, restore) in [
            (
                "document.querySelector('#works-preview').hidden = true",
                "document.querySelector('#works-preview').hidden = false",
            ),
            (
                "document.querySelector('#declaration').hidden = false",
                "document.querySelector('#declaration').hidden = true",
            ),
            (
                "document.querySelector('#private').checked = true",
                "document.querySelector('input[name=visibility]').checked = true",
            ),
            (
                "document.querySelector('#publish').classList.add('_disabled_3a3lq_10')",
                "document.querySelector('#publish').classList.remove('_disabled_3a3lq_10')",
            ),
            (
                "document.querySelector('#actions').appendChild(document.querySelector('#publish').cloneNode(true))",
                "document.querySelector('#actions').lastElementChild.remove()",
            ),
            (
                "document.querySelector('#cover').hidden = false",
                "document.querySelector('#cover').hidden = true",
            ),
        ] {
            page.evaluate_expression(change).await?;
            assert!(
                click_publish_at_origin(&page, &origin, "创作者", "正确描述", None)
                    .await
                    .is_err()
            );
            page.evaluate_expression(restore).await?;
        }
        inspect_target(&page, &origin, "创作者", "正确描述", None).await?;
        assert!(
            inspect_control(&page, &origin, "创作者", "正确描述", None, true)
                .await?
                .is_none()
        );
        assert!(crate::browser::truth(&page, "window.publishClicks.length === 0").await?);
        click_publish_at_origin(&page, &origin, "创作者", "正确描述", None).await?;
        assert!(
            crate::browser::truth(
                &page,
                "window.publishClicks.length === 1 && window.publishClicks[0] === true"
            )
            .await?
        );
        page.evaluate_expression("document.querySelector('#confirm-wrap').hidden=false")
            .await?;
        assert!(
            inspect_target(&page, &origin, "创作者", "正确描述", None)
                .await
                .is_err()
        );
        confirm_publish_at_origin(&page, &origin, "创作者", "正确描述", None).await;
        assert!(crate::browser::truth(&page, "window.confirmClicks === 0").await?);
        page.evaluate_expression("document.querySelector('#confirm').textContent='确认发布'")
            .await?;
        confirm_publish_at_origin(&page, &origin, "创作者", "错误描述", None).await;
        assert!(crate::browser::truth(&page, "window.confirmClicks === 0").await?);
        confirm_publish_at_origin(&page, &origin, "创作者", "正确描述", None).await;
        assert!(
            crate::browser::truth(
                &page,
                "window.publishClicks.length === 1 && window.confirmClicks === 1"
            )
            .await?
        );
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
