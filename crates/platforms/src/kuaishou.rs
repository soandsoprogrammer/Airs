use crate::browser::BrowserSession;
use crate::douyin::read_verification_frame;
use crate::kuaishou_prepare::{dismiss_publish_guide, prepare_video, upload_timeout};
use crate::kuaishou_receipt::KuaishouReceiptObserver;
use crate::kuaishou_submit::{click_publish, preflight};
use crate::sign_in::{SignInGuard, SignInStatus, SignIns, VerificationAction};
use crate::upload_progress::UploadProgressRegistry;
use anyhow::Result;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::Page;
use publisher_core::{
    Account, Capabilities, Content, Job, JobState, MediaKind, PlatformAdapter, Prepared, Receipt,
    RunContext, SessionStatus, Store, UserError,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[path = "kuaishou_photo.rs"]
pub(crate) mod photo;
use photo::{PreparedAtlas, prepare_photos};

const CREATOR_ORIGIN: &str = "https://cp.kuaishou.com";
const CREATOR_HOME_URL: &str = "https://cp.kuaishou.com/profile";
const UPLOAD_URL: &str = "https://cp.kuaishou.com/article/publish/video?tabType=1";
const ATLAS_UPLOAD_URL: &str = "https://cp.kuaishou.com/article/publish/video?tabType=2";
pub const CREATOR_MANAGE_URL: &str = "https://cp.kuaishou.com/article/manage/video";
const PASSPORT_ORIGIN: &str = "https://passport.kuaishou.com";
const LOGIN_URL: &str = concat!(
    "https://passport.kuaishou.com/pc/account/login/?sid=kuaishou.web.cp.api",
    "&callback=https%3A%2F%2Fcp.kuaishou.com%2Frest%2Finfra%2Fsts",
    "%3FfollowUrl%3Dhttps%253A%252F%252Fcp.kuaishou.com%252Fprofile%26setRootDomain%3Dtrue",
);
const LOGIN_SCRIPT: &str = include_str!("kuaishou_login.js");
const QR_DATA_PREFIX: &str = "data:image/png;base64,";
const MAX_QR_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct Kuaishou {
    pub sign_ins: SignIns,
    pub upload_progress: UploadProgressRegistry,
}

impl Kuaishou {
    pub fn new(sign_ins: SignIns, upload_progress: UploadProgressRegistry) -> Self {
        Self {
            sign_ins,
            upload_progress,
        }
    }

    async fn creator_session(
        &self,
        profile: &Path,
        store: &Store,
        account: &Account,
        cancel: &CancellationToken,
        on_needs_login: impl FnOnce() -> Result<()> + Send,
    ) -> Result<Option<(BrowserSession, Page, String)>> {
        if cancel.is_cancelled() {
            return Err(UserError("已取消快手登录".into()).into());
        }
        let sign_in = self.sign_ins.begin(&account.id);
        let mut browser = BrowserSession::launch(profile).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(UserError("已取消快手登录".into()).into()),
            result = tokio::time::timeout(Duration::from_secs(5 * 60), async {
                let page = browser.browser.new_page(CREATOR_HOME_URL).await
                    .map_err(|_| UserError("无法打开快手创作者服务平台，请检查网络后重试".into()))?;
                let nickname = wait_for_login(&page, &sign_in, || {
                    store.sync_session(&account.id, SessionStatus::NeedsLogin, None)?;
                    on_needs_login()
                }).await?;
                store.sync_session(&account.id, SessionStatus::Ready, Some(nickname.clone()))
                    .map_err(|_| UserError("已识别快手账号，但保存登录状态失败".into()))?;
                Ok(Some((page, nickname)))
            }) => result.unwrap_or(Ok(None)),
        };
        drop(sign_in);
        match result {
            Ok(Some((page, nickname))) => Ok(Some((browser, page, nickname))),
            other => {
                browser.close().await?;
                other.map(|_| None)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum LoginPage {
    Creator { nickname: String },
    LoggedOut,
    RoleSelection,
    RoleActionApplied,
    QrSwitch,
    QrOpened,
    Qr { png: String },
    ConfirmOnPhone,
    Expired,
    Pending,
}

async fn login_page(
    page: &Page,
    creator_origin: &str,
    passport_origin: &str,
    open_qr: bool,
) -> Result<LoginPage> {
    evaluate_login_page(page, creator_origin, passport_origin, open_qr, Value::Null).await
}

async fn evaluate_login_page(
    page: &Page,
    creator_origin: &str,
    passport_origin: &str,
    open_qr: bool,
    role_click: Value,
) -> Result<LoginPage> {
    Ok(page
        .evaluate_expression(format!(
            "({LOGIN_SCRIPT})({creator_origin:?}, {passport_origin:?}, {open_qr}, {role_click})"
        ))
        .await?
        .into_value()?)
}

fn decode_qr(data_url: &str) -> Option<Vec<u8>> {
    let encoded = data_url.strip_prefix(QR_DATA_PREFIX)?;
    if encoded.len() > MAX_QR_BYTES.div_ceil(3) * 4 {
        return None;
    }
    let png = STANDARD.decode(encoded).ok()?;
    (png.len() <= MAX_QR_BYTES && png.starts_with(b"\x89PNG\r\n\x1a\n")).then_some(png)
}

async fn open_login_page(page: &Page) -> Result<()> {
    page.goto(LOGIN_URL)
        .await
        .map_err(|_| UserError("无法打开快手扫码登录，请检查网络后重试".into()))?;
    Ok(())
}

async fn wait_for_login(
    page: &Page,
    sign_in: &SignInGuard,
    on_needs_login: impl FnOnce() -> Result<()>,
) -> Result<String> {
    let mut notify_login = Some(on_needs_login);
    let mut phase = SignInStatus::Loading;
    let mut qr_tab_opened = false;
    let mut passport_opened = false;
    let mut role_viewport = None;
    let mut deadline = Instant::now() + Duration::from_secs(30);
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = sign_in.refresh_requested() => {
                if matches!(phase, SignInStatus::Loading | SignInStatus::WaitingForScan | SignInStatus::Expired) {
                    phase = SignInStatus::Loading;
                    sign_in.update(phase, None);
                    if passport_opened {
                        open_login_page(page).await?;
                    } else {
                        page.goto(CREATOR_HOME_URL).await
                            .map_err(|_| UserError("无法刷新快手创作者服务平台，请检查网络后重试".into()))?;
                    }
                    qr_tab_opened = false;
                    deadline = Instant::now() + Duration::from_secs(30);
                }
            }
            Some(action) = sign_in.verification_action() => {
                if let (
                    SignInStatus::VerifyInApp,
                    Some((width, height)),
                    VerificationAction::Click { x, y },
                ) = (phase, role_viewport, action)
                    && (0.0..1.0).contains(&x)
                    && (0.0..1.0).contains(&y)
                {
                    let click = json!({
                        "x": x * f64::from(width),
                        "y": y * f64::from(height),
                        "width": width,
                        "height": height,
                    });
                    let _ = evaluate_login_page(page, CREATOR_ORIGIN, PASSPORT_ORIGIN, false, click).await;
                    ticker.reset_immediately();
                }
            }
            _ = ticker.tick() => {
                match login_page(page, CREATOR_ORIGIN, PASSPORT_ORIGIN, !qr_tab_opened).await {
                    Ok(LoginPage::Creator { nickname }) => {
                        return Ok(nickname);
                    }
                    Ok(LoginPage::LoggedOut) if !passport_opened => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        open_login_page(page).await?;
                        passport_opened = true;
                        deadline = Instant::now() + Duration::from_secs(30);
                    }
                    Ok(LoginPage::RoleSelection) => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        phase = SignInStatus::VerifyInApp;
                        sign_in.update(phase, None);
                        if let Ok((png, width, height)) = read_verification_frame(page).await
                            && matches!(login_page(page, CREATOR_ORIGIN, PASSPORT_ORIGIN, false).await, Ok(LoginPage::RoleSelection))
                        {
                            sign_in.update_verification(png, width, height);
                            sign_in.set_input_feedback(Some("请在快手页面选择创作者角色，再点击「确定」。"));
                            role_viewport = Some((width, height));
                        }
                        deadline = Instant::now() + Duration::from_secs(30);
                    }
                    Ok(LoginPage::QrOpened) => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        passport_opened = true;
                        qr_tab_opened = true;
                    }
                    Ok(LoginPage::Qr { png }) => {
                        if let Some(notify) = notify_login.take() {
                            notify()?;
                        }
                        passport_opened = true;
                        if let Some(png) = decode_qr(&png) {
                            phase = SignInStatus::WaitingForScan;
                            sign_in.update(phase, Some(png));
                            deadline = Instant::now() + Duration::from_secs(30);
                        }
                    }
                    Ok(LoginPage::ConfirmOnPhone) => {
                        phase = SignInStatus::ConfirmOnPhone;
                        sign_in.update(phase, None);
                        deadline = Instant::now() + Duration::from_secs(30);
                    }
                    Ok(LoginPage::Expired) => {
                        phase = SignInStatus::Expired;
                        sign_in.update(phase, None);
                        deadline = Instant::now() + Duration::from_secs(30);
                    }
                    Ok(LoginPage::Pending) if matches!(phase, SignInStatus::WaitingForScan | SignInStatus::ConfirmOnPhone | SignInStatus::VerifyInApp) =>
                    {
                        phase = SignInStatus::CheckingAccount;
                        sign_in.update(phase, None);
                        role_viewport = None;
                    }
                    _ => {}
                }
                if Instant::now() >= deadline {
                    let message = if matches!(phase, SignInStatus::CheckingAccount) {
                        "快手扫码页面已关闭，但暂未确认创作者账号登录，请检查手机结果后重试"
                    } else {
                        "暂未找到可用的快手扫码登录页面，请检查网络后重试"
                    };
                    return Err(UserError(message.into()).into());
                }
            }
        }
    }
}

#[async_trait]
impl PlatformAdapter for Kuaishou {
    fn validate_content(&self, content: &Content) -> Result<()> {
        crate::kuaishou_prepare::caption(content).map(|_| ())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "快手",
            verified: false,
            separate_prepare: true,
            custom_cover: false,
            submit_enabled: true,
            image_posts: true,
        }
    }

    fn prepare_timeout(&self, job: &Job) -> Duration {
        upload_timeout(job.content.asset.size) + Duration::from_secs(7 * 60)
    }

    async fn login(
        &self,
        store: &Store,
        account: &Account,
        cancel: CancellationToken,
    ) -> Result<SessionStatus> {
        let lease = store.lease(&account.id)?;
        let Some((mut browser, _, _)) = self
            .creator_session(&lease.path, store, account, &cancel, || Ok(()))
            .await?
        else {
            return Ok(SessionStatus::NeedsLogin);
        };
        browser.close().await?;
        Ok(SessionStatus::Ready)
    }

    async fn prepare(&self, ctx: RunContext) -> Result<Box<dyn Prepared>> {
        let is_atlas = ctx.job.content.asset.kind == MediaKind::Images;
        let profile = ctx.store.root.join("profiles").join(&ctx.job.account.id);
        let Some((mut browser, page, nickname)) = self
            .creator_session(&profile, &ctx.store, &ctx.job.account, &ctx.cancel, || {
                ctx.stage(
                    JobState::WaitingUser,
                    "请在任务卡片中使用快手 App 扫码，登录后会自动继续准备",
                )
            })
            .await?
        else {
            return Err(UserError("等待快手登录超时，请先在账号管理中完成登录".into()).into());
        };
        let result = async {
            page.goto(if is_atlas {
                ATLAS_UPLOAD_URL
            } else {
                UPLOAD_URL
            })
            .await
            .map_err(|_| {
                UserError("快手已登录，但无法打开作品上传页，请检查网络后重新准备".into())
            })?;
            if is_atlas {
                let (caption, atlas) = prepare_photos(&page, &ctx, &self.upload_progress).await?;
                Ok((caption, Some(atlas)))
            } else {
                let caption = prepare_video(&page, &ctx, &self.upload_progress).await?;
                Ok((caption, None))
            }
        }
        .await;
        let (caption, atlas) = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                browser.close().await?;
                return Err(error);
            }
        };
        Ok(Box::new(KuaishouPrepared {
            browser,
            page,
            nickname,
            caption,
            atlas,
            publish_attempted: false,
            receipt_observer: None,
        }))
    }
}

struct KuaishouPrepared {
    browser: BrowserSession,
    page: Page,
    nickname: String,
    caption: String,
    atlas: Option<PreparedAtlas>,
    publish_attempted: bool,
    receipt_observer: Option<KuaishouReceiptObserver>,
}

#[async_trait]
impl Prepared for KuaishouPrepared {
    async fn preflight_submission(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.publish_attempted,
            "该任务已尝试提交，请在快手创作者服务平台核对结果。"
        );
        dismiss_publish_guide(&self.page, CREATOR_ORIGIN).await?;
        let images = self.atlas.as_ref().map(|atlas| atlas.images.as_slice());
        preflight(&self.page, &self.nickname, &self.caption, images).await?;
        self.receipt_observer = Some(
            KuaishouReceiptObserver::start(
                &self.page,
                &self.caption,
                self.atlas.as_ref().map(|atlas| &atlas.identity),
            )
            .await?,
        );
        Ok(())
    }

    async fn submit(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.publish_attempted && self.receipt_observer.is_some(),
            "发布检查未完成，或该任务已尝试提交。"
        );
        self.publish_attempted = true;
        let images = self.atlas.as_ref().map(|atlas| atlas.images.as_slice());
        click_publish(&self.page, &self.nickname, &self.caption, images).await
    }

    async fn inspect_result(&mut self) -> Result<Option<Receipt>> {
        if !self.publish_attempted {
            return Ok(None);
        }
        let Some(observer) = self.receipt_observer.take() else {
            return Ok(None);
        };
        observer.wait_for(&self.page, Duration::from_secs(90)).await
    }

    async fn close(&mut self) -> Result<()> {
        self.browser.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn login_probes_only_trusted_creator_and_passport_controls() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0; 4096];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 15\r\nConnection: close\r\n\r\n<!doctype html>").await;
                }
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/profile"))
            .await?;
        page.set_content(
            r#"<a class="login" href="/rest/infra/logout">立即登录</a><span>上传视频</span>"#,
        )
        .await?;
        assert!(matches!(
            login_page(&page, &origin, PASSPORT_ORIGIN, true).await?,
            LoginPage::LoggedOut
        ));
        page.set_content(
            r#"
            <header class="header-bar"><div class="user-info-dpd">
              <div class="user-info-avatar">头像</div><div class="user-info-name">快手测试昵称</div>
            </div></header>
        "#,
        )
        .await?;
        assert!(
            matches!(login_page(&page, &origin, PASSPORT_ORIGIN, false).await?,
            LoginPage::Creator { nickname } if nickname == "快手测试昵称")
        );
        assert!(matches!(
            login_page(&page, CREATOR_ORIGIN, PASSPORT_ORIGIN, false).await?,
            LoginPage::Pending
        ));
        page.set_content(r#"
            <button id="unrelated" style="position:fixed;left:32px;top:32px;width:80px;height:24px">发布</button>
            <div class="role-selector"><div class="el-dialog" role="dialog"
                style="position:fixed;left:10px;top:10px;width:300px;height:220px;background:white">
              <p class="tip">请选择您要登录的角色</p>
              <div class="role"><button class="role-item" style="position:fixed;left:32px;top:80px;width:120px;height:32px">
                <span class="radio__content__name">创作者</span>
              </button></div>
              <button class="confirm__btn" disabled style="position:fixed;left:32px;top:140px;width:120px;height:32px">确定</button>
            </div></div>
            <script>
              window.unrelatedClicks = 0;
              document.querySelector('#unrelated').onclick = () => window.unrelatedClicks++;
              document.querySelector('.role-item').onclick = () => document.querySelector('.confirm__btn').disabled = false;
              document.querySelector('.confirm__btn').onclick = () => document.querySelector('.role-selector').remove();
            </script>
        "#).await?;
        let (width, height): (u32, u32) = page
            .evaluate_expression("[innerWidth, innerHeight]")
            .await?
            .into_value()?;
        let click = |y| json!({"x": 45, "y": y, "width": width, "height": height});
        assert!(matches!(
            evaluate_login_page(&page, &origin, PASSPORT_ORIGIN, false, click(150)).await?,
            LoginPage::RoleSelection
        ));
        for y in [90, 150] {
            assert!(matches!(
                evaluate_login_page(&page, &origin, PASSPORT_ORIGIN, false, click(y)).await?,
                LoginPage::RoleActionApplied
            ));
        }
        assert!(matches!(
            evaluate_login_page(&page, &origin, PASSPORT_ORIGIN, false, click(40)).await?,
            LoginPage::Pending
        ));
        assert!(crate::browser::truth(&page, "window.unrelatedClicks === 0").await?);

        page.goto(format!(
            "{origin}/pc/account/login/?sid=kuaishou.web.cp.api"
        ))
        .await?;
        page.set_content(
            r#"
            <main id="login-form">
              <div class="platform-switch">扫码登录</div>
              <div class="qr-login" hidden style="position:absolute;left:1600px;top:100px">
                <h2>快手APP，扫码登录</h2><img alt="qrcode">
              </div>
            </main>
            <script>
            const form = document.querySelector('main');
            const switcher = form.querySelector('.platform-switch');
            const panel = form.querySelector('.qr-login');
            const canvas = document.createElement('canvas');
            canvas.width = canvas.height = 125;
            const ctx = canvas.getContext('2d');
            ctx.fillStyle = 'white';
            ctx.fillRect(0, 0, 125, 125);
            const blank = canvas.toDataURL('image/png');
            ctx.fillStyle = 'black';
            for (let y = 5; y < 120; y += 5) {
              for (let x = 5; x < 120; x += 5) {
                if ((x + y) % 10 === 0) ctx.fillRect(x, y, 5, 5);
              }
            }
            window.fixtureQr = canvas.toDataURL('image/png');
            switcher.onclick = () => {
              switcher.hidden = true;
              panel.hidden = false;
              panel.querySelector('img').src = blank;
            };
            </script>
        "#,
        )
        .await?;
        assert!(matches!(
            login_page(&page, CREATOR_ORIGIN, PASSPORT_ORIGIN, true).await?,
            LoginPage::Pending
        ));
        assert!(matches!(
            login_page(&page, CREATOR_ORIGIN, &origin, false).await?,
            LoginPage::QrSwitch
        ));
        assert!(matches!(
            login_page(&page, CREATOR_ORIGIN, &origin, true).await?,
            LoginPage::QrOpened
        ));
        page.evaluate_expression(
            chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
                .expression("document.querySelector('img').decode()")
                .await_promise(true)
                .build()
                .unwrap(),
        )
        .await?;
        assert!(matches!(
            login_page(&page, CREATOR_ORIGIN, &origin, false).await?,
            LoginPage::Pending
        ));
        let expected: String = page
            .evaluate_expression(
                chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
                    .expression("(async () => { const image = document.querySelector('img'); image.src = window.fixtureQr; await image.decode(); return image.currentSrc; })()")
                    .await_promise(true)
                    .build()
                    .unwrap(),
            )
            .await?
            .into_value()?;
        assert!(crate::browser::truth(&page, "document.querySelector('img').getBoundingClientRect().left > innerWidth && scrollX === 0").await?);
        let LoginPage::Qr { png } = login_page(&page, CREATOR_ORIGIN, &origin, false).await? else {
            panic!("有效二维码在视口外时仍应读取原始图片");
        };
        assert!(decode_qr(&png).is_some());
        assert_eq!(png, expected);
        assert!(crate::browser::truth(&page, "scrollX === 0").await?);
        page.evaluate_expression("document.querySelector('h2').textContent = '二维码已过期'")
            .await?;
        assert!(matches!(
            login_page(&page, CREATOR_ORIGIN, &origin, false).await?,
            LoginPage::Expired
        ));
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
