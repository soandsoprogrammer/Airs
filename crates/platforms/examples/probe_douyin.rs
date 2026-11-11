//! Check whether a fresh background session can recognize Douyin's sign-in screen.
use publisher_platforms::{browser::BrowserSession, douyin::read_login_qr};
use serde_json::Value;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let profile = tempfile::tempdir()?;
    let mut browser = BrowserSession::launch(profile.path()).await?;
    let result = async {
        let page = browser
            .browser
            .new_page("https://creator.douyin.com/creator-micro/home")
            .await?;
        let script = format!(
            r#"({})("https://creator.douyin.com", "input.upload-btn-input")"#,
            include_str!("../src/douyin_creator_page.js")
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let state: Value = page
                    .evaluate_expression(script.as_str())
                    .await?
                    .into_value()?;
                anyhow::ensure!(state["authenticated"] != true, "空会话不应识别为已登录");
                if state["needsInteraction"] == true && read_login_qr(&page).await?.is_some() {
                    println!("抖音空会话已在后台取得登录二维码");
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await?
    }
    .await;
    browser.close().await?;
    result
}
