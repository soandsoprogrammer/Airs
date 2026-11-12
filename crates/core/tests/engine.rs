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
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;
struct AuditAdapter {
    submits: Arc<AtomicUsize>,
    unknown: bool,
    stalled_budget: Option<Duration>,
    rendezvous: Option<Arc<Barrier>>,
}
struct AuditPrepared {
    ctx: RunContext,
    submits: Arc<AtomicUsize>,
    unknown: bool,
    rendezvous: Option<Arc<Barrier>>,
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
        if let Some(barrier) = &self.rendezvous {
            barrier.wait().await;
        }
        Ok(Box::new(AuditPrepared {
            ctx,
            submits: self.submits.clone(),
            unknown: self.unknown,
            rendezvous: self.rendezvous.clone(),
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
        if let Some(barrier) = &self.rendezvous {
            barrier.wait().await;
        }
        if self.unknown {
            Ok(None)
        } else {
            Ok(Some(Receipt {
                platform_id: format!("fixture-{}", self.ctx.job.id),
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
    setup_for_platform(Platform::Simulation, unknown, stalled_budget, None)
}
fn setup_for_platform(
    platform: Platform,
    unknown: bool,
    stalled_budget: Option<Duration>,
    rendezvous: Option<Arc<Barrier>>,
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
        rendezvous,
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
    let (_dir, store, engine, job, submissions) =
        setup_for_platform(Platform::Douyin, false, None, None);
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
async fn same_account_queued_job_can_be_cancelled_while_first_waits() {
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
    assert_eq!(s.job(&j.id).unwrap().state, JobState::WaitingUser);
    assert!(
        s.events(&second.id)
            .unwrap()
            .iter()
            .all(|event| !event.state.active())
    );
    e.cancel(&j.id).unwrap();
    wait(&s, &j.id, JobState::Cancelled).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_accounts_on_one_platform_prepare_and_submit_concurrently() {
    let rendezvous = Arc::new(Barrier::new(3));
    let (_dir, store, engine, first, submissions) =
        setup_for_platform(Platform::Simulation, false, None, Some(rendezvous.clone()));
    let other_account = store
        .add_account("另一个本地测试账号", first.account.platform)
        .unwrap();
    let second = store
        .enqueue("other-account", &[other_account], &first.content)
        .unwrap()
        .remove(0);

    for job in [&first, &second] {
        engine.start(job.id.clone()).unwrap();
    }
    for job in [&first, &second] {
        wait(&store, &job.id, JobState::Uploading).await;
    }
    tokio::time::timeout(Duration::from_secs(5), rendezvous.wait())
        .await
        .expect("both accounts should reach the preparation barrier");
    for job in [&first, &second] {
        wait(&store, &job.id, JobState::WaitingUser).await;
    }
    assert_eq!(submissions.load(Ordering::SeqCst), 0);

    for job in [&first, &second] {
        engine.submit(&job.id).unwrap();
    }
    for job in [&first, &second] {
        wait(&store, &job.id, JobState::Submitting).await;
    }
    tokio::time::timeout(Duration::from_secs(5), rendezvous.wait())
        .await
        .expect("both accounts should reach the submission barrier");
    assert_eq!(submissions.load(Ordering::SeqCst), 2);
    for job in [&first, &second] {
        wait(&store, &job.id, JobState::Submitted).await;
        let receipt = store.job(&job.id).unwrap().result.unwrap();
        assert_eq!(receipt.platform_id, format!("fixture-{}", job.id));
    }
}

#[tokio::test]
async fn same_account_queue_advances_in_start_order() {
    let (_dir, store, engine, first, _) = setup(false);
    let account = std::slice::from_ref(&first.account);
    let second = store
        .enqueue("second", account, &first.content)
        .unwrap()
        .remove(0);
    let third = store
        .enqueue("third", account, &first.content)
        .unwrap()
        .remove(0);
    engine.start(first.id.clone()).unwrap();
    wait(&store, &first.id, JobState::WaitingUser).await;
    engine.start(second.id.clone()).unwrap();
    engine.start(third.id.clone()).unwrap();
    engine.submit(&first.id).unwrap();

    wait(&store, &second.id, JobState::WaitingUser).await;
    assert_eq!(store.job(&first.id).unwrap().state, JobState::Submitted);
    assert_eq!(store.job(&third.id).unwrap().state, JobState::Queued);

    engine.cancel(&second.id).unwrap();
    wait(&store, &third.id, JobState::WaitingUser).await;
    assert_eq!(store.job(&second.id).unwrap().state, JobState::Cancelled);
    engine.cancel(&third.id).unwrap();
    wait(&store, &third.id, JobState::Cancelled).await;
}

#[tokio::test]
async fn busy_account_queue_leaves_slots_for_other_accounts_up_to_the_limit() {
    let (_dir, store, engine, first, _) = setup(false);
    let waiting_for_first = ["second", "third"].map(|key| {
        store
            .enqueue(key, std::slice::from_ref(&first.account), &first.content)
            .unwrap()
            .remove(0)
    });
    let other_accounts: Vec<_> = (0..MAX_CONCURRENT_JOBS)
        .map(|index| {
            store
                .add_account(&format!("测试账号 {index}"), first.account.platform)
                .unwrap()
        })
        .collect();
    let other_jobs = store
        .enqueue("other-accounts", &other_accounts, &first.content)
        .unwrap();
    let (last, within_limit) = other_jobs.split_last().unwrap();

    engine.start(first.id.clone()).unwrap();
    wait(&store, &first.id, JobState::WaitingUser).await;
    for job in waiting_for_first.iter().chain(&other_jobs) {
        engine.start(job.id.clone()).unwrap();
    }
    for job in within_limit {
        wait(&store, &job.id, JobState::WaitingUser).await;
    }
    for job in waiting_for_first.iter().chain(std::iter::once(last)) {
        assert_eq!(store.job(&job.id).unwrap().state, JobState::Queued);
    }

    engine.cancel(&within_limit[0].id).unwrap();
    wait(&store, &last.id, JobState::WaitingUser).await;
    assert_eq!(
        store.job(&within_limit[0].id).unwrap().state,
        JobState::Cancelled
    );
    assert_eq!(store.job(&first.id).unwrap().state, JobState::WaitingUser);
    for job in &waiting_for_first {
        assert_eq!(store.job(&job.id).unwrap().state, JobState::Queued);
    }

    engine.cancel_all();
    for job in std::iter::once(&first)
        .chain(&waiting_for_first)
        .chain(&other_jobs)
    {
        wait(&store, &job.id, JobState::Cancelled).await;
    }
    assert_eq!(engine.active_count(), 0);
}
