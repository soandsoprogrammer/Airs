use super::{CREATOR_ORIGIN, HOME_URL};
use crate::sign_in::{
    SignInGuard, SignInStatus, VerificationAction, WechatChannelChoice, WechatChannelsPreview,
};
use anyhow::{Result, ensure};
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::{
            dom::DescribeNodeParams,
            page::{CaptureScreenshotFormat, FrameId, Viewport},
        },
        js_protocol::runtime::{EvaluateParams, ReleaseObjectParams},
    },
    layout::Point,
    page::ScreenshotParams,
};
use publisher_core::UserError;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior, interval};

const PROBE: &str = include_str!("wechat_channels_login.js");
const OAUTH_ORIGIN: &str = "https://open.weixin.qq.com";

#[derive(Clone, Deserialize)]
pub(super) struct Identity {
    pub(super) nickname: Option<String>,
    avatar: Option<String>,
}

impl Identity {
    fn matches(&self, current: &Self) -> bool {
        if let (Some(expected), Some(avatar)) = (&self.avatar, &current.avatar)
            && expected != avatar
        {
            return false;
        }
        match (&self.nickname, &current.nickname) {
            (Some(expected), Some(nickname)) => expected == nickname,
            _ => self.avatar.is_some() && self.avatar == current.avatar,
        }
    }
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
struct Region {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize, PartialEq, Eq, Serialize)]
struct ChannelRow {
    #[serde(flatten)]
    choice: WechatChannelChoice,
    avatar: Option<String>,
}

impl Region {
    fn within(self, frame: Self) -> Option<Self> {
        (self.x >= 0.
            && self.y >= 0.
            && self.x + self.width <= frame.width + 1.
            && self.y + self.height <= frame.height + 1.)
            .then_some(Self {
                x: frame.x + self.x,
                y: frame.y + self.y,
                ..self
            })
    }

    fn is_qr(self) -> bool {
        (90. ..=520.).contains(&self.width)
            && (90. ..=520.).contains(&self.height)
            && (0.9..=1.1).contains(&(self.width / self.height))
            && self.x.is_finite()
            && self.y.is_finite()
            && self.x >= 0.
            && self.y >= 0.
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum State {
    Creator {
        #[serde(flatten)]
        identity: Identity,
    },
    Scan {
        image: Option<Region>,
        frame: Option<Region>,
    },
    SwitchToScan {
        control: Region,
    },
    Choice {
        nickname: String,
        quick: Region,
        scan: Region,
    },
    SelectChannel {
        rows: Vec<ChannelRow>,
    },
    SelectingChannel,
    ConfirmOnPhone,
    Expired,
    NoAccount,
    Pending,
}

fn expression(origin: &str, oauth_origin: &str, frame_only: bool) -> String {
    let args = serde_json::json!([origin, oauth_origin, frame_only]);
    format!("({PROBE})(...{args})")
}

async fn observe(page: &Page, origin: &str, oauth_origin: &str) -> Result<State> {
    Ok(page
        .evaluate_expression(expression(origin, oauth_origin, false))
        .await?
        .into_value()?)
}

async fn frame_state(
    page: &Page,
    origin: &str,
    oauth_origin: &str,
    bounds: Region,
) -> Result<State> {
    let x = bounds.x + bounds.width / 2.;
    let y = bounds.y + bounds.height / 2.;
    let mut request = EvaluateParams::new(format!(
        "document.elementFromPoint({x} - scrollX, {y} - scrollY)"
    ));
    request.return_by_value = Some(false);
    let hit = page.evaluate_expression(request).await?;
    let Some(object_id) = hit.object().object_id.clone() else {
        return Ok(State::Pending);
    };
    let owner = page
        .execute(
            DescribeNodeParams::builder()
                .object_id(object_id.clone())
                .depth(0)
                .build(),
        )
        .await;
    let _ = page.execute(ReleaseObjectParams::new(object_id)).await;
    let Some(frame): Option<FrameId> = owner?.result.node.frame_id else {
        return Ok(State::Pending);
    };
    let Some(context_id) = page.frame_execution_context(frame).await? else {
        return Ok(State::Pending);
    };
    let mut request = EvaluateParams::new(expression(origin, oauth_origin, true));
    request.context_id = Some(context_id);
    Ok(page.evaluate_expression(request).await?.into_value()?)
}

async fn observe_with_frame(page: &Page, origin: &str, oauth_origin: &str) -> Result<State> {
    let state = observe(page, origin, oauth_origin).await?;
    if let State::Scan {
        image: None,
        frame: Some(frame),
    } = state
    {
        return Ok(
            match frame_state(page, origin, oauth_origin, frame).await? {
                State::Scan {
                    image: Some(image), ..
                } => State::Scan {
                    image: image.within(frame),
                    frame: Some(frame),
                },
                State::SwitchToScan { control } => control
                    .within(frame)
                    .map_or(State::Pending, |control| State::SwitchToScan { control }),
                State::Choice {
                    nickname,
                    quick,
                    scan,
                } => match (quick.within(frame), scan.within(frame)) {
                    (Some(quick), Some(scan)) => State::Choice {
                        nickname,
                        quick,
                        scan,
                    },
                    (None, Some(control)) => State::SwitchToScan { control },
                    _ => State::Pending,
                },
                state => state,
            },
        );
    }
    Ok(state)
}

async fn click_control(
    page: &Page,
    origin: &str,
    oauth_origin: &str,
    control: Region,
) -> Result<()> {
    let args = serde_json::json!([
        origin,
        oauth_origin,
        control.x + control.width / 2.,
        control.y + control.height / 2.,
    ]);
    let point: Option<(f64, f64)> = page
        .evaluate_expression(format!(
            r#"((origin, oauthOrigin, x, y) => {{
                if (location.origin !== origin) return null;
                x -= scrollX;
                y -= scrollY;
                const frame = document.elementFromPoint(x, y);
                if (frame?.tagName !== 'IFRAME' || !frame.closest('.login-qrcode-wrap')) return null;
                const source = new URL(frame.src);
                return source.origin === oauthOrigin && source.pathname === '/connect/qrconnect'
                    ? [x, y] : null;
            }})(...{args})"#
        ))
        .await?
        .into_value()?;
    let Some((x, y)) = point else {
        anyhow::bail!("WeChat login control is no longer visible");
    };
    page.click(Point { x, y }).await?;
    Ok(())
}

enum LoginMode {
    Choose {
        offered: Option<String>,
    },
    Desktop {
        retry_after: Instant,
    },
    Scan {
        switched: bool,
    },
    SelectChannel {
        offered: Vec<ChannelRow>,
        retry_at: Option<Instant>,
    },
}

async fn select_channel(
    page: &Page,
    origin: &str,
    oauth_origin: &str,
    rows: &[ChannelRow],
    index: usize,
) -> Result<bool> {
    let args = serde_json::json!([origin, oauth_origin, false, { "rows": rows, "index": index }]);
    let state: State = page
        .evaluate_expression(format!("({PROBE})(...{args})"))
        .await?
        .into_value()?;
    Ok(matches!(state, State::SelectingChannel))
}

async fn reload_login(page: &Page) -> Result<()> {
    page.goto(HOME_URL)
        .await
        .map_err(|_| UserError("无法刷新视频号助手登录页，请检查网络后重试".into()))?;
    Ok(())
}

async fn capture_qr(
    page: &Page,
    origin: &str,
    oauth_origin: &str,
    image: Region,
) -> Result<Option<Vec<u8>>> {
    if !image.is_qr() {
        return Ok(None);
    }
    let png = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .clip(Viewport {
                    x: image.x,
                    y: image.y,
                    width: image.width,
                    height: image.height,
                    scale: 1.,
                })
                .capture_beyond_viewport(false)
                .build(),
        )
        .await?;
    Ok(matches!(
        observe_with_frame(page, origin, oauth_origin).await?,
        State::Scan { image: Some(current), .. } if current == image
    )
    .then_some(png))
}

pub(super) async fn wait_for_login(
    page: &Page,
    sign_in: &SignInGuard,
    on_needs_login: impl FnOnce() -> Result<()>,
) -> Result<Identity> {
    let mut notify_login = Some(on_needs_login);
    let mut mode = LoginMode::Choose { offered: None };
    let mut deadline = Instant::now() + Duration::from_secs(45);
    let mut ticker = interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = sign_in.refresh_requested() => {
                sign_in.update(SignInStatus::Loading, None);
                reload_login(page).await?;
                mode = if matches!(mode, LoginMode::Scan { .. }) {
                    LoginMode::Scan { switched: false }
                } else {
                    LoginMode::Choose { offered: None }
                };
                deadline = Instant::now() + Duration::from_secs(45);
                ticker.reset_immediately();
            }
            Some(action) = sign_in.verification_action() => {
                match action {
                    VerificationAction::WechatQuickLogin => {
                        if let LoginMode::Choose { offered: Some(expected) } = &mode
                            && let Ok(State::Choice { nickname, quick, .. }) =
                                observe_with_frame(page, CREATOR_ORIGIN, OAUTH_ORIGIN).await
                            && &nickname == expected
                        {
                            sign_in.update_wechat_channels(WechatChannelsPreview::WaitingForDesktop);
                            if click_control(page, CREATOR_ORIGIN, OAUTH_ORIGIN, quick).await.is_ok() {
                                mode = LoginMode::Desktop {
                                    retry_after: Instant::now() + Duration::from_secs(5),
                                };
                                deadline = Instant::now() + Duration::from_secs(60);
                            } else {
                                sign_in.update(SignInStatus::Loading, None);
                            }
                        } else {
                            sign_in.update(SignInStatus::Loading, None);
                        }
                    }
                    VerificationAction::WechatScanLogin => {
                        sign_in.update(SignInStatus::Loading, None);
                        if matches!(mode, LoginMode::Desktop { .. }) {
                            reload_login(page).await?;
                        }
                        mode = LoginMode::Scan { switched: false };
                        deadline = Instant::now() + Duration::from_secs(45);
                    }
                    VerificationAction::WechatSelectChannel { index } => {
                        if let LoginMode::SelectChannel { offered, retry_at } = &mut mode {
                            sign_in.update(SignInStatus::Loading, None);
                            if select_channel(page, CREATOR_ORIGIN, OAUTH_ORIGIN, offered, index)
                                .await.unwrap_or(false)
                            {
                                *retry_at = Some(Instant::now() + Duration::from_secs(8));
                                deadline = Instant::now() + Duration::from_secs(45);
                            }
                        }
                    }
                    _ => {}
                }
                ticker.reset_immediately();
            }
            _ = ticker.tick() => {
                let state = observe_with_frame(page, CREATOR_ORIGIN, OAUTH_ORIGIN)
                    .await.unwrap_or(State::Pending);
                match state {
                    State::Creator { identity } => return Ok(identity),
                    State::SelectChannel { rows } => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        if let LoginMode::SelectChannel { offered, retry_at } = &mode {
                            if retry_at.is_some_and(|time| Instant::now() < time) {
                                continue;
                            }
                            if *offered != rows {
                                sign_in.update(SignInStatus::Loading, None);
                            }
                        }
                        let channels = rows.iter().map(|row| row.choice.clone()).collect();
                        mode = LoginMode::SelectChannel { offered: rows, retry_at: None };
                        sign_in.update_wechat_channels(WechatChannelsPreview::SelectChannel { channels });
                        deadline = Instant::now() + Duration::from_secs(45);
                    }
                    State::Choice { nickname, .. }
                        if matches!(mode, LoginMode::Choose { .. })
                            || matches!(mode, LoginMode::Desktop { retry_after } if Instant::now() >= retry_after) =>
                    {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        mode = LoginMode::Choose { offered: Some(nickname.clone()) };
                        sign_in.update_wechat_channels(WechatChannelsPreview::Choice { nickname });
                        deadline = Instant::now() + Duration::from_secs(45);
                    }
                    State::SwitchToScan { control } | State::Choice { scan: control, .. }
                        if matches!(mode, LoginMode::Choose { .. } | LoginMode::Scan { switched: false }) =>
                    {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        sign_in.update(SignInStatus::Loading, None);
                        if click_control(page, CREATOR_ORIGIN, OAUTH_ORIGIN, control).await.is_ok() {
                            mode = LoginMode::Scan { switched: true };
                            deadline = Instant::now() + Duration::from_secs(45);
                        }
                    }
                    State::Scan { image: Some(image), .. } => {
                        if let Ok(Some(png)) = capture_qr(page, CREATOR_ORIGIN, OAUTH_ORIGIN, image).await {
                            if let Some(notify) = notify_login.take() {
                                notify()?;
                            }
                            if !matches!(mode, LoginMode::Choose { .. }) {
                                mode = LoginMode::Scan { switched: true };
                            }
                            sign_in.update(SignInStatus::WaitingForScan, Some(png));
                            deadline = Instant::now() + Duration::from_secs(45);
                        }
                    }
                    State::ConfirmOnPhone | State::Expired => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        let status = if matches!(state, State::ConfirmOnPhone) {
                            mode = LoginMode::Scan { switched: true };
                            SignInStatus::ConfirmOnPhone
                        } else {
                            SignInStatus::Expired
                        };
                        sign_in.update(status, None);
                        deadline = Instant::now() + Duration::from_secs(45);
                    }
                    State::NoAccount => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        return Err(UserError("该微信没有可管理的视频号，请核对账号授权。".into()).into());
                    }
                    _ => {}
                }
                if Instant::now() >= deadline {
                    if matches!(mode, LoginMode::Desktop { .. }) {
                        sign_in.update(SignInStatus::Loading, None);
                        reload_login(page).await?;
                        mode = LoginMode::Scan { switched: false };
                        deadline = Instant::now() + Duration::from_secs(45);
                        continue;
                    }
                    return Err(UserError("视频号助手未显示可用的账号或登录二维码，请检查网络后重试。".into()).into());
                }
            }
        }
    }
}

pub(super) async fn verify_identity(page: &Page, expected: &Identity) -> Result<()> {
    let matches = matches!(
        observe(page, CREATOR_ORIGIN, OAUTH_ORIGIN).await,
        Ok(State::Creator { identity }) if expected.matches(&identity)
    );
    ensure!(
        matches,
        UserError("当前视频号账号无法核对，请重新准备任务。".into())
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserSession;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::{sleep, timeout},
    };

    async fn serve(body: String) -> Result<(String, JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 2048];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        Ok((origin, task))
    }

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn offers_desktop_login_and_qr_then_checks_the_selected_account() -> Result<()> {
        let (oauth, frame_server) = serve(
            r#"<div class="js_quick_login web_qrcode_panel_quick_login">
                  <div class="js_quick_login_nickname">已扫码测试账号</div>
                  <button class="js_quick_login_btn qlogin_btn">微信快捷登录</button>
                  <button class="js_switchToNormal web_qrcode_switch">使用其他头像、昵称或账号</button>
                </div>
                <div class="js_normal_login" hidden>
                  <img class="js_qrcode_img" width="136" height="136">
                </div>
                <script>
                const image = document.querySelector('img');
                const canvas = document.createElement('canvas');
                canvas.width = canvas.height = 136;
                const ctx = canvas.getContext('2d');
                ctx.fillStyle = '#212121';
                ctx.fillRect(0, 0, 136, 136);
                image.src = canvas.toDataURL('image/png');
                document.querySelector('.js_quick_login_btn').onclick = event => {
                  if (event.isTrusted) parent.postMessage('desktop-login', '*');
                };
                document.querySelector('.js_switchToNormal').onclick = event => {
                  if (!event.isTrusted) return;
                  document.querySelector('.js_quick_login').hidden = true;
                  document.querySelector('.js_normal_login').hidden = false;
                };
                </script>"#
                .into(),
        )
        .await?;
        let (origin, server) = serve(format!(
            r#"<div class="login-qrcode-wrap">
              <iframe src="{oauth}/connect/qrconnect" hidden></iframe>
              <iframe src="{oauth}/connect/qrconnect" width="300" height="240" style="border:0"></iframe>
            </div>
            <script>addEventListener('message', event => {{window.loginMessage = event.data;}});</script>"#
        ))
        .await?;
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/login.html"))
            .await?;
        let (quick, scan) = timeout(Duration::from_secs(10), async {
            loop {
                if let State::Choice {
                    nickname,
                    quick,
                    scan,
                } = observe_with_frame(&page, &origin, &oauth).await?
                {
                    assert_eq!(nickname, "已扫码测试账号");
                    return Ok::<_, anyhow::Error>((quick, scan));
                }
                sleep(Duration::from_millis(150)).await;
            }
        })
        .await??;
        assert!(
            page.evaluate_expression("window.loginMessage === undefined")
                .await?
                .into_value::<bool>()?
        );
        click_control(&page, &origin, &oauth, quick).await?;
        timeout(Duration::from_secs(3), async {
            loop {
                if page
                    .evaluate_expression("window.loginMessage === 'desktop-login'")
                    .await?
                    .into_value::<bool>()?
                {
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await??;
        click_control(&page, &origin, &oauth, scan).await?;
        let png = timeout(Duration::from_secs(10), async {
            loop {
                if let State::Scan {
                    image: Some(image), ..
                } = observe_with_frame(&page, &origin, &oauth).await?
                    && let Some(png) = capture_qr(&page, &origin, &oauth, image).await?
                {
                    return Ok::<_, anyhow::Error>(png);
                }
                sleep(Duration::from_millis(150)).await;
            }
        })
        .await??;
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(u32::from_be_bytes(png[16..20].try_into()?), 136);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into()?), 136);

        page.evaluate_expression(r#"
            document.body.innerHTML = `<div class="login-qrcode-wrap"><div class="choose-finder-area">
              <div class="choose-header"><div class="choose-title">选择视频号登录</div></div>
              <div class="finder-list-wrap"><div class="finder-list">
                <div class="finder-item baned"><div class="right-part blocked"><div class="info"><div class="name"><span>停用账号</span></div></div></div></div>
                <div class="finder-item" id="channel"><div class="right-part"><div class="info"><div class="name"><span>视频号测试账号</span></div></div><span class="role">管理员</span></div></div>
              </div></div></div></div>`;
            document.getElementById('channel').onclick = () => {
              history.replaceState(null, '', '/platform');
              document.body.innerHTML = '<div class="account-info"><span class="name">视频号测试账号</span></div>';
            };"#).await?;
        let State::SelectChannel { rows } = observe(&page, &origin, &oauth).await? else {
            anyhow::bail!("channel picker was not detected");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].choice.role, "管理员");
        assert!(!select_channel(&page, &origin, &oauth, &rows, 0).await?);
        page.evaluate_expression("document.querySelector('#channel .role').textContent = '运营者'")
            .await?;
        assert!(!select_channel(&page, &origin, &oauth, &rows, 1).await?);
        let State::SelectChannel { rows } = observe(&page, &origin, &oauth).await? else {
            anyhow::bail!("updated channel picker was not detected");
        };
        assert!(select_channel(&page, &origin, &oauth, &rows, 1).await?);
        let State::Creator { identity } = observe(&page, &origin, &oauth).await? else {
            anyhow::bail!("creator identity was not detected");
        };
        assert_eq!(identity.nickname.as_deref(), Some("视频号测试账号"));
        assert!(matches!(
            observe(&page, &oauth, &origin).await?,
            State::Pending
        ));
        browser.close().await?;
        server.abort();
        frame_server.abort();
        Ok(())
    }
}
