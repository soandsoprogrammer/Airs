use crate::browser::*;
pub use crate::douyin_publish_verification::relay_publish_verification;
use crate::douyin_receipt::DouyinReceiptObserver;
use crate::douyin_submit::{click_publish, preflight, prepared_description};
use crate::sign_in::{SignInGuard, SignInStatus, SignIns, VerificationAction};
use crate::upload_progress::{UploadProgress, UploadProgressGuard, UploadProgressRegistry};
use anyhow::{Result, bail};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::{
            dom::{DescribeNodeParams, SetFileInputFilesParams},
            input::InsertTextParams,
            page::CaptureScreenshotFormat,
        },
        js_protocol::runtime::EvaluateParams,
    },
    layout::Point,
    page::ScreenshotParams,
};
use publisher_core::*;
use std::{path::Path, sync::LazyLock, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub const UPLOAD_URL: &str = "https://creator.douyin.com/creator-micro/content/upload";
const IMAGE_UPLOAD_URL: &str =
    "https://creator.douyin.com/creator-micro/content/upload?default-tab=3";
pub const CREATOR_MANAGE_URL: &str = "https://creator.douyin.com/creator-micro/content/manage";
const CREATOR_ORIGIN: &str = "https://creator.douyin.com";
const CREATOR_HOME_URL: &str = "https://creator.douyin.com/creator-micro/home";
const FILE_SELECTOR: &str = "input.upload-btn-input, input[type='file'][accept*='video'], div[class^='container'] input[type='file']:not([accept*='image'])";
pub(crate) const PHOTO_FILE_SELECTOR: &str =
    "input[type='file'][multiple][accept*='image/']:not([accept*='video'])";
pub(crate) const PHOTO_PAGE_SCRIPT: &str = include_str!("douyin_photo_page.js");
static CREATOR_PAGE_SCRIPT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "({})({CREATOR_ORIGIN:?}, {FILE_SELECTOR:?}, {PHOTO_FILE_SELECTOR:?})",
        include_str!("douyin_creator_page.js")
    )
});
static UPLOAD_PAGE_SCRIPT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "({})({CREATOR_ORIGIN:?})",
        include_str!("douyin_upload_page.js")
    )
});

fn upload_timeout(bytes: u64) -> Duration {
    const MIB: u64 = 1024 * 1024;
    // Budget one second per MiB, plus time for the platform to process the video.
    Duration::from_secs((600 + bytes.div_ceil(MIB)).min(2 * 60 * 60))
}

#[derive(Default)]
pub struct Douyin {
    pub sign_ins: SignIns,
    pub upload_progress: UploadProgressRegistry,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatorPage {
    authenticated: bool,
    needs_interaction: bool,
    nickname: Option<String>,
    upload_ready: bool,
    photo_upload_ready: bool,
    can_verify_identity: bool,
    qr_status: Option<QrStatus>,
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum QrStatus {
    ConfirmOnPhone,
    VerifyInApp,
    Expired,
    Failed,
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum UploadStatus {
    Waiting,
    Uploading,
    Processing,
    EditorMissing,
    Ready,
    Failed,
    NeedsLogin,
}

#[derive(serde::Deserialize)]
struct UploadObservation {
    status: UploadStatus,
    percent: Option<serde_json::Value>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) struct PhotoSnapshot {
    sources: Vec<String>,
    cover: String,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum PhotoStatus {
    WrongPage,
    Waiting,
    SelectFiles,
    Uploading,
    Processing,
    Failed,
    CountMismatch,
    ContentChanged,
    EditorMissing,
    NeedsLogin,
    Ready,
}

#[derive(serde::Deserialize)]
struct PhotoObservation {
    status: PhotoStatus,
    photos: Option<PhotoSnapshot>,
}

pub(crate) fn photo_expression(
    origin: &str,
    count: usize,
    action: &str,
    title: &str,
    description: &str,
) -> String {
    let args = serde_json::json!([
        origin,
        PHOTO_FILE_SELECTOR,
        count,
        action,
        title,
        description
    ]);
    format!("({PHOTO_PAGE_SCRIPT})(...{args})")
}

async fn observe_photos(
    page: &Page,
    origin: &str,
    count: usize,
    action: &str,
    title: &str,
    description: &str,
) -> Result<PhotoObservation> {
    Ok(page
        .evaluate_expression(photo_expression(origin, count, action, title, description))
        .await?
        .into_value()?)
}

pub(crate) async fn select_photos(page: &Page, origin: &str, images: &[ImageFile]) -> Result<()> {
    let paths = images
        .iter()
        .map(|image| {
            image
                .path
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| UserError("无法读取图片路径。".into()))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // Resolve the input from the guarded page, then send one ordered selection.
    let mut params = EvaluateParams::new(photo_expression(origin, images.len(), "input", "", ""));
    params.return_by_value = Some(false);
    let result = page.evaluate_expression(params).await?;
    let input = result
        .object()
        .object_id
        .clone()
        .ok_or_else(|| UserError("未找到抖音图文的多图上传入口。".into()))?;
    let node = page
        .execute(DescribeNodeParams::builder().object_id(input).build())
        .await?
        .result
        .node;
    let mut params = SetFileInputFilesParams::new(paths);
    params.backend_node_id = Some(node.backend_node_id);
    page.execute(params).await?;
    Ok(())
}

async fn wait_for_uploaded_photos(
    page: &Page,
    origin: &str,
    count: usize,
    limit: Duration,
    progress: &UploadProgressGuard,
) -> Result<()> {
    let wait = async {
        let mut read_failures = 0;
        loop {
            match observe_photos(page, origin, count, "observe", "", "").await {
                Ok(observation) => {
                    read_failures = 0;
                    match observation.status {
                        PhotoStatus::Ready => return Ok(()),
                        PhotoStatus::Failed => {
                            return Err(UserError(
                                "抖音提示图片上传失败，请检查图片格式、大小和网络。".into(),
                            ));
                        }
                        PhotoStatus::CountMismatch => {
                            return Err(UserError(
                                "抖音显示的图片数量与任务不一致，请重新准备。".into(),
                            ));
                        }
                        PhotoStatus::NeedsLogin => {
                            return Err(UserError("抖音在上传图片时要求重新登录。".into()));
                        }
                        PhotoStatus::Processing => progress.set(Some(UploadProgress::Processing)),
                        PhotoStatus::EditorMissing => {
                            progress.set(Some(UploadProgress::EditorMissing))
                        }
                        _ => progress.set(Some(UploadProgress::Uploading { percent: None })),
                    }
                }
                Err(_) => {
                    read_failures += 1;
                    if read_failures >= 4 {
                        return Err(UserError(
                            "图片上传时与抖音页面的连接中断，请重新准备。".into(),
                        ));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::time::timeout(limit, wait).await.map_err(|_| {
        UserError("等待抖音图文上传和编辑页就绪超时，请到创作者中心检查。".into())
    })??;
    Ok(())
}

impl UploadObservation {
    fn progress(&self) -> Option<UploadProgress> {
        match self.status {
            UploadStatus::Uploading => Some(UploadProgress::Uploading {
                percent: self
                    .percent
                    .as_ref()
                    .and_then(serde_json::Value::as_u64)
                    .filter(|&percent| percent <= 100)
                    .map(|percent| percent as u8),
            }),
            UploadStatus::Processing => Some(UploadProgress::Processing),
            UploadStatus::EditorMissing => Some(UploadProgress::EditorMissing),
            UploadStatus::Ready => Some(UploadProgress::FillingForm),
            UploadStatus::Waiting | UploadStatus::Failed | UploadStatus::NeedsLogin => None,
        }
    }
}

impl UploadStatus {
    fn timeout_error(self) -> UserError {
        let message = match self {
            Self::Uploading => "等待上传超时；抖音最后一次显示视频仍在上传。",
            Self::Processing => "等待处理超时；抖音最后一次显示视频仍在处理。",
            Self::EditorMissing => "抖音显示视频已上传，但未找到作品编辑框。",
            _ => "等待超时；未能确认抖音是否已收到视频，请到创作者中心核对。",
        };
        UserError(format!("{message} 未执行最终提交。"))
    }
}

async fn wait_for_uploaded_video(
    page: &Page,
    limit: Duration,
    progress: &UploadProgressGuard,
) -> Result<()> {
    let mut last_observed = UploadStatus::Waiting;
    let result = tokio::time::timeout(limit, async {
        let mut failures = 0;
        let mut editor_wait_started = None;
        let mut progress_missing_once = false;
        loop {
            let observation = page
                .evaluate_expression(UPLOAD_PAGE_SCRIPT.as_str())
                .await
                .and_then(|result| result.into_value::<UploadObservation>().map_err(Into::into));
            match observation.as_ref().ok().and_then(UploadObservation::progress) {
                Some(value) => {
                    progress_missing_once = false;
                    progress.set(Some(value));
                }
                None if progress_missing_once => progress.set(None),
                // A single redraw or navigation can hide the upload widget briefly.
                None => progress_missing_once = true,
            }
            match observation {
                Ok(observation) => {
                    failures = 0;
                    let status = observation.status;
                    if !matches!(status, UploadStatus::Waiting) {
                        last_observed = status;
                    }
                    if !matches!(status, UploadStatus::EditorMissing) {
                        editor_wait_started = None;
                    }
                    match status {
                        UploadStatus::Ready => return Ok(()),
                        UploadStatus::Failed => {
                            return Err(UserError(
                                "抖音页面提示上传失败，请检查视频格式、大小或网络。未执行最终提交。".into(),
                            ).into());
                        }
                        UploadStatus::NeedsLogin => {
                            return Err(UserError(
                                "上传时抖音要求重新登录，请检查账号会话。未执行最终提交。".into(),
                            ).into());
                        }
                        UploadStatus::EditorMissing => {
                            let since = editor_wait_started.get_or_insert_with(Instant::now);
                            if since.elapsed() >= Duration::from_secs(60) {
                                return Err(status.timeout_error().into());
                            }
                        }
                        UploadStatus::Waiting | UploadStatus::Uploading | UploadStatus::Processing => {}
                    }
                }
                Err(_) => {
                    failures += 1;
                    if failures >= 4 {
                        return Err(UserError(
                            "上传时无法读取抖音页面，未能确认视频是否上传。未执行最终提交。".into(),
                        ).into());
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await;
    result.unwrap_or_else(|_| Err(last_observed.timeout_error().into()))
}

async fn creator_page(page: &Page) -> Result<CreatorPage> {
    Ok(page
        .evaluate_expression(CREATOR_PAGE_SCRIPT.as_str())
        .await?
        .into_value()?)
}

pub(crate) async fn wait_for_photo_upload_tab(
    page: &Page,
    origin: &str,
    cancel: &CancellationToken,
    limit: Duration,
) -> Result<bool> {
    let point_probe = photo_expression(origin, 0, "tabPoint", "", "");
    let wait = async {
        let mut switched = false;
        loop {
            if let Ok(observation) = observe_photos(page, origin, 0, "observe", "", "").await
                && matches!(observation.status, PhotoStatus::SelectFiles)
            {
                return Ok(true);
            }
            // The site's tab click can select the image pane without changing the URL.
            if !switched
                && let Ok(point) = page.evaluate_expression(point_probe.as_str()).await
                && let Ok(Some([x, y])) = point.into_value::<Option<[f64; 2]>>()
            {
                page.click(Point::new(x, y)).await?;
                switched = true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(UserError("已取消".into()).into()),
        result = tokio::time::timeout(limit, wait) => result.unwrap_or(Ok(false)),
    }
}

async fn resolve_creator_page(
    page: &Page,
    next_identity_check: &mut Instant,
) -> Result<CreatorPage> {
    let mut state = creator_page(page).await?;
    if state.can_verify_identity && !state.authenticated && Instant::now() >= *next_identity_check {
        let profile = read_creator_profile(page).await;
        *next_identity_check = Instant::now() + Duration::from_secs(4);
        if let Ok(profile) = profile
            && profile.verified
        {
            state.authenticated = true;
            state.nickname = profile.nickname;
        }
    }
    Ok(state)
}

async fn wait_for_creator_page(
    page: &Page,
    cancel: &CancellationToken,
    limit: Duration,
    ready: impl Fn(&CreatorPage) -> bool,
) -> Result<Option<CreatorPage>> {
    let poll = async {
        let mut failures = 0;
        let mut next_identity_check = Instant::now();
        loop {
            match resolve_creator_page(page, &mut next_identity_check).await {
                Ok(state) if ready(&state) => return Ok(Some(state)),
                Ok(_) => failures = 0,
                Err(_) => {
                    // A redirect can replace the JavaScript context between polls.
                    failures += 1;
                    if failures >= 4 {
                        return Err(UserError("浏览器连接中断，请重新检查登录".into()).into());
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(UserError("已取消".into()).into()),
        result = tokio::time::timeout(limit, poll) => result.unwrap_or(Ok(None)),
    }
}

async fn session_nickname(page: &Page, state: CreatorPage) -> Option<String> {
    if state.nickname.is_some() {
        state.nickname
    } else {
        read_nickname(page).await.ok().flatten()
    }
}

async fn open_creator_page(
    profile: &Path,
    url: &str,
    cancel: &CancellationToken,
) -> Result<(BrowserSession, Page)> {
    let mut browser = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("已取消"),
        browser = BrowserSession::launch(profile) => browser
            .map_err(|_| UserError("浏览器无法启动，请检查设置中的 Chrome 程序路径".into()))?,
    };
    let page = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(UserError("已取消".into())),
        page = browser.browser.new_page(url) => page
            .map_err(|_| UserError("无法打开抖音创作者页面，请检查网络".into())),
    };
    match page {
        Ok(page) => Ok((browser, page)),
        Err(error) => {
            browser.close().await?;
            Err(error.into())
        }
    }
}

async fn check_background_session(
    profile: &Path,
    url: &str,
    cancel: &CancellationToken,
) -> Result<(BrowserSession, Page, CreatorPage)> {
    let (mut browser, page) = open_creator_page(profile, url, cancel).await?;
    let state = wait_for_creator_page(&page, cancel, Duration::from_secs(20), |state| {
        state.authenticated || state.needs_interaction
    })
    .await;
    match state {
        Ok(Some(state)) => Ok((browser, page, state)),
        result => {
            browser.close().await?;
            Err(result
                .err()
                .unwrap_or_else(|| UserError("无法确认抖音会话，请检查网络后重试".into()).into()))
        }
    }
}

async fn creator_session(
    profile: &Path,
    url: &str,
    cancel: &CancellationToken,
    sign_ins: &SignIns,
    account_id: &str,
    on_interaction: impl FnOnce() -> Result<()>,
) -> Result<Option<(BrowserSession, Page, CreatorPage)>> {
    let (mut background, page, state) = check_background_session(profile, url, cancel).await?;
    if state.authenticated {
        return Ok(Some((background, page, state)));
    }

    let sign_in = sign_ins.begin(account_id);
    let result = async {
        on_interaction()?;
        wait_for_qr_sign_in(&page, &sign_in, cancel).await
    }
    .await;
    drop(sign_in);
    match result {
        Ok(Some(state)) => Ok(Some((background, page, state))),
        result => {
            background.close().await?;
            result.map(|_| None)
        }
    }
}

/// Returns only the QR image from the official sign-in panel.
pub async fn read_login_qr(page: &Page) -> Result<Option<Vec<u8>>> {
    let expression = r#"(() => {
      if (location.origin !== 'https://creator.douyin.com') return {};
      const prefix = 'data:image/png;base64,';
      for (const img of document.querySelectorAll('img[src^="data:image/png;base64,"]')) {
        if (!img.getClientRects().length || getComputedStyle(img).visibility !== 'visible' ||
            img.naturalWidth < 128 || img.naturalWidth !== img.naturalHeight) continue;
        const label = img.getAttribute('alt') ?? '';
        let panel = img.parentElement;
        for (let depth = 0; panel && depth < 4; depth++, panel = panel.parentElement) {
          if (img.closest('#animate_qrcode_container, [class*="scan_qrcode_login_content"]') ||
              /二维码/.test(label + (img.getAttribute('aria-label') ?? '')) ||
              /如何扫码|扫码登录/.test(panel.innerText ?? '')) {
            return { png: img.src.slice(prefix.length) };
          }
        }
      }
      return {};
    })()"#;
    let result: serde_json::Value = page.evaluate_expression(expression).await?.into_value()?;
    let Some(encoded) = result.get("png").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    anyhow::ensure!(encoded.len() <= 1024 * 1024, "抖音二维码图片超过大小限制");
    let png = STANDARD.decode(encoded)?;
    anyhow::ensure!(
        png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "抖音二维码图片格式无效"
    );
    Ok(Some(png))
}

async fn wait_for_qr_sign_in(
    page: &Page,
    sign_in: &SignInGuard,
    cancel: &CancellationToken,
) -> Result<Option<CreatorPage>> {
    let poll = async {
        let mut qr_deadline = Instant::now() + Duration::from_secs(25);
        let mut next_identity_check = Instant::now();
        let mut phase = SignInStatus::Loading;
        let mut qr_seen = false;
        let mut failures = 0;
        let mut verification_viewport = None;
        let mut verification_missing_once = false;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = sign_in.refresh_requested() => {
                    if matches!(phase, SignInStatus::ConfirmOnPhone | SignInStatus::CheckingAccount) {
                        continue;
                    }
                    phase = SignInStatus::Loading;
                    verification_viewport = None;
                    verification_missing_once = false;
                    sign_in.update(phase, None);
                    qr_seen = false;
                    page.reload().await
                        .map_err(|_| UserError("二维码刷新失败，请重新检查登录".into()))?;
                    qr_deadline = Instant::now() + Duration::from_secs(25);
                }
                Some(action) = sign_in.verification_action() => {
                    let feedback = if phase == SignInStatus::VerifyInApp
                        && let Some(viewport) = verification_viewport
                        && let Ok(state) = creator_page(page).await
                        && matches!(state.qr_status, Some(QrStatus::VerifyInApp))
                    {
                        // The action comes from a person using the verification preview.
                        // Treat a navigation during dispatch as a normal page transition.
                        forward_verification_action(page, CREATOR_ORIGIN, viewport, action).await
                    } else if matches!(action, VerificationAction::FillText(_)) {
                        Some("验证页面已变化，请重新选择输入框后再试。")
                    } else {
                        None
                    };
                    sign_in.set_input_feedback(feedback);
                }
                _ = ticker.tick() => {
                    let state = match resolve_creator_page(page, &mut next_identity_check).await {
                        Ok(state) => { failures = 0; state }
                        Err(_) => {
                            failures += 1;
                            if failures >= 4 {
                                return Err(UserError("扫码过程中与抖音页面的连接中断，请重新检查登录".into()).into());
                            }
                            continue;
                        }
                    };
                    if state.authenticated {
                        return Ok(Some(state));
                    }
                    if let Some(status) = state.qr_status {
                        verification_missing_once = false;
                        phase = match status {
                            QrStatus::ConfirmOnPhone => SignInStatus::ConfirmOnPhone,
                            QrStatus::VerifyInApp => SignInStatus::VerifyInApp,
                            QrStatus::Expired => SignInStatus::Expired,
                            QrStatus::Failed => return Err(UserError("抖音页面提示登录失败，请重新检查登录".into()).into()),
                        };
                        sign_in.update(phase, None);
                        if phase == SignInStatus::VerifyInApp {
                            if let Ok(viewport) = capture_verification(page, sign_in).await {
                                verification_viewport = Some(viewport);
                            }
                        } else {
                            verification_viewport = None;
                        }
                        qr_deadline = Instant::now() + Duration::from_secs(45);
                        continue;
                    }
                    if phase == SignInStatus::VerifyInApp && !verification_missing_once {
                        // A verification document can briefly redraw between polls.
                        verification_missing_once = true;
                        continue;
                    }
                    verification_missing_once = false;
                    if let Ok(Some(png)) = read_login_qr(page).await {
                        qr_seen = true;
                        phase = SignInStatus::WaitingForScan;
                        sign_in.update(phase, Some(png));
                        qr_deadline = Instant::now() + Duration::from_secs(45);
                    } else {
                        if qr_seen || phase == SignInStatus::VerifyInApp {
                            phase = SignInStatus::CheckingAccount;
                            sign_in.update(phase, None);
                        }
                        if Instant::now() >= qr_deadline {
                            let message = if qr_seen {
                                "二维码已关闭，但暂未确认账号登录。请查看手机端的登录结果后重试"
                            } else {
                                "抖音未返回可用的二维码，请重新检查登录"
                            };
                            return Err(UserError(message.into()).into());
                        }
                    }
                }
            }
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(UserError("已取消登录".into()).into()),
        result = tokio::time::timeout(Duration::from_secs(300), poll) => result.unwrap_or(Ok(None)),
    }
}

async fn capture_verification(page: &Page, sign_in: &SignInGuard) -> Result<(u32, u32)> {
    let (png, width, height) = read_verification_frame(page).await?;
    sign_in.update_verification(png, width, height);
    Ok((width, height))
}

pub(crate) async fn read_verification_frame(page: &Page) -> Result<(Vec<u8>, u32, u32)> {
    let (width, height): (u32, u32) = page
        .evaluate_expression("[window.innerWidth, window.innerHeight]")
        .await?
        .into_value()?;
    anyhow::ensure!(width > 0 && height > 0, "验证页面尚未显示");
    let png = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .capture_beyond_viewport(false)
                .build(),
        )
        .await?;
    Ok((png, width, height))
}

async fn forward_verification_action(
    page: &Page,
    expected_origin: &str,
    (width, height): (u32, u32),
    action: VerificationAction,
) -> Option<&'static str> {
    match action {
        VerificationAction::Click { x, y }
            if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) =>
        {
            let _ = page
                .click(Point {
                    x: x * f64::from(width),
                    y: y * f64::from(height),
                })
                .await;
            None
        }
        VerificationAction::FillText(text) => Some(
            match fill_verification_field(page, expected_origin, text).await {
                Ok(()) => "已发送填写请求，请在验证页面核对后提交。",
                Err(feedback) => feedback,
            },
        ),
        _ => None,
    }
}

async fn fill_verification_field(
    page: &Page,
    expected_origin: &str,
    text: String,
) -> Result<(), &'static str> {
    if text.trim().is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
        return Err("请检查要填写的内容。");
    }
    let select_field = r#"(expectedOrigin) => {
      if (location.origin !== expectedOrigin) return false;
      let field = document.activeElement;
      while (field?.localName === 'iframe') {
        field = field.contentDocument?.activeElement;
      }
      if (!field || field.matches(':disabled, [readonly]') ||
          !field.getClientRects().length ||
          field.ownerDocument.defaultView.getComputedStyle(field).visibility !== 'visible') {
        return false;
      }
      const isEditable = field.localName === 'textarea' ||
        (field.localName === 'input' && ['text', 'search', 'tel', 'number', 'password'].includes(field.type));
      if (!isEditable) return false;
      field.select();
      return true;
    }"#;
    let expected_origin = serde_json::to_string(expected_origin)
        .map_err(|_| "填写失败，请重新点击网页中的输入框。")?;
    if !truth(page, &format!("({select_field})({expected_origin})"))
        .await
        .map_err(|_| "填写失败，请重新点击网页中的输入框。")?
    {
        return Err("请先点击下方验证页面中的输入框，再点「填入网页」。");
    }
    page.execute(InsertTextParams::new(text))
        .await
        .map_err(|_| "填写失败，请重新点击网页中的输入框。")?;
    Ok(())
}

pub async fn read_nickname(page: &Page) -> Result<Option<String>> {
    Ok(read_creator_profile(page).await?.nickname)
}

#[derive(serde::Deserialize)]
struct CreatorProfile {
    verified: bool,
    nickname: Option<String>,
}

/// Return only a verification flag and display nickname from the creator profile.
async fn read_creator_profile(page: &Page) -> Result<CreatorProfile> {
    let script = r#"(async()=>{
      let fallback = null;
      if(location.origin==='https://creator.douyin.com') {
        for(const path of ['/aweme/v1/creator/pc/user/info/','/aweme/v1/creator/user/info/']) {
          try {
            const r=await fetch(path,{credentials:'same-origin',signal:AbortSignal.timeout(3000)});
            if(!r.ok) continue; const data=await r.json();
            if(data.status_code!==0) continue;
            const profile=data.user_profile ?? data.user?.user_profile;
            const name=profile?.nick_name;
            const nickname=typeof name==='string'&&name.trim()&&name.length<=128 ? name.trim() : null;
            const identifiers=[data.uid,data.user_id,data.user?.uid,data.user?.user_id,profile?.uid,profile?.unique_id];
            const verified=identifiers.some(id=>/^[a-z0-9_.-]{2,128}$/i.test(String(id ?? '')) &&
              (typeof id==='number' || typeof id==='string') && !/^0+$/.test(String(id)));
            if(verified) return {verified,nickname:nickname ?? fallback};
            fallback ??= nickname;
          } catch(_) {}
        }
      }
      const checked=document.querySelector('[role="menuitemradio"][aria-checked="true"] [class*="account-name"]');
      const name=checked?.textContent?.trim(); return {verified:false,nickname:fallback ?? (name&&name.length<=128?name:null)};
    })()"#;
    let params = chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
        .expression(script)
        .await_promise(true)
        .return_by_value(true)
        .build()
        .map_err(|_| UserError("无法读取平台昵称".into()))?;
    let result = page.evaluate_expression(params).await?.into_value()?;
    Ok(result)
}
#[async_trait]
impl PlatformAdapter for Douyin {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "抖音",
            verified: false,
            separate_prepare: true,
            custom_cover: false,
            submit_enabled: true,
            image_posts: true,
        }
    }
    fn prepare_timeout(&self, job: &Job) -> Duration {
        upload_timeout(job.content.asset.size) + Duration::from_secs(8 * 60)
    }

    async fn login(
        &self,
        store: &Store,
        account: &Account,
        cancel: CancellationToken,
    ) -> Result<SessionStatus> {
        let lease = store.lease(&account.id)?;
        let Some((mut browser, page, state)) = creator_session(
            &lease.path,
            CREATOR_HOME_URL,
            &cancel,
            &self.sign_ins,
            &account.id,
            || store.sync_session(&account.id, SessionStatus::NeedsLogin, None),
        )
        .await?
        else {
            return Ok(SessionStatus::NeedsLogin);
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(UserError("已取消登录".into()).into()),
            result = async {
                let nickname = session_nickname(&page, state).await;
                store.sync_session(&account.id, SessionStatus::Ready, nickname)
                    .map_err(|_| UserError("已识别登录，但保存账号状态失败".into()))?;
                Ok(SessionStatus::Ready)
            } => result,
        };
        browser.close().await?;
        result
    }
    async fn prepare(&self, ctx: RunContext) -> Result<Box<dyn Prepared>> {
        let content = &ctx.job.content;
        content.validate().map_err(|e| UserError(e.to_string()))?;
        let is_photo_post = content.asset.kind == MediaKind::Images;
        let (upload_url, media_label) = if is_photo_post {
            (IMAGE_UPLOAD_URL, "图文")
        } else {
            (UPLOAD_URL, "视频")
        };
        let input_ready = |state: &CreatorPage| {
            if is_photo_post {
                state.photo_upload_ready
            } else {
                state.upload_ready
            }
        };
        let profile = ctx.store.root.join("profiles").join(&ctx.job.account.id);
        let Some((mut browser, page, state)) = creator_session(
            &profile,
            upload_url,
            &ctx.cancel,
            &self.sign_ins,
            &ctx.job.account.id,
            || {
                ctx.store
                    .sync_session(&ctx.job.account.id, SessionStatus::NeedsLogin, None)?;
                ctx.stage(
                    JobState::WaitingUser,
                    "请在任务卡片中使用抖音 App 扫码，登录后会自动继续准备",
                )
            },
        )
        .await?
        else {
            return Err(UserError("等待登录超时，请先在账号管理中完成登录".into()).into());
        };
        let result: Result<Option<PhotoSnapshot>> = async {
            let upload_ready = input_ready(&state);
            let nickname = session_nickname(&page, state).await;
            ctx.store
                .sync_session(&ctx.job.account.id, SessionStatus::Ready, nickname)?;
            if !upload_ready {
                // Successful login may have sent the upload tab back to the creator homepage.
                page.goto(upload_url).await.map_err(|_| {
                    UserError(format!(
                        "已登录，但无法打开抖音{media_label}上传页，请重新准备"
                    ))
                })?;
                let ready = if is_photo_post {
                    wait_for_photo_upload_tab(
                        &page,
                        CREATOR_ORIGIN,
                        &ctx.cancel,
                        Duration::from_secs(30),
                    )
                    .await?
                } else {
                    wait_for_creator_page(&page, &ctx.cancel, Duration::from_secs(30), input_ready)
                        .await?
                        .is_some()
                };
                if !ready {
                    return Err(UserError(format!(
                        "已登录，但未找到抖音{media_label}上传控件，请检查平台页面后重新准备"
                    ))
                    .into());
                }
            }
            let limit = upload_timeout(content.asset.size);
            ctx.stage(
                JobState::Uploading,
                &format!(
                    "正在等待抖音上传和处理{media_label}，此素材最多等待 {} 分钟",
                    limit.as_secs().div_ceil(60)
                ),
            )?;
            let progress = self.upload_progress.begin(&ctx.job.id);
            if is_photo_post {
                content
                    .asset
                    .verify()
                    .map_err(|e| UserError(e.to_string()))?;
                select_photos(&page, CREATOR_ORIGIN, &content.asset.images)
                    .await
                    .map_err(|_| {
                        UserError("无法把本地图片交给抖音，请检查图片和上传入口。".into())
                    })?;
                let count = content.asset.images.len();
                wait_for_uploaded_photos(&page, CREATOR_ORIGIN, count, limit, &progress).await?;
                content
                    .asset
                    .verify()
                    .map_err(|e| UserError(e.to_string()))?;
                progress.set(Some(UploadProgress::FillingForm));
                let observation = observe_photos(
                    &page,
                    CREATOR_ORIGIN,
                    count,
                    "fill",
                    &content.title,
                    &prepared_description(content),
                )
                .await
                .map_err(|_| UserError("无法填写抖音图文的标题和描述。".into()))?;
                return observation.photos.map(Some).ok_or_else(|| {
                    UserError(
                        "抖音已收到图片，但无法确认标题和描述。请检查文字长度后重新准备。".into(),
                    )
                    .into()
                });
            }
            upload_file(&page, FILE_SELECTOR, &content.asset.path)
                .await
                .map_err(|_| {
                    UserError(
                        "无法把本地视频交给抖音上传，请检查素材是否可访问及上传页状态。".into(),
                    )
                })?;
            wait_for_uploaded_video(&page, limit, &progress).await?;
            content
                .asset
                .verify()
                .map_err(|e| UserError(e.to_string()))?;
            let text = prepared_description(content);
            fill_fields(&page, &content.title, &text)
                .await
                .map_err(|_| {
                    UserError(
                        "抖音已显示上传完成，但无法确认标题和描述已填写。未执行最终提交。".into(),
                    )
                })?;
            Ok(None)
        }
        .await;
        let photos = match result {
            Ok(photos) => photos,
            Err(error) => {
                browser.close().await?;
                return Err(error);
            }
        };
        Ok(Box::new(DouyinPrepared {
            browser,
            page,
            content: content.clone(),
            photos,
            sign_ins: self.sign_ins.clone(),
            account_id: ctx.job.account.id.clone(),
            publish_attempted: false,
            receipt_observer: None,
        }))
    }
}
struct DouyinPrepared {
    browser: BrowserSession,
    page: Page,
    content: Content,
    photos: Option<PhotoSnapshot>,
    sign_ins: SignIns,
    account_id: String,
    publish_attempted: bool,
    receipt_observer: Option<DouyinReceiptObserver>,
}
#[async_trait]
impl Prepared for DouyinPrepared {
    async fn preflight_submission(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.publish_attempted,
            "该任务已尝试提交，请在抖音核对结果。"
        );
        preflight(&self.page, &self.content, self.photos.as_ref()).await?;
        self.receipt_observer = Some(DouyinReceiptObserver::start(&self.page).await?);
        Ok(())
    }

    fn submission_timeout(&self) -> Duration {
        Duration::from_secs(6 * 60)
    }

    async fn submit(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.publish_attempted && self.receipt_observer.is_some(),
            "发布检查未完成，或该任务已尝试提交。"
        );
        self.publish_attempted = true;
        click_publish(&self.page, &self.content, self.photos.as_ref()).await
    }
    async fn inspect_result(&mut self) -> Result<Option<Receipt>> {
        if !self.publish_attempted {
            return Ok(None);
        }
        let Some(observer) = self.receipt_observer.take() else {
            return Ok(None);
        };
        tokio::select! {
            biased;
            receipt = observer.wait_for(&self.page, self.submission_timeout()) => receipt,
            _ = relay_publish_verification(&self.page, &self.sign_ins, &self.account_id) => Ok(None),
        }
    }
    async fn close(&mut self) -> Result<()> {
        self.browser.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_progress_uses_only_valid_observed_percentages() {
        let read = |status: &str, percent: serde_json::Value| {
            serde_json::from_value::<UploadObservation>(
                serde_json::json!({"status": status, "percent": percent}),
            )
            .unwrap()
            .progress()
        };
        assert_eq!(
            read("uploading", serde_json::json!(62)),
            Some(UploadProgress::Uploading { percent: Some(62) })
        );
        for invalid in [
            serde_json::json!(101),
            serde_json::json!(-1),
            serde_json::json!(23.5),
            serde_json::json!("42"),
        ] {
            assert_eq!(
                read("uploading", invalid),
                Some(UploadProgress::Uploading { percent: None })
            );
        }
        assert_eq!(
            read("processing", serde_json::json!(100)),
            Some(UploadProgress::Processing)
        );
        assert_eq!(read("waiting", serde_json::json!(50)), None);
    }

    #[test]
    fn upload_timeout_accounts_for_large_files() {
        let small = upload_timeout(5 * 1024 * 1024);
        let large = upload_timeout(3 * 1024 * 1024 * 1024);
        assert_eq!(small, Duration::from_secs(605));
        assert!(large > Duration::from_secs(900));
        assert_eq!(upload_timeout(u64::MAX), Duration::from_secs(2 * 60 * 60));
    }

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn verification_preview_fills_the_selected_field_without_submitting() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser.browser.new_page("about:blank").await?;
        page.set_content(
            r#"
            <input id="code" value="000000" style="position:absolute;left:24px;top:24px;width:160px;height:40px">
            <button id="confirm" style="position:absolute;left:24px;top:84px;width:160px;height:40px">Confirm</button>
            <script>
              const code = document.querySelector('#code');
              window.inputEvents = 0;
              code.addEventListener('input', () => window.inputEvents++);
              document.querySelector('#confirm').addEventListener('click', () => {
                window.confirmed = code.value;
              });
            </script>
        "#,
        )
        .await?;
        let sign_ins = SignIns::default();
        let sign_in = sign_ins.begin("test-account");
        let (width, height) = capture_verification(&page, &sign_in).await?;
        let preview = &sign_ins.snapshot()["test-account"];
        assert!(preview.verification.is_some());
        let verification_id = preview.verification_id.unwrap();
        let viewport = (width, height);

        assert!(
            fill_verification_field(&page, "null", "123456".into())
                .await
                .is_err()
        );
        let select_code = VerificationAction::Click {
            x: 40. / f64::from(width),
            y: 40. / f64::from(height),
        };
        assert!(sign_ins.send_verification_action("test-account", verification_id, select_code));
        forward_verification_action(
            &page,
            "null",
            viewport,
            sign_in.verification_action().await.unwrap(),
        )
        .await;
        assert!(
            fill_verification_field(&page, "https://unrelated.invalid", "123456".into())
                .await
                .is_err()
        );
        let fill = VerificationAction::FillText("309167".into());
        assert!(sign_ins.send_verification_action("test-account", verification_id, fill));
        let feedback = forward_verification_action(
            &page,
            "null",
            viewport,
            sign_in.verification_action().await.unwrap(),
        )
        .await;
        sign_in.set_input_feedback(feedback);
        assert_eq!(sign_ins.snapshot()["test-account"].input_feedback, feedback);
        assert!(
            truth(
                &page,
                "document.querySelector('#code').value === '309167' && window.inputEvents === 1 && window.confirmed === undefined"
            )
            .await?
        );

        forward_verification_action(
            &page,
            "null",
            viewport,
            VerificationAction::Click {
                x: 40. / f64::from(width),
                y: 100. / f64::from(height),
            },
        )
        .await;
        assert!(truth(&page, "window.confirmed === '309167'").await?);
        assert!(sign_ins.send_verification_action(
            "test-account",
            verification_id,
            VerificationAction::FillText("queued".into()),
        ));
        sign_in.update(SignInStatus::CheckingAccount, None);
        assert_eq!(sign_ins.snapshot()["test-account"].input_feedback, None);
        capture_verification(&page, &sign_in).await?;
        assert_ne!(
            sign_ins.snapshot()["test-account"].verification_id,
            Some(verification_id)
        );
        assert!(!sign_ins.send_verification_action(
            "test-account",
            verification_id,
            VerificationAction::FillText("stale".into()),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), sign_in.verification_action())
                .await
                .is_err()
        );
        browser.close().await?;
        Ok(())
    }
}
