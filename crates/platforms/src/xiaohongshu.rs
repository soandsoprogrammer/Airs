use crate::{browser::BrowserSession, sign_in::SignIns, upload_progress::UploadProgressRegistry};
use anyhow::Result;
use async_trait::async_trait;
use chromiumoxide::Page;
use publisher_core::{
    Account, Capabilities, Job, JobState, PlatformAdapter, Prepared, Receipt, RunContext,
    SessionStatus, Store, UserError,
};
use std::{path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

#[path = "xiaohongshu_editor.rs"]
mod editor;
#[path = "xiaohongshu_login.rs"]
mod login;
#[path = "xiaohongshu_receipt.rs"]
mod receipt;

const CREATOR_ORIGIN: &str = "https://creator.xiaohongshu.com";
const HOME_URL: &str = "https://creator.xiaohongshu.com/new/home";
pub const CREATOR_MANAGE_URL: &str = "https://creator.xiaohongshu.com/new/note-manager";

pub struct Xiaohongshu {
    sign_ins: SignIns,
    upload_progress: UploadProgressRegistry,
}

impl Xiaohongshu {
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
            return Err(UserError("已取消小红书登录".into()).into());
        }
        let sign_in = self.sign_ins.begin(&account.id);
        let mut browser = BrowserSession::launch(profile).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(UserError("已取消小红书登录".into()).into()),
            result = tokio::time::timeout(Duration::from_secs(300), async {
                let page = browser.browser.new_page(HOME_URL).await
                    .map_err(|_| UserError("无法打开小红书创作服务平台，请检查网络后重试".into()))?;
                let nickname = login::wait_for_login(&page, &sign_in, || {
                    store.sync_session(&account.id, SessionStatus::NeedsLogin, None)?;
                    on_needs_login()
                }).await?;
                store.sync_session(&account.id, SessionStatus::Ready, Some(nickname.clone()))
                    .map_err(|_| UserError("已识别小红书账号，但保存登录状态失败".into()))?;
                Ok(Some((page, nickname)))
            }) => result.unwrap_or(Ok(None)),
        };
        match result {
            Ok(Some((page, nickname))) => Ok(Some((browser, page, nickname))),
            other => {
                browser.close().await?;
                other.map(|_| None)
            }
        }
    }
}

#[async_trait]
impl PlatformAdapter for Xiaohongshu {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "小红书",
            verified: false,
            separate_prepare: true,
            custom_cover: false,
            submit_enabled: true,
            image_posts: true,
        }
    }

    fn prepare_timeout(&self, job: &Job) -> Duration {
        editor::upload_timeout(job.content.asset.size) + Duration::from_secs(7 * 60)
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
        let expected = editor::Expected::new(&ctx.job.content);
        let profile = ctx.store.root.join("profiles").join(&ctx.job.account.id);
        let Some((mut browser, page, nickname)) = self
            .creator_session(&profile, &ctx.store, &ctx.job.account, &ctx.cancel, || {
                ctx.stage(
                    JobState::WaitingUser,
                    "请在任务卡片中完成小红书登录，登录后会自动继续准备",
                )
            })
            .await?
        else {
            return Err(UserError("等待小红书登录超时，请先在账号管理中完成登录".into()).into());
        };
        if let Err(error) =
            editor::prepare(&page, &ctx, &self.upload_progress, &nickname, &expected).await
        {
            browser.close().await?;
            return Err(error);
        }
        Ok(Box::new(XiaohongshuPrepared {
            browser,
            page,
            nickname,
            expected,
            publish_attempted: false,
            receipt_observer: None,
        }))
    }
}

struct XiaohongshuPrepared {
    browser: BrowserSession,
    page: Page,
    nickname: String,
    expected: editor::Expected,
    publish_attempted: bool,
    receipt_observer: Option<receipt::Observer>,
}

#[async_trait]
impl Prepared for XiaohongshuPrepared {
    async fn preflight_submission(&mut self) -> Result<()> {
        anyhow::ensure!(!self.publish_attempted, "该小红书任务已尝试发布");
        editor::publish_point(&self.page, &self.nickname, &self.expected).await?;
        self.receipt_observer = Some(receipt::Observer::start(&self.page).await?);
        Ok(())
    }

    async fn submit(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.publish_attempted && self.receipt_observer.is_some(),
            "小红书发布检查未完成，或该任务已尝试提交"
        );
        let point = editor::publish_point(&self.page, &self.nickname, &self.expected).await?;
        self.publish_attempted = true;
        self.page.click(point).await?;
        Ok(())
    }

    async fn inspect_result(&mut self) -> Result<Option<Receipt>> {
        if !self.publish_attempted {
            return Ok(None);
        }
        let Some(observer) = self.receipt_observer.take() else {
            return Ok(None);
        };
        observer
            .wait_for(&self.page, &self.expected, Duration::from_secs(75))
            .await
    }

    async fn close(&mut self) -> Result<()> {
        self.browser.close().await
    }
}
