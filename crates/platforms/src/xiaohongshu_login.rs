use super::{CREATOR_ORIGIN, HOME_URL};
use crate::{
    douyin::read_verification_frame,
    sign_in::{SignInGuard, SignInStatus, VerificationAction},
};
use anyhow::Result;
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{
        input::InsertTextParams,
        page::{CaptureScreenshotFormat, Viewport},
    },
    layout::Point,
    page::ScreenshotParams,
};
use publisher_core::UserError;
use serde::Deserialize;
use std::time::Duration;
use tokio::time::Instant;

const PAGE: &str = include_str!("xiaohongshu_login.js");
const QR_WAIT: Duration = Duration::from_secs(8);
const PAGE_WAIT: Duration = Duration::from_secs(35);

#[derive(Debug, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum State {
    Creator {
        nickname: String,
    },
    Login {
        switch_available: bool,
        switch_point: Option<[f64; 2]>,
    },
    Scan {
        expired: bool,
        image: Option<QrImage>,
    },
    Restricted,
    Pending,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
struct QrImage {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Default)]
struct ScanAttempt {
    until: Option<Instant>,
    switched: bool,
    unavailable: bool,
}

async fn observe(page: &Page, origin: &str) -> Result<State> {
    Ok(page
        .evaluate_expression(format!("({PAGE})({origin:?})"))
        .await?
        .into_value()?)
}

async fn capture_qr(page: &Page, origin: &str, image: QrImage) -> Result<Option<Vec<u8>>> {
    let png = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .clip(Viewport {
                    x: image.x,
                    y: image.y,
                    width: image.width,
                    height: image.height,
                    scale: 1.0,
                })
                .capture_beyond_viewport(false)
                .build(),
        )
        .await?;
    Ok(matches!(observe(page, origin).await?, State::Scan {
        expired: false,
        image: Some(current)
    } if current == image)
    .then_some(png))
}

pub(super) async fn wait_for_login(
    page: &Page,
    sign_in: &SignInGuard,
    on_needs_login: impl FnOnce() -> Result<()>,
) -> Result<String> {
    let mut notify_login = Some(on_needs_login);
    let mut viewport = None;
    let mut scan = ScanAttempt::default();
    let mut deadline = Instant::now() + PAGE_WAIT;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = sign_in.refresh_requested() => {
                viewport = None;
                scan = ScanAttempt::default();
                sign_in.update(SignInStatus::Loading, None);
                page.goto(HOME_URL).await.map_err(|_| UserError("无法刷新小红书登录页，请检查网络后重试".into()))?;
                deadline = Instant::now() + PAGE_WAIT;
            }
            Some(action) = sign_in.verification_action() => {
                if let Some(size) = viewport
                    && matches!(observe(page, CREATOR_ORIGIN).await, Ok(State::Login { .. }))
                {
                    if let Some(feedback) = forward_action(page, size, action).await {
                        sign_in.set_input_feedback(Some(feedback));
                    }
                    ticker.reset_immediately();
                }
            }
            _ = ticker.tick() => {
                let current = observe(page, CREATOR_ORIGIN).await;
                if let Ok(State::Creator { nickname }) = current {
                    return Ok(nickname);
                }
                if !scan.unavailable
                    && (scan.until.is_some_and(|until| Instant::now() >= until)
                        || (scan.switched && matches!(current, Ok(State::Restricted))))
                {
                    scan.unavailable = true;
                    if scan.switched || !matches!(current, Ok(State::Login { .. })) {
                        sign_in.update(SignInStatus::Loading, None);
                        page.goto(HOME_URL).await.map_err(|_| UserError("无法打开小红书短信登录页，请检查网络后重试".into()))?;
                        deadline = Instant::now() + PAGE_WAIT;
                        continue;
                    }
                }
                match current {
                    Ok(State::Login { switch_available, switch_point }) => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        if !scan.unavailable && switch_available {
                            scan.until.get_or_insert_with(|| Instant::now() + QR_WAIT);
                            if !scan.switched && let Some([x, y]) = switch_point {
                                scan.switched = true;
                                let _ = page.click(Point::new(x, y)).await;
                            }
                            continue;
                        }
                        if let Ok((png, width, height)) = read_verification_frame(page).await
                            && matches!(observe(page, CREATOR_ORIGIN).await, Ok(State::Login { .. }))
                        {
                            sign_in.update_verification(png, width, height);
                            if viewport.is_none() {
                                let feedback = if scan.unavailable {
                                    "扫码暂不可用。可在平台登录页使用短信，或切换网络后重试扫码。"
                                } else {
                                    "先在平台登录页点击输入框，再输入手机号或验证码。"
                                };
                                sign_in.set_input_feedback(Some(feedback));
                            }
                            viewport = Some((width, height));
                        }
                        deadline = Instant::now() + PAGE_WAIT;
                    }
                    Ok(State::Scan { expired, image }) if !scan.unavailable => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        if expired {
                            scan.until = None;
                            sign_in.update(SignInStatus::Expired, None);
                        } else if let Some(image) = image
                            && let Ok(Some(png)) = capture_qr(page, CREATOR_ORIGIN, image).await
                        {
                            scan.until = None;
                            sign_in.update(SignInStatus::WaitingForScan, Some(png));
                        } else {
                            scan.until.get_or_insert_with(|| Instant::now() + QR_WAIT);
                        }
                        deadline = Instant::now() + PAGE_WAIT;
                    }
                    _ => {
                        if viewport.take().is_some() {
                            sign_in.update(SignInStatus::CheckingAccount, None);
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Err(UserError("暂未识别到小红书账号或登录页面，请检查网络后重试".into()).into());
                }
            }
        }
    }
}

async fn forward_action(
    page: &Page,
    (width, height): (u32, u32),
    action: VerificationAction,
) -> Option<&'static str> {
    match action {
        VerificationAction::Click { x, y }
            if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) =>
        {
            let _ = page
                .click(Point::new(x * f64::from(width), y * f64::from(height)))
                .await;
            None
        }
        VerificationAction::FillText(text) => {
            let value = text.trim();
            if value.is_empty()
                || value.len() > 20
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Some("请填写数字格式的手机号或验证码。");
            }
            let field = page
                .evaluate_expression(format!("({PAGE})({CREATOR_ORIGIN:?}, true)"))
                .await;
            if !field.is_ok_and(|result| result.into_value::<bool>().unwrap_or(false)) {
                return Some("请先点击小红书页面的手机号或验证码输入框。");
            }
            Some(
                if page.execute(InsertTextParams::new(value)).await.is_ok() {
                    "已填写，请在小红书登录页面核对后继续。"
                } else {
                    "填写未完成，请重新点击输入框后再试。"
                },
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserSession;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn official_switch_is_hit_tested_and_the_qr_is_cropped() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0; 1024];
                if socket.read(&mut request).await.is_ok() {
                    let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 15\r\nConnection: close\r\n\r\n<!doctype html>").await;
                }
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser.browser.new_page(format!("{origin}/login")).await?;
        page.set_content(
            r#"
            <div class="login-box-container">
              <div class="sso-login-wrapper">
                <input placeholder="手机号">
                <input placeholder="验证码">
                <img class="css-wemwzq" style="width:64px;height:64px;clip-path:inset(0 50% 0 0)"
                     src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='64' height='64'/%3E">
              </div>
            </div>
            <script>
              const box = document.querySelector('.login-box-container');
              document.querySelector('.css-wemwzq').addEventListener('click', event => {
                if (!event.isTrusted) return;
                box.innerHTML = `<div><span>APP扫一扫登录</span></div><div>
                  <img style="width:152px;height:152px" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='152' height='152'%3E%3Crect width='152' height='152' fill='black'/%3E%3C/svg%3E">
                </div>`;
              });
            </script>
        "#,
        )
        .await?;
        assert!(matches!(
            observe(&page, CREATOR_ORIGIN).await?,
            State::Pending
        ));
        let State::Login {
            switch_available: true,
            switch_point: Some([x, y]),
        } = observe(&page, &origin).await?
        else {
            anyhow::bail!("fixture official QR switch was not detected");
        };
        page.click(Point::new(x, y)).await?;
        page.evaluate_expression("document.querySelector('img').decode()")
            .await?;
        let State::Scan {
            expired: false,
            image: Some(image),
        } = observe(&page, &origin).await?
        else {
            anyhow::bail!("fixture QR image was not detected");
        };
        let png = capture_qr(&page, &origin, image)
            .await?
            .ok_or_else(|| anyhow::anyhow!("fixture QR image was not captured"))?;
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[16..20], &152_u32.to_be_bytes());
        assert_eq!(&png[20..24], &152_u32.to_be_bytes());
        page.evaluate_expression(
            r#"document.querySelector('.login-box-container').innerHTML =
            '<div>APP扫一扫登录</div><div>二维码已过期</div>'"#,
        )
        .await?;
        assert!(matches!(
            observe(&page, &origin).await?,
            State::Scan {
                expired: true,
                image: None
            }
        ));
        page.evaluate_expression(
            r#"document.querySelector('.login-box-container').innerHTML =
            '<div class="sso-login-wrapper"><input placeholder="手机号"></div>'"#,
        )
        .await?;
        assert!(matches!(
            observe(&page, &origin).await?,
            State::Login {
                switch_available: false,
                switch_point: None
            }
        ));
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
