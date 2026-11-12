use anyhow::Result;
use async_trait::async_trait;
use publisher_core::*;
use std::{
    future::pending,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct AuditAdapter {
    submits: Arc<AtomicUsize>,
    unknown: bool,
    stalled_budget: Option<Duration>,
}
struct AuditPrepared {
    ctx: RunContext,
    submits: Arc<AtomicUsize>,
    unknown: bool,
}
#[async_trait]
impl PlatformAdapter for AuditAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            image_posts: false,
            name: "测试夹具",
            verified: false,
            separate_prepare: true,
            custom_cover: true,
            submit_enabled: true,
        }
    }
    fn prepare_timeout(&self, _: &Job) -> Duration {
        self.stalled_budget.unwrap_or(Duration::from_secs(900))
    }
    async fn login(&self, _: &Store, _: &Account, _: CancellationToken) -> Result<SessionStatus> {
        Ok(SessionStatus::Ready)
    }
    async fn prepare(&self, ctx: RunContext) -> Result<Box<dyn Prepared>> {
        ctx.stage(JobState::Uploading, "本地测试夹具")?;
        if self.stalled_budget.is_some() {
            pending::<()>().await;
        }
        Ok(Box::new(AuditPrepared {
            ctx,
            submits: self.submits.clone(),
            unknown: self.unknown,
        }))
    }
}
#[async_trait]
impl Prepared for AuditPrepared {
    async fn submit(&mut self) -> Result<()> {
        // The adapter sees committed state even through a freshly opened connection.
        let reopened = Store::open(&self.ctx.store.root)?;
        assert_eq!(reopened.job(&self.ctx.job.id)?.state, JobState::Submitting);
        self.submits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn inspect_result(&mut self) -> Result<Option<Receipt>> {
        if self.unknown {
            Ok(None)
        } else {
            Ok(Some(Receipt {
                platform_id: "fixture-001".into(),
                evidence: "本地夹具回执，不是真实平台".into(),
                url: None,
            }))
        }
    }
    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}
fn setup(unknown: bool) -> (tempfile::TempDir, Store, Engine, Job, Arc<AtomicUsize>) {
    setup_with_stalled_budget(unknown, None)
}
fn setup_with_stalled_budget(
    unknown: bool,
    stalled_budget: Option<Duration>,
) -> (tempfile::TempDir, Store, Engine, Job, Arc<AtomicUsize>) {
    setup_for_platform(Platform::Simulation, unknown, stalled_budget)
}
fn setup_for_platform(
    platform: Platform,
    unknown: bool,
    stalled_budget: Option<Duration>,
) -> (tempfile::TempDir, Store, Engine, Job, Arc<AtomicUsize>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.add_account("本地测试账号", platform).unwrap();
    let path = dir.path().join("fixture.mp4");
    std::fs::write(&path, b"fixture").unwrap();
    let mut douyin_video_covers = DouyinVideoCovers::default();
    if platform == Platform::Douyin {
        let cover = dir.path().join("cover.png");
        std::fs::write(&cover, b"fixture cover").unwrap();
        douyin_video_covers.portrait = Some(ImageFile::inspect(&cover).unwrap());
    }
    let c = Content {
        asset: store.add_asset(&path).unwrap(),
        title: "测试".into(),
        description: String::new(),
        tags: vec![],
        douyin_video_covers,
    };
    let j = store.enqueue("request", &[a], &c).unwrap().remove(0);
    let count = Arc::new(AtomicUsize::new(0));
    let adapter = Arc::new(AuditAdapter {
        submits: count.clone(),
        unknown,
        stalled_budget,
    });
    let engine = Engine::new(store.clone(), vec![(platform, adapter)]);
    (dir, store, engine, j, count)
}
async fn wait(s: &Store, id: &str, state: JobState) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if s.job(id).unwrap().state == state {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn dry_run_stops_then_one_submit_has_committed_state() {
    let (_dir, s, e, j, count) = setup(false);
    e.start(j.id.clone()).unwrap();
    assert!(e.start(j.id.clone()).is_err());
    wait(&s, &j.id, JobState::WaitingUser).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(s.job(&j.id).unwrap().ready_to_submit);
    e.submit(&j.id).unwrap();
    let _ = e.submit(&j.id);
    wait(&s, &j.id, JobState::Submitted).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(e.resume(j.id.clone()).is_err());
}
#[tokio::test]
async fn changed_douyin_cover_stops_before_submission() {
    let (_dir, store, engine, job, submissions) = setup_for_platform(Platform::Douyin, false, None);
    engine.start(job.id.clone()).unwrap();
    wait(&store, &job.id, JobState::WaitingUser).await;
    let cover = job.content.douyin_video_covers.portrait.as_ref().unwrap();
    std::fs::write(&cover.path, b"the cover was replaced during confirmation").unwrap();
    engine.submit(&job.id).unwrap();
    wait(&store, &job.id, JobState::Failed).await;
    let failed = store.job(&job.id).unwrap();
    assert!(failed.message.contains("抖音竖版封面"));
    assert!(!failed.ready_to_submit);
    assert_eq!(submissions.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn ambiguous_result_is_never_retried() {
    let (_dir, s, e, j, count) = setup(true);
    e.start(j.id.clone()).unwrap();
    wait(&s, &j.id, JobState::WaitingUser).await;
    e.submit(&j.id).unwrap();
    wait(&s, &j.id, JobState::Unknown).await;
    assert!(e.resume(j.id.clone()).is_err());
    assert!(e.submit(&j.id).is_err());
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancel_at_prepare_boundary_does_not_submit() {
    let (_dir, s, e, j, count) = setup(false);
    e.start(j.id.clone()).unwrap();
    wait(&s, &j.id, JobState::WaitingUser).await;
    e.cancel(&j.id).unwrap();
    wait(&s, &j.id, JobState::Cancelled).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn adapter_preparation_timeout_fails_before_submit() {
    let (_dir, store, engine, job, submissions) =
        setup_with_stalled_budget(false, Some(Duration::from_millis(25)));
    engine.start(job.id.clone()).unwrap();
    wait(&store, &job.id, JobState::Failed).await;
    let failed = store.job(&job.id).unwrap();
    assert!(failed.message.contains("准备已超时"));
    assert!(!failed.ready_to_submit);
    assert_eq!(submissions.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn queued_job_waits_for_first_account_and_can_be_cancelled() {
    let (_dir, s, e, j, _) = setup(false);
    let second = s
        .enqueue("second", std::slice::from_ref(&j.account), &j.content)
        .unwrap()
        .remove(0);
    e.start(j.id.clone()).unwrap();
    wait(&s, &j.id, JobState::WaitingUser).await;
    e.start(second.id.clone()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(s.job(&second.id).unwrap().state, JobState::Queued);
    e.cancel(&second.id).unwrap();
    wait(&s, &second.id, JobState::Cancelled).await;
    e.cancel(&j.id).unwrap();
    wait(&s, &j.id, JobState::Cancelled).await;
}
