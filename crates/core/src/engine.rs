use crate::*;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct RunContext {
    pub store: Store,
    pub job: Job,
    pub cancel: CancellationToken,
}
impl RunContext {
    pub fn stage(&self, state: JobState, msg: &str) -> Result<()> {
        self.store.transition(&self.job.id, state, msg)?;
        Ok(())
    }
}
#[async_trait]
pub trait Prepared: Send {
    async fn preflight_submission(&mut self) -> Result<()> {
        Ok(())
    }
    fn submission_timeout(&self) -> Duration {
        Duration::from_secs(120)
    }
    async fn submit(&mut self) -> Result<()>;
    async fn inspect_result(&mut self) -> Result<Option<Receipt>>;
    async fn close(&mut self) -> Result<()>;
}
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn validate_content(&self, _content: &Content) -> Result<()> {
        Ok(())
    }
    fn prepare_timeout(&self, _job: &Job) -> Duration {
        Duration::from_secs(900)
    }
    async fn login(
        &self,
        store: &Store,
        account: &Account,
        cancel: CancellationToken,
    ) -> Result<SessionStatus>;
    async fn prepare(&self, ctx: RunContext) -> Result<Box<dyn Prepared>>;
}
pub const MAX_CONCURRENT_JOBS: usize = 3;

struct QueuedJob {
    id: String,
    account_id: String,
}

struct RunningJob {
    account_id: String,
    cancel: CancellationToken,
    submit: Arc<Notify>,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<QueuedJob>,
    running: HashMap<String, RunningJob>,
}

impl Queue {
    fn contains(&self, id: &str) -> bool {
        self.running.contains_key(id) || self.pending.iter().any(|job| job.id == id)
    }
}

#[derive(Clone)]
pub struct Engine {
    store: Store,
    adapters: Arc<HashMap<String, Arc<dyn PlatformAdapter>>>,
    queue: Arc<Mutex<Queue>>,
}
impl Engine {
    pub fn new(store: Store, adapters: Vec<(Platform, Arc<dyn PlatformAdapter>)>) -> Self {
        Self {
            store,
            adapters: Arc::new(
                adapters
                    .into_iter()
                    .map(|(p, a)| (format!("{p:?}"), a))
                    .collect(),
            ),
            queue: Default::default(),
        }
    }
    pub fn adapter(&self, p: Platform) -> Result<Arc<dyn PlatformAdapter>> {
        self.adapters
            .get(&format!("{p:?}"))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("平台尚未接入"))
    }
    pub fn cancel_all(&self) {
        let mut queue = self.queue.lock().unwrap();
        for job in queue.running.values() {
            job.cancel.cancel();
        }
        for job in queue.pending.drain(..) {
            let _ = self.cancel_queued(&job.id);
        }
    }
    pub fn active_count(&self) -> usize {
        let queue = self.queue.lock().unwrap();
        queue.running.len() + queue.pending.len()
    }
    pub fn scheduled_jobs(&self) -> HashSet<String> {
        let queue = self.queue.lock().unwrap();
        queue
            .running
            .keys()
            .chain(queue.pending.iter().map(|job| &job.id))
            .cloned()
            .collect()
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        let mut queue = self.queue.lock().unwrap();
        if let Some(job) = queue.running.get(id) {
            job.cancel.cancel();
        } else {
            self.cancel_queued(id)?;
            queue.pending.retain(|job| job.id != id);
        }
        Ok(())
    }
    fn cancel_queued(&self, id: &str) -> Result<()> {
        if self.store.job(id)?.state == JobState::Queued {
            self.store
                .transition(id, JobState::Cancelled, "排队任务已取消")?;
        }
        Ok(())
    }
    pub fn submit(&self, id: &str) -> Result<()> {
        let j = self.store.job(id)?;
        ensure!(
            self.adapter(j.account.platform)?
                .capabilities()
                .submit_enabled,
            "该平台真实提交尚未开放"
        );
        ensure!(
            j.state == JobState::WaitingUser && j.ready_to_submit,
            "任务尚未准备完成"
        );
        self.queue
            .lock()
            .unwrap()
            .running
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("该任务已停止，请重新准备"))?
            .submit
            .notify_one();
        Ok(())
    }
    pub fn start(&self, id: String) -> Result<()> {
        self.enqueue(id, false)
    }
    pub fn resume(&self, id: String) -> Result<()> {
        self.enqueue(id, true)
    }
    fn enqueue(&self, id: String, retry: bool) -> Result<()> {
        let mut queue = self.queue.lock().unwrap();
        ensure!(!queue.contains(&id), "任务已经在队列中或正在执行");
        let job = self.store.job(&id)?;
        if retry && matches!(job.state, JobState::Failed | JobState::Cancelled) {
            job.content.validate()?;
            job.content.validate_for_platform(job.account.platform)?;
            self.store
                .transition(&id, JobState::Queued, "用户重新准备；保留之前的执行记录")?;
        } else {
            ensure!(job.state == JobState::Queued, "任务不在队列中");
        }
        queue.pending.push_back(QueuedJob {
            id,
            account_id: job.account.id,
        });
        self.dispatch(&mut queue);
        Ok(())
    }
    fn dispatch(&self, queue: &mut Queue) {
        while queue.running.len() < MAX_CONCURRENT_JOBS {
            // Leave tasks for busy accounts in place while other accounts use free slots.
            let Some(index) = queue.pending.iter().position(|pending| {
                !queue
                    .running
                    .values()
                    .any(|running| running.account_id == pending.account_id)
            }) else {
                break;
            };
            let QueuedJob { id, account_id } = queue.pending.remove(index).unwrap();
            let cancel = CancellationToken::new();
            let submit = Arc::new(Notify::new());
            queue.running.insert(
                id.clone(),
                RunningJob {
                    account_id,
                    cancel: cancel.clone(),
                    submit: submit.clone(),
                },
            );
            let engine = self.clone();
            tokio::spawn(async move {
                let _ = engine.run(&id, cancel, submit).await;
                let mut queue = engine.queue.lock().unwrap();
                queue.running.remove(&id);
                engine.dispatch(&mut queue);
            });
        }
    }
    async fn run(&self, id: &str, cancel: CancellationToken, submit: Arc<Notify>) -> Result<()> {
        if cancel.is_cancelled() {
            return self.cancel_queued(id);
        }
        if self.store.job(id)?.state != JobState::Queued {
            return Ok(());
        }
        let job = self.store.claim(id)?;
        let result = self.execute(job.clone(), cancel.clone(), submit).await;
        if let Err(error) = &result {
            let state = self.store.job(id)?.state;
            let failure = error
                .downcast_ref::<UserError>()
                .map(|e| e.0.as_str())
                .unwrap_or("准备失败：请检查登录、素材、网络或平台页面变化后重新准备");
            let (next, msg) = if state == JobState::Submitting {
                (
                    JobState::Unknown,
                    "提交结果无法确认，请在平台核对；不会自动重发",
                )
            } else if cancel.is_cancelled() {
                (JobState::Cancelled, "提交前已停止；平台可能保留上传草稿")
            } else {
                (JobState::Failed, failure)
            };
            if state.active() {
                self.store.transition(id, next, msg)?;
            }
        }
        result
    }
    async fn execute(
        &self,
        job: Job,
        cancel: CancellationToken,
        submit: Arc<Notify>,
    ) -> Result<()> {
        let adapter = self.adapter(job.account.platform)?;
        let capabilities = adapter.capabilities();
        if !capabilities.separate_prepare {
            return Err(UserError("该平台尚不支持提交前暂停".into()).into());
        }
        if job.content.asset.kind == MediaKind::Images && !capabilities.image_posts {
            return Err(UserError("该平台尚不支持图文发布".into()).into());
        }
        if job.account.platform == Platform::Douyin
            && !job.content.douyin_video_covers.is_empty()
            && !capabilities.custom_cover
        {
            return Err(UserError("该平台尚不支持自定义封面".into()).into());
        }
        job.content
            .validate()
            .and_then(|()| job.content.validate_for_platform(job.account.platform))
            .map_err(|e| UserError(e.to_string()))?;
        adapter.validate_content(&job.content)?;
        let _profile = self
            .store
            .lease(&job.account.id)
            .map_err(|_| UserError("该账号正在登录或执行任务，请稍后重新准备".into()))?;
        let ctx = RunContext {
            store: self.store.clone(),
            job: job.clone(),
            cancel: cancel.clone(),
        };
        let prepare_timeout = adapter.prepare_timeout(&job);
        let mut prepared = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("已取消"),
            _ = tokio::time::sleep(prepare_timeout) => {
                return Err(UserError("发布准备已超时。请检查网络后重新准备。".into()).into());
            }
            result = adapter.prepare(ctx) => result?,
        };
        self.store.prepared(&job.id)?;
        let proceed = tokio::select! { _=submit.notified()=>true, _=cancel.cancelled()=>false, _=tokio::time::sleep(std::time::Duration::from_secs(1800))=>false };
        if !proceed {
            let _ = prepared.close().await;
            if !cancel.is_cancelled() {
                return Err(UserError("提交前等待已超时，浏览器已关闭；可重新准备".into()).into());
            }
            anyhow::bail!("已停止");
        }
        if cancel.is_cancelled() {
            let _ = prepared.close().await;
            anyhow::bail!("已取消");
        }
        if let Err(e) = job
            .content
            .asset
            .verify()
            .and_then(|()| job.content.verify_platform_files(job.account.platform))
        {
            let _ = prepared.close().await;
            return Err(UserError(e.to_string()).into());
        }
        let preflight = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(UserError("已停止提交".into()).into()),
            result = prepared.preflight_submission() => result,
        };
        if let Err(error) = preflight {
            let _ = prepared.close().await;
            return Err(error);
        }
        // This transaction must commit before the first platform submission action.
        self.store.transition(
            &job.id,
            JobState::Submitting,
            "正在提交；中断时将保留结果待确认",
        )?;
        let submission_timeout = prepared.submission_timeout();
        let result = tokio::select! {
            result = async { prepared.submit().await?; prepared.inspect_result().await } => result,
            _ = cancel.cancelled() => Err(anyhow::anyhow!("提交后停止等待")),
            _ = tokio::time::sleep(submission_timeout) => Err(anyhow::anyhow!("提交确认超时")),
        };
        let _ = prepared.close().await;
        match result {
            Ok(Some(receipt)) => {
                self.store.received(&job.id, receipt)?;
            }
            _ => {
                self.store.transition(
                    &job.id,
                    JobState::Unknown,
                    "提交结果待确认，请到平台核对；不会自动重发",
                )?;
            }
        }
        Ok(())
    }
}
