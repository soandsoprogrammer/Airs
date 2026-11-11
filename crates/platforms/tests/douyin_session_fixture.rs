use chromiumoxide::Page;
use publisher_platforms::browser::BrowserSession;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn creator_state(page: &Page, origin: &str) -> anyhow::Result<Value> {
    let script = include_str!("../src/douyin_creator_page.js");
    let origin = serde_json::to_string(origin)?;
    let file_selector = serde_json::to_string("input.upload-btn-input")?;
    let photo_selector = serde_json::to_string("input[type=file][multiple][accept*='image/']")?;
    Ok(page
        .evaluate(format!(
            "({script})({origin}, {file_selector}, {photo_selector})"
        ))
        .await?
        .into_value()?)
}

#[tokio::test]
#[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
async fn creator_page_distinguishes_session_from_interaction_required() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let Ok(length) = stream.read(&mut request).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&request[..length]);
                let target = request.split_whitespace().nth(1);
                if target == Some("/creator-micro/content/upload?fixture=signed-out") {
                    let _ = stream
                        .write_all(b"HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    return;
                }
                let (status, body) = match target {
                    Some("/creator-micro/home") => (
                        "200 OK",
                        "<div><span>本地昵称</span><span>抖音号：fixture123</span></div>",
                    ),
                    Some("/creator-micro/home?fixture=anonymous") => {
                        ("200 OK", "<h1>创作者中心</h1>")
                    }
                    Some("/creator-micro/content/upload") => (
                        "200 OK",
                        r#"<input class="upload-btn-input" type="file" accept="video/*">"#,
                    ),
                    Some("/creator-micro/content/upload?default-tab=3") => (
                        "200 OK",
                        r#"<input class="upload-btn-input" type="file" accept="video/*">
                           <div class="container-drag-photo">上传图片
                             <input type="file" accept="image/png" multiple hidden>
                           </div>"#,
                    ),
                    Some("/login") => (
                        "200 OK",
                        "<main><h1>登录抖音创作者中心</h1><p>扫码登录</p></main>",
                    ),
                    _ => ("404 Not Found", ""),
                };
                let html = format!("<!doctype html><meta charset=utf-8>{body}");
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    let profile = tempfile::tempdir()?;
    let mut browser = BrowserSession::launch(profile.path()).await?;
    let home = browser
        .browser
        .new_page(format!("{origin}/creator-micro/home"))
        .await?;
    let state = creator_state(&home, &origin).await?;
    assert_eq!(state["authenticated"], json!(true));
    assert_eq!(state["needsInteraction"], json!(false));
    assert_eq!(state["nickname"], json!("本地昵称"));
    assert_eq!(state["uploadReady"], json!(false));
    let foreign = creator_state(&home, "https://creator.douyin.com").await?;
    assert_eq!(foreign["authenticated"], json!(false));
    assert_eq!(foreign["needsInteraction"], json!(false));
    assert!(foreign["nickname"].is_null());

    let anonymous = browser
        .browser
        .new_page(format!("{origin}/creator-micro/home?fixture=anonymous"))
        .await?;
    let state = creator_state(&anonymous, &origin).await?;
    assert_eq!(state["authenticated"], json!(false));
    assert_eq!(state["needsInteraction"], json!(false));
    assert_eq!(state["canVerifyIdentity"], json!(true));
    assert!(state["nickname"].is_null());
    assert_eq!(state["uploadReady"], json!(false));

    home.evaluate(
        r#"document.body.insertAdjacentHTML('beforeend', '<div role="dialog" aria-modal="true"><h2>登录抖音创作者中心</h2><input placeholder="请输入手机号"><input placeholder="请输入验证码"></div>')"#,
    )
    .await?;
    let state = creator_state(&home, &origin).await?;
    assert_eq!(state["authenticated"], json!(false));
    assert_eq!(state["needsInteraction"], json!(true));

    let upload = browser
        .browser
        .new_page(format!("{origin}/creator-micro/content/upload"))
        .await?;
    let state = creator_state(&upload, &origin).await?;
    assert_eq!(state["uploadReady"], json!(true));
    assert_eq!(state["photoUploadReady"], json!(false));
    assert_eq!(state["needsInteraction"], json!(false));

    let photo_upload = browser
        .browser
        .new_page(format!(
            "{origin}/creator-micro/content/upload?default-tab=3"
        ))
        .await?;
    let state = creator_state(&photo_upload, &origin).await?;
    assert_eq!(state["authenticated"], json!(true));
    assert_eq!(state["uploadReady"], json!(false));
    assert_eq!(state["photoUploadReady"], json!(true));

    let redirected = browser
        .browser
        .new_page(format!(
            "{origin}/creator-micro/content/upload?fixture=signed-out"
        ))
        .await?;
    let state = creator_state(&redirected, &origin).await?;
    assert_eq!(state["authenticated"], json!(false));
    assert_eq!(state["needsInteraction"], json!(true));
    assert_eq!(state["uploadReady"], json!(false));

    for (message, expected) in [
        ("二维码失效", "expired"),
        ("扫码成功，请在手机上确认", "confirmOnPhone"),
        ("请完成安全验证", "verifyInApp"),
        ("登录失败", "failed"),
    ] {
        let message = serde_json::to_string(message)?;
        redirected
            .evaluate(format!(
                "document.body.textContent = '扫码登录'; document.body.append({message});"
            ))
            .await?;
        let state = creator_state(&redirected, &origin).await?;
        assert_eq!(state["qrStatus"], json!(expected));
        assert_eq!(state["needsInteraction"], json!(true));
        assert_eq!(state["canVerifyIdentity"], json!(false));
    }

    browser.close().await?;
    server.abort();
    Ok(())
}
