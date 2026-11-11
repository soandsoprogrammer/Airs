use publisher_platforms::browser::*;
#[tokio::test]
#[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
async fn cdp_upload_fill_dialog_and_cleanup_without_submission() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let profile = tmp.path().join("profile");
    std::fs::create_dir(&profile)?;
    let video = tmp.path().join("fixture.mp4");
    std::fs::write(&video, b"local fixture only")?;
    let mut session = BrowserSession::launch(&profile).await?;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/upload.html")
        .canonicalize()?;
    let page = session
        .browser
        .new_page(format!("file://{}", fixture.display()))
        .await?;
    upload_file(&page, "input[type=file]", &video).await?;
    assert!(truth(&page, "document.querySelector('#dialog').open").await?);
    page.evaluate("document.querySelector('#dialog').close()")
        .await?;
    fill_fields(&page, "中文标题 ' \" 测试", "中文描述 #测试").await?;
    assert!(
        publisher_platforms::douyin::read_nickname(&page)
            .await?
            .is_none()
    );
    page.evaluate(r#"document.body.insertAdjacentHTML('beforeend','<div role="menuitemradio" aria-checked="true"><span class="account-name-fixture">平台昵称测试</span></div>')"#).await?;
    assert_eq!(
        publisher_platforms::douyin::read_nickname(&page)
            .await?
            .as_deref(),
        Some("平台昵称测试")
    );
    assert!(truth(&page,"document.querySelector('input[type=file]').files.length===1 && submitted===0 && document.querySelector('#result').textContent==='上传已接收'").await?);
    session.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
async fn profiles_preserve_session_and_isolate_same_site_accounts() -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let body = "<!doctype html><title>Local profile fixture</title>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: text/html\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    let tmp = tempfile::tempdir()?;
    let a = tmp.path().join("account-a");
    let b = tmp.path().join("account-b");
    std::fs::create_dir_all(&a)?;
    std::fs::create_dir_all(&b)?;
    let mut first = BrowserSession::launch(&a).await?;
    let page = first.browser.new_page(url.clone()).await?;
    page.evaluate("document.cookie='profile_marker=account_a; max-age=3600; path=/'")
        .await?;
    first.close().await?;
    let mut restored = BrowserSession::launch(&a).await?;
    let page = restored.browser.new_page(url.clone()).await?;
    assert!(
        truth(
            &page,
            "document.cookie.includes('profile_marker=account_a')"
        )
        .await?
    );
    let mut second = BrowserSession::launch(&b).await?;
    let other = second.browser.new_page(url).await?;
    assert!(truth(&other, "!document.cookie.includes('profile_marker')").await?);
    second.close().await?;
    restored.close().await?;
    server.abort();
    Ok(())
}
