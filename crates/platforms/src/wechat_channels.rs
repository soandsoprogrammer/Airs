use crate::{browser::BrowserSession, sign_in::SignIns, upload_progress::UploadProgressRegistry};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use chromiumoxide::Page;
use publisher_core::{
    Account, Capabilities, Content, Job, JobState, MediaKind, PlatformAdapter, Prepared, Receipt,
    RunContext, SessionStatus, Store, UserError,
};
use std::{path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

#[path = "wechat_channels_editor.rs"]
mod editor;
#[path = "wechat_channels_login.rs"]
mod login;
#[path = "wechat_channels_receipt.rs"]
mod receipt;

const CREATOR_ORIGIN: &str = "https://channels.weixin.qq.com";
const HOME_URL: &str = "https://channels.weixin.qq.com/platform";
pub const CREATOR_MANAGE_URL: &str = "https://channels.weixin.qq.com/platform/post/list";
const LOGIN_WAIT: Duration = Duration::from_secs(5 * 60);
const RECEIPT_WAIT: Duration = Duration::from_secs(2 * 60);
const VIDEO_SIZE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

fn upload_timeout(bytes: u64) -> Duration {
    Duration::from_secs((600 + bytes.div_ceil(1024 * 1024)).min(2 * 60 * 60))
}

pub struct WechatChannels {
    sign_ins: SignIns,
    upload_progress: UploadProgressRegistry,
}

impl WechatChannels {
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
    ) -> Result<Option<(BrowserSession, Page, login::Identity)>> {
        if cancel.is_cancelled() {
            return Err(UserError("已取消视频号登录".into()).into());
        }
        let sign_in = self.sign_ins.begin(&account.id);
        let mut browser = BrowserSession::launch(profile).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(UserError("已取消视频号登录".into()).into()),
            result = tokio::time::timeout(LOGIN_WAIT, async {
                let page = browser.browser.new_page(HOME_URL).await.map_err(|_| {
                    UserError("无法打开视频号助手，请检查网络后重试".into())
                })?;
                let identity = login::wait_for_login(&page, &sign_in, || {
                    store.sync_session(&account.id, SessionStatus::NeedsLogin, None)?;
                    on_needs_login()
                }).await?;
                store.sync_session(&account.id, SessionStatus::Ready, identity.nickname.clone())
                    .map_err(|_| UserError("已识别视频号账号，但保存登录状态失败".into()))?;
                Ok(Some((page, identity)))
            }) => result.unwrap_or(Ok(None)),
        };
        match result {
            Ok(Some((page, identity))) => Ok(Some((browser, page, identity))),
            other => {
                browser.close().await?;
                other.map(|_| None)
            }
        }
    }
}

#[async_trait]
impl PlatformAdapter for WechatChannels {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "视频号",
            verified: false,
            separate_prepare: true,
            custom_cover: false,
            submit_enabled: true,
            image_posts: false,
        }
    }

    fn validate_content(&self, content: &Content) -> Result<()> {
        ensure!(
            content.asset.kind == MediaKind::Video,
            UserError("视频号目前仅支持视频发布".into())
        );
        ensure!(
            content.asset.size <= VIDEO_SIZE_LIMIT,
            UserError("视频号视频文件不能超过 4 GB".into())
        );
        editor::validate_content(content)
    }

    fn prepare_timeout(&self, job: &Job) -> Duration {
        upload_timeout(job.content.asset.size) + LOGIN_WAIT + Duration::from_secs(90)
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
        let content = &ctx.job.content;
        self.validate_content(content)?;
        let profile = ctx.store.root.join("profiles").join(&ctx.job.account.id);
        let Some((mut browser, page, identity)) = self
            .creator_session(&profile, &ctx.store, &ctx.job.account, &ctx.cancel, || {
                ctx.stage(
                    JobState::WaitingUser,
                    "请在任务卡片中登录视频号，登录后自动继续准备",
                )
            })
            .await?
        else {
            return Err(UserError("等待视频号登录超时，请先在账号管理中登录".into()).into());
        };
        let result = async {
            editor::open_editor(&page).await?;
            login::verify_identity(&page, &identity).await?;
            ctx.stage(JobState::Uploading, "正在向视频号助手上传视频")?;
            let progress = self.upload_progress.begin(&ctx.job.id);
            editor::upload_and_fill(
                &page,
                content,
                upload_timeout(content.asset.size),
                &progress,
            )
            .await
        }
        .await;
        match result {
            Ok(snapshot) => Ok(Box::new(WechatPrepared {
                browser,
                page,
                identity,
                content: content.clone(),
                snapshot,
                publish_attempted: false,
                receipt_observer: None,
            })),
            Err(error) => {
                browser.close().await?;
                Err(error)
            }
        }
    }
}

struct WechatPrepared {
    browser: BrowserSession,
    page: Page,
    identity: login::Identity,
    content: Content,
    snapshot: editor::EditorSnapshot,
    publish_attempted: bool,
    receipt_observer: Option<receipt::Observer>,
}

#[async_trait]
impl Prepared for WechatPrepared {
    async fn preflight_submission(&mut self) -> Result<()> {
        ensure!(!self.publish_attempted, "该视频号任务已尝试发表");
        login::verify_identity(&self.page, &self.identity).await?;
        editor::preflight(&self.page, &self.content, &self.snapshot).await?;
        self.receipt_observer = Some(receipt::Observer::start(&self.page).await?);
        Ok(())
    }

    fn submission_timeout(&self) -> Duration {
        RECEIPT_WAIT + Duration::from_secs(20)
    }

    async fn submit(&mut self) -> Result<()> {
        ensure!(
            !self.publish_attempted && self.receipt_observer.is_some(),
            "视频号发表检查未完成，或该任务已尝试发表"
        );
        login::verify_identity(&self.page, &self.identity).await?;
        self.publish_attempted = true;
        editor::click_publish(&self.page, &self.content, &self.snapshot).await
    }

    async fn inspect_result(&mut self) -> Result<Option<Receipt>> {
        if !self.publish_attempted {
            return Ok(None);
        }
        let Some(observer) = self.receipt_observer.take() else {
            return Ok(None);
        };
        observer
            .wait_for(
                &self.page,
                self.snapshot.description(),
                self.snapshot.short_title(),
                RECEIPT_WAIT,
            )
            .await
    }

    async fn close(&mut self) -> Result<()> {
        self.browser.close().await
    }
}
