use publisher_core::*;
use publisher_platforms::sign_in::{SignInPreview, SignIns, VerificationAction};
use publisher_platforms::upload_progress::{UploadProgress, UploadProgressRegistry};
use publisher_platforms::{Douyin, Kuaishou, Xiaohongshu};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
};
use tokio_util::sync::CancellationToken;

pub const ACCOUNT_PLATFORMS: [Platform; 3] =
    [Platform::Douyin, Platform::Kuaishou, Platform::Xiaohongshu];

#[derive(Default, PartialEq, Eq)]
pub struct Snapshot {
    pub accounts: Vec<Account>,
    pub assets: Vec<Asset>,
    pub jobs: Vec<Job>,
    pub events: HashMap<String, Vec<JobEvent>>,
    pub logins: Vec<String>,
    pub sign_ins: HashMap<String, SignInPreview>,
    pub upload_progress: HashMap<String, UploadProgress>,
    pub prepare_enabled_platforms: Vec<Platform>,
    pub image_enabled_platforms: Vec<Platform>,
    pub submit_enabled_platforms: Vec<Platform>,
    pub draft: Option<Content>,
    pub browser: String,
}
impl Snapshot {
    pub fn can_prepare(&self, platform: Platform, kind: MediaKind) -> bool {
        self.prepare_enabled_platforms.contains(&platform)
            && (kind == MediaKind::Video || self.image_enabled_platforms.contains(&platform))
    }

    pub fn can_submit(&self, job: &Job) -> bool {
        job.state == JobState::WaitingUser
            && job.ready_to_submit
            && self.can_prepare(job.account.platform, job.content.asset.kind)
            && self
                .submit_enabled_platforms
                .contains(&job.account.platform)
            && self
                .accounts
                .iter()
                .any(|account| account.id == job.account.id)
    }
}
pub enum Update {
    Snapshot(Box<Snapshot>),
    Message(String),
    AccountAdded(String),
    ComposeAssetImported(String, MediaKind),
    BatchEnqueueResult {
        key: String,
        result: Result<(), String>,
    },
}
pub enum Command {
    AddAccount(Platform),
    Rename(String, String),
    DeleteAccount(String),
    Login(String),
    CancelLogin(String),
    RefreshSignIn(String),
    VerificationInput(String, u64, VerificationAction),
    Import {
        paths: Vec<PathBuf>,
        kind: MediaKind,
        select_for_compose: bool,
    },
    Enqueue {
        key: String,
        accounts: Vec<String>,
        content: Content,
    },
    SaveDraft(Content),
    Cancel(String),
    DeleteJob(String),
    ConfirmNotPublished(String),
    Resume(String),
    Submit(String),
    SetBrowser(String),
    Shutdown,
}
pub struct Backend {
    pub tx: tokio::sync::mpsc::UnboundedSender<Command>,
    pub rx: mpsc::Receiver<Update>,
    pub root: PathBuf,
    pub finished: Arc<(Mutex<bool>, Condvar)>,
}
struct Worker {
    store: Store,
    engine: Engine,
    updates: mpsc::Sender<Update>,
    logins: Arc<Mutex<HashMap<String, CancellationToken>>>,
    sign_ins: SignIns,
    upload_progress: UploadProgressRegistry,
}
impl Worker {
    fn refresh(&self) -> anyhow::Result<()> {
        let mut jobs = self.store.jobs()?;
        jobs.retain(|job| ACCOUNT_PLATFORMS.contains(&job.account.platform));
        let events = jobs
            .iter()
            .take(100)
            .map(|j| Ok((j.id.clone(), self.store.events(&j.id)?)))
            .collect::<anyhow::Result<_>>()?;
        let browser = std::fs::read(self.store.root.join("settings.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .and_then(|v| {
                v.get("browser")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                publisher_platforms::browser::chrome_path()
                    .to_string_lossy()
                    .into_owned()
            });
        let capabilities: Vec<_> = ACCOUNT_PLATFORMS
            .into_iter()
            .filter_map(|platform| {
                Some((platform, self.engine.adapter(platform).ok()?.capabilities()))
            })
            .collect();
        let snap = Snapshot {
            accounts: self.accounts()?,
            assets: self.store.assets()?,
            jobs,
            events,
            logins: self.logins.lock().unwrap().keys().cloned().collect(),
            sign_ins: self.sign_ins.snapshot(),
            upload_progress: self.upload_progress.snapshot(),
            prepare_enabled_platforms: capabilities
                .iter()
                .filter_map(|(platform, cap)| cap.separate_prepare.then_some(*platform))
                .collect(),
            submit_enabled_platforms: capabilities
                .iter()
                .filter_map(|(platform, cap)| cap.submit_enabled.then_some(*platform))
                .collect(),
            image_enabled_platforms: capabilities
                .iter()
                .filter_map(|(platform, cap)| cap.image_posts.then_some(*platform))
                .collect(),
            draft: self.store.draft("composer")?,
            browser,
        };
        let _ = self.updates.send(Update::Snapshot(Box::new(snap)));
        Ok(())
    }
    fn accounts(&self) -> anyhow::Result<Vec<Account>> {
        let mut accounts = self.store.accounts()?;
        accounts.retain(|account| ACCOUNT_PLATFORMS.contains(&account.platform));
        Ok(accounts)
    }
    fn account(&self, id: &str) -> anyhow::Result<Account> {
        self.accounts()?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| anyhow::anyhow!("账号不存在"))
    }
    fn job(&self, id: &str) -> anyhow::Result<Job> {
        let job = self.store.job(id)?;
        anyhow::ensure!(
            ACCOUNT_PLATFORMS.contains(&job.account.platform),
            "任务不存在"
        );
        Ok(job)
    }
    fn ensure_can_prepare(&self, platform: Platform, content: &Content) -> anyhow::Result<()> {
        let adapter = self.engine.adapter(platform)?;
        let capabilities = adapter.capabilities();
        anyhow::ensure!(
            capabilities.separate_prepare,
            "{}暂未开放发布准备",
            platform.name()
        );
        anyhow::ensure!(
            content.asset.kind == MediaKind::Video || capabilities.image_posts,
            "{}暂未开放图文发布",
            platform.name()
        );
        adapter.validate_content(content)
    }
    fn enqueue(
        &self,
        key: &str,
        accounts: &[String],
        content: &Content,
    ) -> anyhow::Result<&'static str> {
        let selected: Vec<_> = self
            .accounts()?
            .into_iter()
            .filter(|a| accounts.contains(&a.id))
            .collect();
        anyhow::ensure!(
            accounts
                .iter()
                .all(|id| selected.iter().any(|a| &a.id == id)),
            "部分账号已不可用，请重新选择账号"
        );
        for account in &selected {
            self.ensure_can_prepare(account.platform, content)?;
        }
        let jobs = self.store.enqueue(key, &selected, content)?;
        anyhow::ensure!(!jobs.is_empty(), "该批次的任务记录已删除");
        let mut startup_failed = false;
        for job in jobs {
            if job.state == JobState::Queued
                && ACCOUNT_PLATFORMS.contains(&job.account.platform)
                && self.engine.start(job.id).is_err()
            {
                startup_failed = true;
            }
        }
        Ok(if startup_failed {
            "任务已建立。部分任务未能启动，可在任务记录中继续准备。"
        } else {
            "准备任务已建立，将停在最终提交前"
        })
    }
    fn handle(&self, cmd: Command) -> anyhow::Result<String> {
        match cmd {
            Command::AddAccount(p) => {
                anyhow::ensure!(ACCOUNT_PLATFORMS.contains(&p), "平台尚未接入");
                let account = self.store.add_account("", p)?;
                let update = if self.refresh().is_ok() {
                    Update::AccountAdded(account.id)
                } else {
                    Update::Message("账号已添加，暂时无法读取账号列表".into())
                };
                let _ = self.updates.send(update);
                Ok(String::new())
            }
            Command::Rename(id, alias) => {
                anyhow::ensure!(!alias.trim().is_empty(), "请填写新别名");
                self.account(&id)?;
                self.store.rename_account(&id, &alias)?;
                Ok("显示名称已更新".into())
            }
            Command::DeleteAccount(id) => {
                self.account(&id)?;
                anyhow::ensure!(
                    !self.logins.lock().unwrap().contains_key(&id),
                    "请先停止该账号的登录检查"
                );
                self.store.remove_account(&id)?;
                Ok("账号和本地登录资料已删除，历史任务可在任务记录中查看".into())
            }
            Command::Login(id) => {
                let a = self.account(&id)?;
                let platform = a.platform;
                anyhow::ensure!(
                    !self.logins.lock().unwrap().contains_key(&id),
                    "该账号正在登录"
                );
                let adapter = self.engine.adapter(a.platform)?;
                let cancel = CancellationToken::new();
                self.logins
                    .lock()
                    .unwrap()
                    .insert(id.clone(), cancel.clone());
                let store = self.store.clone();
                let updates = self.updates.clone();
                let logins = self.logins.clone();
                tokio::spawn(async move {
                    // Let the adapter close its browser normally, so Chrome flushes the session.
                    let result = adapter.login(&store, &a, cancel.clone()).await;
                    let message = match result {
                        Ok(SessionStatus::NeedsLogin) => {
                            match store.sync_session(&a.id, SessionStatus::NeedsLogin, None) {
                                Ok(()) => "等待登录超时；可再次点击登录 / 检查会话".into(),
                                Err(_) => "无法保存登录检查结果，请检查磁盘空间".into(),
                            }
                        }
                        Ok(session) => {
                            let a = store
                                .accounts()
                                .unwrap_or_default()
                                .into_iter()
                                .find(|x| x.id == a.id)
                                .unwrap_or(a);
                            match store.sync_session(&a.id, session, None) {
                                Ok(()) => format!("{}：{}", a.alias, session.label()),
                                Err(_) => "登录完成，但账号记录保存失败，请检查磁盘空间".into(),
                            }
                        }
                        Err(_) if cancel.is_cancelled() => "已停止本次登录检查".into(),
                        Err(e) => e
                            .downcast_ref::<UserError>()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| {
                                "无法检查登录；请检查账号是否正在使用、浏览器及网络".into()
                            }),
                    };
                    let _ = updates.send(Update::Message(message));
                    logins.lock().unwrap().remove(&id);
                });
                Ok(if platform == Platform::Xiaohongshu {
                    "正在检查小红书登录状态。需要登录时，会在账号详情中显示登录页。"
                } else {
                    "正在后台检查登录状态。需要扫码时，二维码会显示在账号详情中。"
                }
                .into())
            }
            Command::CancelLogin(id) => {
                if let Some(c) = self.logins.lock().unwrap().get(&id) {
                    c.cancel();
                }
                Ok("正在停止登录".into())
            }
            Command::RefreshSignIn(id) => {
                anyhow::ensure!(
                    self.sign_ins.request_refresh(&id),
                    "本次扫码已结束，请重新检查登录"
                );
                Ok("正在刷新登录二维码".into())
            }
            Command::VerificationInput(id, verification_id, action) => {
                let feedback_on_failure = match &action {
                    VerificationAction::FillText(_) => {
                        Some("暂时无法填入文字，请检查验证页面后重试")
                    }
                    VerificationAction::RequestSmsCode | VerificationAction::VerifySmsCode(_) => {
                        Some("短信验证页面已变化，请查看任务状态")
                    }
                    VerificationAction::Click { .. } => None,
                };
                let queued = self
                    .sign_ins
                    .send_verification_action(&id, verification_id, action);
                if !queued && let Some(message) = feedback_on_failure {
                    anyhow::bail!("{message}");
                }
                Ok(String::new())
            }
            Command::Import {
                paths,
                kind,
                select_for_compose,
            } => {
                let asset = match kind {
                    MediaKind::Video => self.store.add_asset(
                        paths
                            .first()
                            .ok_or_else(|| anyhow::anyhow!("请选择一个视频"))?,
                    )?,
                    MediaKind::Images => self.store.add_image_asset(&paths)?,
                };
                let success = "素材已导入，保留原文件引用";
                if !select_for_compose {
                    return Ok(success.into());
                }
                let message = if self.refresh().is_ok() {
                    let _ = self
                        .updates
                        .send(Update::ComposeAssetImported(asset.id, kind));
                    success
                } else {
                    "素材已导入，暂时无法读取素材列表"
                };
                let _ = self.updates.send(Update::Message(message.into()));
                Ok(String::new())
            }
            Command::SaveDraft(content) => {
                self.store.save_draft("composer", &content)?;
                Ok("草稿已保存，可在重启后恢复".into())
            }
            Command::Enqueue {
                key,
                accounts,
                content,
            } => {
                let result = match self.enqueue(&key, &accounts, &content) {
                    Ok(message) => {
                        let message = if self.refresh().is_ok() {
                            message
                        } else {
                            "任务已建立，暂时无法读取任务列表"
                        };
                        let _ = self.updates.send(Update::Message(message.into()));
                        Ok(())
                    }
                    Err(error) => Err(error.to_string()),
                };
                let _ = self
                    .updates
                    .send(Update::BatchEnqueueResult { key, result });
                Ok(String::new())
            }
            Command::Cancel(id) => {
                self.job(&id)?;
                self.engine.cancel(&id)?;
                Ok("停止请求已发送；提交后的任务保留待确认结果".into())
            }
            Command::DeleteJob(id) => {
                self.job(&id)?;
                self.store.remove_job(&id)?;
                Ok("任务记录已删除".into())
            }
            Command::ConfirmNotPublished(id) => {
                self.job(&id)?;
                self.store.confirm_not_published(&id)?;
                Ok("已记录为未发布。可在任务卡片选择重新准备。".into())
            }
            Command::Resume(id) => {
                let job = self.job(&id)?;
                self.ensure_can_prepare(job.account.platform, &job.content)?;
                self.engine.resume(id)?;
                Ok("已继续准备；最终提交仍需单独操作".into())
            }
            Command::Submit(id) => {
                let platform = self.job(&id)?.account.platform;
                self.engine.submit(&id)?;
                Ok(format!("已请求发布到{}，等待任务结果", platform.name()))
            }
            Command::SetBrowser(path) => {
                let path = PathBuf::from(path.trim());
                anyhow::ensure!(path.is_file(), "浏览器路径必须指向可执行文件");
                let raw = serde_json::to_string(&serde_json::json!({"browser":path}))?;
                let temporary = self.store.root.join("settings.tmp");
                std::fs::write(&temporary, raw)?;
                std::fs::rename(temporary, self.store.root.join("settings.json"))?;
                Ok("浏览器路径已保存，下次打开账号时生效".into())
            }
            Command::Shutdown => unreachable!(),
        }
    }
    async fn shutdown(&self) {
        self.engine.cancel_all();
        for token in self.logins.lock().unwrap().values() {
            token.cancel();
        }
        for _ in 0..200 {
            if self.engine.active_count() == 0 && self.logins.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
impl Backend {
    pub fn new(store: Store) -> Self {
        let root = store.root.clone();
        let (tx, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let (updates, rx) = mpsc::channel();
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let completion = finished.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("后台启动失败");
            runtime.block_on(async move {
                let douyin = Douyin::default();
                let kuaishou =
                    Kuaishou::new(douyin.sign_ins.clone(), douyin.upload_progress.clone());
                let xiaohongshu =
                    Xiaohongshu::new(douyin.sign_ins.clone(), douyin.upload_progress.clone());
                let worker = Worker {
                    sign_ins: douyin.sign_ins.clone(),
                    upload_progress: douyin.upload_progress.clone(),
                    engine: Engine::new(
                        store.clone(),
                        vec![
                            (Platform::Douyin, Arc::new(douyin)),
                            (Platform::Kuaishou, Arc::new(kuaishou)),
                            (Platform::Xiaohongshu, Arc::new(xiaohongshu)),
                        ],
                    ),
                    store,
                    updates,
                    logins: Default::default(),
                };
                let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
                loop {
                    let cmd = tokio::select! {
                        _ = ticker.tick() => {
                            if worker.refresh().is_err() {
                                let _ = worker.updates.send(Update::Message(
                                    "读取本地记录失败，请检查数据目录与磁盘空间".into(),
                                ));
                            }
                            continue;
                        }
                        cmd = commands.recv() => cmd,
                    };
                    let Some(cmd) = cmd else {
                        worker.shutdown().await;
                        break;
                    };
                    if matches!(cmd, Command::Shutdown) {
                        worker.shutdown().await;
                        break;
                    }
                    let message = worker.handle(cmd).unwrap_or_else(|e| e.to_string());
                    if !message.is_empty() {
                        let _ = worker.updates.send(Update::Message(message));
                        let _ = worker.refresh();
                    }
                }
            });
            runtime.shutdown_timeout(std::time::Duration::from_secs(3));
            *completion.0.lock().unwrap() = true;
            completion.1.notify_all();
        });
        Self {
            tx,
            rx,
            root,
            finished,
        }
    }
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}
