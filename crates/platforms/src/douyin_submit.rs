use crate::douyin::{PHOTO_FILE_SELECTOR, PHOTO_PAGE_SCRIPT, PhotoSnapshot};
use anyhow::Result;
use chromiumoxide::{Page, layout::Point};
use publisher_core::{Content, MediaKind, UserError};

const CREATOR_ORIGIN: &str = "https://creator.douyin.com";
const UPLOAD_PROBE: &str = include_str!("douyin_upload_page.js");

const PUBLISH_TARGET: &str = r#"(origin, title, description, scroll, photos, photoSelector, uploadProbe, photoProbe) => {
  if (location.origin !== origin ||
      (!photos && !['/creator-micro/content/publish', '/creator-micro/content/post/video']
        .includes(location.pathname.replace(/\/$/, '')))) {
    return { status: 'wrongPage' };
  }

  const visible = (element) => element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const present = (selector) => [...document.querySelectorAll(selector)].filter(visible);

  if (present('dialog, [role="dialog"], [aria-modal="true"]').length ||
      present('[aria-invalid="true"]').length ||
      present('[role="alert"], [aria-live="assertive"]').some((alert) =>
        /发布失败|上传失败|安全验证|身份验证|请(?:先)?(?:登录|完成验证|勾选|(?:添加|设置|选择|上传)(?:视频)?封面)/
          .test(alert.innerText))) {
    return { status: 'blocked' };
  }
  if (photos) {
    const observation = photoProbe(origin, photoSelector, photos.sources.length, 'observe', '', '');
    if (observation.status === 'wrongPage') return { status: 'wrongPage' };
    if (observation.status !== 'ready') return { status: 'uploadNotReady' };
    if (observation.photos.cover !== photos.cover ||
        observation.photos.sources.some((source, index) => source !== photos.sources[index])) {
      return { status: 'photosChanged' };
    }
    if (observation.title !== title ||
        observation.description !== description.replace(/\r\n?/g, '\n').replace(/\u00a0/g, ' ').trim()) {
      return { status: 'contentChanged' };
    }
    const selectedRadio = [...document.querySelectorAll('input[type="radio"]:checked, [role="radio"][aria-checked="true"]')]
      .map((element) => element.closest('label') ?? element).filter(visible);
    if (selectedRadio.some((element) => /^(?:定时发布|定时|仅自己可见|私密|好友可见|仅好友可见|粉丝可见)$/
      .test((element.getAttribute('aria-label') ?? element.innerText ?? '').trim()))) {
      return { status: 'settingsChanged' };
    }
  } else {
    if (uploadProbe(origin).status !== 'ready') return { status: 'uploadNotReady' };
    const titles = present('input[placeholder*="填写作品标题"]');
    const descriptions = present('div.zone-container[contenteditable="true"]');
    if (titles.length !== 1 || descriptions.length !== 1 ||
        titles[0].value !== title || descriptions[0].innerText.trim() !== description.trim()) {
      return { status: 'contentChanged' };
    }
  }

  const buttons = present('button, [role="button"], input[type="submit"], input[type="button"]')
    .filter((button) =>
      (button.localName === 'input' ? button.value : button.innerText).trim() === '发布' &&
      !button.closest(':disabled, [disabled], [aria-disabled="true"], [aria-busy="true"]') &&
      getComputedStyle(button).pointerEvents !== 'none');
  if (buttons.length !== 1) return { status: 'publishUnavailable' };
  if (!scroll) return { status: 'ready' };

  const button = buttons[0];
  button.scrollIntoView({ behavior: 'instant', block: 'center', inline: 'center' });
  const bounds = button.getBoundingClientRect();
  const x = bounds.left + bounds.width / 2;
  const y = bounds.top + bounds.height / 2;
  const hit = document.elementFromPoint(x, y);
  if (!hit || !button.contains(hit)) return { status: 'blocked' };
  return { status: 'ready', point: [x, y] };
}"#;

#[derive(serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum PublishCheck {
    Ready { point: Option<(f64, f64)> },
    WrongPage,
    Blocked,
    UploadNotReady,
    ContentChanged,
    PhotosChanged,
    SettingsChanged,
    PublishUnavailable,
}

/// Generate the same description for preparation and subsequent verification.
pub(crate) fn prepared_description(content: &Content) -> String {
    if content.tags.is_empty() {
        content.description.clone()
    } else {
        format!(
            "{} {}",
            content.description,
            content
                .tags
                .iter()
                .map(|tag| format!("#{tag}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

async fn inspect_target(
    page: &Page,
    origin: &str,
    content: &Content,
    photos: Option<&PhotoSnapshot>,
    scroll: bool,
) -> Result<Option<Point>> {
    if (content.asset.kind == MediaKind::Images) != photos.is_some() {
        return Err(UserError("任务素材与抖音编辑页不匹配，请重新准备。".into()).into());
    }
    let arguments = serde_json::json!([
        origin,
        content.title,
        prepared_description(content),
        scroll,
        photos,
        PHOTO_FILE_SELECTOR,
    ]);
    let expression =
        format!("({PUBLISH_TARGET})(...{arguments}, ({UPLOAD_PROBE}), ({PHOTO_PAGE_SCRIPT}))");
    let check = page
        .evaluate_expression(expression)
        .await
        .and_then(|result| result.into_value::<PublishCheck>().map_err(Into::into))
        .map_err(|_| UserError("无法核对抖音发布页，请重新准备作品。".into()))?;
    let message = match check {
        PublishCheck::Ready { point } => return Ok(point.map(|(x, y)| Point { x, y })),
        PublishCheck::WrongPage => "当前页面不是抖音作品编辑页，请重新准备作品。",
        PublishCheck::Blocked => "抖音发布页有待处理的提示、必填项或遮挡，请先在平台核对。",
        PublishCheck::UploadNotReady => "抖音尚未确认素材上传完成，请重新准备作品。",
        PublishCheck::ContentChanged => "抖音编辑页的标题或描述与任务记录不一致，请重新准备作品。",
        PublishCheck::PhotosChanged => "抖音编辑页的图片或封面已变化，请重新准备作品。",
        PublishCheck::SettingsChanged => {
            "抖音编辑页的发布时间或可见范围已变化，请先在创作者中心核对。"
        }
        PublishCheck::PublishUnavailable => {
            "未找到唯一可用的「发布」按钮，请先在抖音创作者中心核对。"
        }
    };
    Err(UserError(message.into()).into())
}

/// Check the live page without sending the publish action.
pub(crate) async fn preflight(
    page: &Page,
    content: &Content,
    photos: Option<&PhotoSnapshot>,
) -> Result<()> {
    inspect_target(page, CREATOR_ORIGIN, content, photos, false).await?;
    Ok(())
}

/// Recheck the live composer and dispatch a single left click to its publish button.
pub(crate) async fn click_publish(
    page: &Page,
    content: &Content,
    photos: Option<&PhotoSnapshot>,
) -> Result<()> {
    click_publish_at_origin(page, CREATOR_ORIGIN, content, photos).await
}

async fn click_publish_at_origin(
    page: &Page,
    origin: &str,
    content: &Content,
    photos: Option<&PhotoSnapshot>,
) -> Result<()> {
    let point = inspect_target(page, origin, content, photos, true)
        .await?
        .ok_or_else(|| UserError("无法定位抖音发布按钮。".into()))?;
    page.click(point)
        .await
        .map_err(|_| UserError("无法确认抖音是否收到发布操作，请到创作者中心核对。".into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{BrowserSession, fill_fields, truth};
    use crate::douyin::{photo_expression, select_photos};
    use publisher_core::Asset;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn fixture_server() -> Result<(String, tokio::task::JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            const BODY: &str = "<!doctype html><meta charset=utf-8><body></body>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                BODY.len()
            );
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0; 2048];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        Ok((origin, server))
    }

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn publish_click_requires_matching_content_and_one_uncovered_button() -> Result<()> {
        let (origin, server) = fixture_server().await?;
        let profile = tempfile::tempdir()?;
        let asset = profile.path().join("fixture.mp4");
        std::fs::write(&asset, b"fixture")?;
        let content = Content {
            asset: Asset::inspect(&asset)?,
            title: "短片".into(),
            description: "正文".into(),
            tags: vec!["测试".into()],
            douyin_video_covers: Default::default(),
        };
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/creator-micro/content/post/video"))
            .await?;
        page.set_content(
            r#"
            <input placeholder="填写作品标题">
            <div class="zone-container" contenteditable="true"></div>
            <button>重新上传视频</button>
            <div style="height:1800px"></div>
            <button id="publish">发布</button>
            <button disabled>发布</button>
            <button hidden>发布</button>
            <script>
              window.publishClicks = [];
              document.querySelector('#publish').addEventListener('click',
                (event) => window.publishClicks.push(event.isTrusted));
            </script>
            "#,
        )
        .await?;
        fill_fields(&page, &content.title, &prepared_description(&content)).await?;
        assert!(preflight(&page, &content, None).await.is_err());
        inspect_target(&page, &origin, &content, None, false).await?;
        assert!(
            truth(
                &page,
                "document.querySelector('#publish').getBoundingClientRect().top > innerHeight"
            )
            .await?
        );

        let mut changed = content.clone();
        changed.tags.push("另一项".into());
        assert!(
            click_publish_at_origin(&page, &origin, &changed, None)
                .await
                .is_err()
        );

        page.evaluate(
            r#"document.body.insertAdjacentHTML('beforeend', '<button id="duplicate">发布</button>')"#,
        )
        .await?;
        assert!(
            click_publish_at_origin(&page, &origin, &content, None)
                .await
                .is_err()
        );
        page.evaluate("document.querySelector('#duplicate').remove()")
            .await?;

        page.evaluate(
            r#"document.body.insertAdjacentHTML('beforeend', '<div id="overlay" style="position:fixed;inset:0;z-index:9999;background:#eee"></div>')"#,
        )
        .await?;
        assert!(
            click_publish_at_origin(&page, &origin, &content, None)
                .await
                .is_err()
        );
        page.evaluate("document.querySelector('#overlay').remove()")
            .await?;
        assert!(truth(&page, "window.publishClicks.length === 0").await?);

        click_publish_at_origin(&page, &origin, &content, None).await?;
        assert!(
            truth(
                &page,
                "window.publishClicks.length === 1 && window.publishClicks[0] === true"
            )
            .await?
        );
        browser.close().await?;
        server.abort();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn photo_upload_and_publish_keep_order_and_require_completed_images() -> Result<()> {
        let (origin, server) = fixture_server().await?;
        let profile = tempfile::tempdir()?;
        let first = profile.path().join("z-first.png");
        let second = profile.path().join("a-second.jpg");
        std::fs::write(&first, b"first")?;
        std::fs::write(&second, b"second")?;
        let content = Content {
            asset: Asset::inspect_images(&[first, second])?,
            title: "图文标题".into(),
            description: "首行\n次行".into(),
            tags: vec!["照片".into()],
            douyin_video_covers: Default::default(),
        };
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!(
                "{origin}/creator-micro/content/upload?default-tab=3"
            ))
            .await?;
        page.set_content(
            r#"
            <div class="tab-container-fixture">
              <div class="tab-item-fixture">发布视频</div>
              <div class="tab-item-fixture" id="photo-tab">发布图文</div>
            </div>
            <div class="container-drag-photos">
              <span>点击上传 或直接将图片文件拖入此区域</span>
              <input type="file" accept="image/jpeg,image/png" multiple style="display:none">
            </div>
            <div class="container-drag-another" hidden>
              <input type="file" accept="image/png" multiple>
            </div>
            <script>
              window.photoTabClicks = [];
              document.querySelector('#photo-tab').addEventListener('click', (event) => {
                photoTabClicks.push(event.isTrusted);
                document.querySelector('.container-drag-photos').hidden = false;
              });
              document.querySelector('input').addEventListener('change', ({target}) => {
                window.selectedFiles = [...target.files].map((file) => file.name);
                window.photoUrls = [...target.files].map((file) => URL.createObjectURL(file));
                history.pushState(null, '', '/creator-micro/content/post/image?media_type=image&type=new');
                document.body.innerHTML = `
                  <div id="grid">
                    <div id="first"><div class="img-photo"></div><div class="operator-button-photo" hidden>查看</div></div>
                    <div id="second"><div class="img-photo"></div><div class="operator-button-photo" hidden>查看</div></div>
                  </div>
                  <div class="info-photo">已添加2张图片</div>
                  <div class="mycard-photo"><div class="cover-photo"></div></div>
                  <input placeholder="添加作品标题">
                  <div class="zone-container" contenteditable="true"></div>
                  <button id="publish">发布</button>`;
                for (const [index, element] of [...document.querySelectorAll('.img-photo')].entries()) {
                  element.style.cssText = 'width:60px;height:60px;background-image:url(' + photoUrls[index] + ')';
                }
                document.querySelector('.cover-photo').style.cssText =
                  'width:60px;height:60px;background-image:url(' + photoUrls[0] + ')';
                window.publishClicks = [];
                document.querySelector('#publish').addEventListener('click',
                  (event) => publishClicks.push(event.isTrusted));
              });
            </script>
            "#,
        )
        .await?;
        let observe = |action, title, description| {
            let page = page.clone();
            let expression = photo_expression(&origin, 2, action, title, description);
            async move {
                Ok::<Value, anyhow::Error>(
                    page.evaluate_expression(expression).await?.into_value()?,
                )
            }
        };
        assert_eq!(observe("observe", "", "").await?["status"], "selectFiles");
        page.evaluate_expression(
            "history.replaceState(null, '', '/creator-micro/content/upload'); document.querySelector('.container-drag-photos').hidden = true",
        )
        .await?;
        assert_eq!(observe("observe", "", "").await?["status"], "waiting");
        let cancel = tokio_util::sync::CancellationToken::new();
        assert!(
            crate::douyin::wait_for_photo_upload_tab(
                &page,
                &origin,
                &cancel,
                std::time::Duration::from_secs(3)
            )
            .await?
        );
        assert!(
            truth(
                &page,
                "location.search === '' && JSON.stringify(photoTabClicks) === '[true]'"
            )
            .await?
        );
        assert!(
            select_photos(&page, "https://unrelated.invalid", &content.asset.images)
                .await
                .is_err()
        );
        select_photos(&page, &origin, &content.asset.images).await?;
        assert!(
            truth(
                &page,
                "JSON.stringify(selectedFiles) === '[\"z-first.png\",\"a-second.jpg\"]'"
            )
            .await?
        );
        assert_eq!(observe("observe", "", "").await?["status"], "ready");
        page.evaluate_expression(
            "document.querySelector('.info-photo').textContent = '已添加1张图片'",
        )
        .await?;
        assert_eq!(observe("observe", "", "").await?["status"], "countMismatch");
        page.evaluate_expression(
            "document.querySelector('.info-photo').textContent = '已添加2张图片'",
        )
        .await?;
        page.evaluate_expression("document.querySelector('#second').insertAdjacentHTML('beforeend', '<progress max=100 value=100></progress><div id=progress role=progressbar>100%</div>')")
            .await?;
        assert_eq!(observe("observe", "", "").await?["status"], "uploading");
        page.evaluate_expression(
            "document.querySelector('#progress').className = 'upload-opacity-failed'",
        )
        .await?;
        assert_eq!(observe("observe", "", "").await?["status"], "failed");
        page.evaluate_expression("document.querySelector('#progress').remove(); document.querySelector('progress').remove()")
            .await?;
        page.evaluate_expression(
            "document.querySelector('.cover-photo').style.backgroundImage = 'none'",
        )
        .await?;
        assert_eq!(observe("observe", "", "").await?["status"], "processing");
        page.evaluate_expression("document.querySelector('.cover-photo').style.backgroundImage = 'url(' + photoUrls[0] + ')'")
            .await?;
        let filled = observe("fill", &content.title, &prepared_description(&content)).await?;
        assert_eq!(filled["title"], json!(content.title));
        assert_eq!(filled["description"], json!(prepared_description(&content)));
        let photos: PhotoSnapshot = serde_json::from_value(filled["photos"].clone())?;
        inspect_target(&page, &origin, &content, Some(&photos), false).await?;

        page.evaluate_expression(
            "document.querySelector('#grid').prepend(document.querySelector('#second'))",
        )
        .await?;
        assert!(
            click_publish_at_origin(&page, &origin, &content, Some(&photos))
                .await
                .is_err()
        );
        page.evaluate_expression(
            "document.querySelector('#grid').append(document.querySelector('#second'))",
        )
        .await?;
        page.evaluate_expression("history.replaceState(null, '', '/creator-micro/content/post/image?media_type=image&type=edit')")
            .await?;
        assert!(
            click_publish_at_origin(&page, &origin, &content, Some(&photos))
                .await
                .is_err()
        );
        page.evaluate_expression("history.replaceState(null, '', '/creator-micro/content/publish-media/image-text?media_type=image&type=new')")
            .await?;
        assert!(truth(&page, "publishClicks.length === 0").await?);
        click_publish_at_origin(&page, &origin, &content, Some(&photos)).await?;
        assert!(truth(&page, "JSON.stringify(publishClicks) === '[true]'").await?);
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
