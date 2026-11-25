use crate::browser::{fill_fields, truth};
use anyhow::{Result, ensure};
use chromiumoxide::{Page, cdp::browser_protocol::input::InsertTextParams, layout::Point};
use publisher_core::{Content, UserError};
use std::time::Duration;
use tokio::time::{sleep, timeout};

pub(crate) const VIDEO_TEXT_CHECK: &str = include_str!("douyin_video_text.js");

const EDITOR: &str = r#"(origin) => {
  if (location.origin !== origin || !['/creator-micro/content/publish', '/creator-micro/content/post/video']
      .includes(location.pathname.replace(/\/$/, ''))) return null;
  const editors = [...document.querySelectorAll('div.zone-container[contenteditable="true"]')]
    .filter((editor) => editor.getClientRects().length && getComputedStyle(editor).visibility === 'visible');
  return editors.length === 1 ? editors[0] : null;
}"#;

const CANDIDATE: &str = r#"(origin, tag, getEditor) => {
  if (!getEditor(origin)) return [];
  const matches = [...document.querySelectorAll('span[class^="tag-hash-view-name-"]')]
    .filter((item) => item.getClientRects().length && getComputedStyle(item).visibility === 'visible' &&
      item.innerText.trim().toLowerCase() === tag.toLowerCase());
  if (matches.length !== 1) return [];
  const candidate = matches[0];
  candidate.scrollIntoView({block:'nearest', inline:'nearest'});
  const bounds = candidate.getBoundingClientRect();
  const x = bounds.left + bounds.width / 2;
  const y = bounds.top + bounds.height / 2;
  return candidate.contains(document.elementFromPoint(x, y)) ? [x, y] : [];
}"#;

pub(crate) async fn prepare_video_text(page: &Page, origin: &str, content: &Content) -> Result<()> {
    let origin = serde_json::to_string(origin)?;
    let has_editor = format!("Boolean(({EDITOR})({origin}))");
    if !truth(page, &has_editor).await? {
        return Err(UserError("无法确认当前是抖音视频编辑页，请重新准备作品。".into()).into());
    }
    let expected = serde_json::json!([content.title, content.description, content.tags]);
    let matches = format!(
        r#"(() => {{
          const [title, description, tags] = {expected};
          const editor = ({EDITOR})({origin});
          return document.querySelector('input[placeholder*="填写作品标题"]')?.value === title &&
            !!editor && ({VIDEO_TEXT_CHECK})(editor, description, tags) === 'ready';
        }})()"#
    );
    if truth(page, &matches).await? {
        return Ok(());
    }
    fill_video_description(page, &origin, content)
        .await
        .map_err(|_| UserError("抖音视频编辑页的标题或描述填写失败，请重新准备作品。".into()))?;

    for (index, tag) in content.tags.iter().enumerate() {
        let add_space = index > 0 || !content.description.trim().is_empty();
        if add_topic(page, &origin, tag, add_space).await.is_err()
            || verify_text(page, &origin, &content.description, &content.tags[..=index])
                .await
                .is_err()
        {
            return Err(UserError(format!(
                "抖音未能关联话题「{tag}」，请核对话题名称后重新准备。"
            ))
            .into());
        }
    }
    Ok(())
}

async fn fill_video_description(page: &Page, origin: &str, content: &Content) -> Result<()> {
    let description = content
        .description
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines = description.split('\n');
    fill_fields(page, &content.title, lines.next().unwrap_or_default()).await?;
    for line in lines {
        move_caret_to_end(page, origin).await?;
        page.find_element("div.zone-container[contenteditable=\"true\"]")
            .await?
            .press_key("Enter")
            .await?;
        if !line.is_empty() {
            page.execute(InsertTextParams::new(line)).await?;
        }
    }
    verify_text(page, origin, &description, &[]).await
}

async fn move_caret_to_end(page: &Page, origin: &str) -> Result<()> {
    let focus = format!(
        r#"(() => {{
          const editor = ({EDITOR})({origin});
          if (!editor) return false;
          editor.focus();
          const line = [...editor.querySelectorAll('[data-line-wrapper]')].at(-1) ?? editor;
          const tail = [...line.querySelectorAll('[data-string]:not([data-enter])')].at(-1)
            ?? line.querySelector('[data-string][data-enter]');
          const range = document.createRange();
          if (tail?.firstChild?.nodeType === Node.TEXT_NODE) {{
            range.setStart(tail.firstChild, tail.hasAttribute('data-enter') ? 0 : tail.firstChild.length);
            range.collapse(true);
          }} else {{
            range.selectNodeContents(tail ?? line);
            range.collapse(false);
          }}
          const selection = getSelection();
          if (!selection || document.activeElement !== editor) return false;
          selection.removeAllRanges();
          selection.addRange(range);
          return true;
        }})()"#,
    );
    ensure!(truth(page, &focus).await?, "无法聚焦抖音视频描述");
    Ok(())
}

async fn add_topic(page: &Page, origin: &str, tag: &str, add_space: bool) -> Result<()> {
    move_caret_to_end(page, origin).await?;
    if add_space {
        let already_separated =
            format!(r#"/\s$/.test(({EDITOR})({origin})?.innerText.replace(/\u200b/g,'') ?? '')"#);
        if !truth(page, &already_separated).await? {
            page.execute(InsertTextParams::new(" ")).await?;
            move_caret_to_end(page, origin).await?;
        }
    }
    page.execute(InsertTextParams::new("#")).await?;
    sleep(Duration::from_millis(400)).await;
    for character in tag.chars() {
        page.execute(InsertTextParams::new(character.to_string()))
            .await?;
        sleep(Duration::from_millis(70)).await;
    }

    let tag = serde_json::to_string(tag)?;
    let expression = format!("({CANDIDATE})({origin}, {tag}, ({EDITOR}))");
    let (x, y) = timeout(Duration::from_secs(6), async {
        loop {
            let point: Vec<f64> = page
                .evaluate_expression(expression.as_str())
                .await?
                .into_value()?;
            if let [x, y] = point.as_slice() {
                return Ok::<_, anyhow::Error>((*x, *y));
            }
            sleep(Duration::from_millis(150)).await;
        }
    })
    .await??;
    page.click(Point { x, y }).await?;
    Ok(())
}

async fn verify_text(page: &Page, origin: &str, description: &str, tags: &[String]) -> Result<()> {
    let description = serde_json::to_string(description)?;
    let tags = serde_json::to_string(tags)?;
    let expression = format!(
        r#"(() => {{
          const editor = ({EDITOR})({origin});
          return !!editor && ({VIDEO_TEXT_CHECK})(editor, {description}, {tags}) === 'ready';
        }})()"#,
    );
    timeout(Duration::from_secs(2), async {
        while !truth(page, &expression).await? {
            sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
