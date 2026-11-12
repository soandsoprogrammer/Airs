use crate::*;
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

enum SubmissionEvidence {
    None,
    Receipt(Receipt),
    UserConfirmedNotPublished,
}

pub struct ProfileLease {
    _file: File,
    pub path: PathBuf,
}
impl Drop for ProfileLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}
impl Store {
    pub fn default_root() -> Result<PathBuf> {
        Ok(
            directories::ProjectDirs::from("com", "RustSocialPublisher", "SocialPublisher")
                .ok_or_else(|| anyhow::anyhow!("无法定位用户数据目录"))?
                .data_local_dir()
                .into(),
        )
    }
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.as_ref(), fs::Permissions::from_mode(0o700))?;
        }
        let s = Self {
            root: root.as_ref().into(),
        };
        let connection = s.conn()?;
        connection.execute_batch(include_str!("../../../migrations/001_initial.sql"))?;
        connection.execute_batch(include_str!("../../../migrations/002_removed_accounts.sql"))?;
        connection.execute_batch(include_str!("../../../migrations/003_removed_jobs.sql"))?;
        Ok(s)
    }
    fn conn(&self) -> Result<Connection> {
        let c = Connection::open(self.root.join("publisher.sqlite3"))?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(c)
    }
    pub fn lease(&self, key: &str) -> Result<ProfileLease> {
        let lease = self.lock_profile(key)?;
        let removed: bool = self.conn()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM removed_accounts WHERE account_id=?1)",
            [key],
            |row| row.get(0),
        )?;
        ensure!(!removed, "账号已移除");
        fs::create_dir_all(&lease.path)?;
        Ok(lease)
    }
    fn lock_profile(&self, key: &str) -> Result<ProfileLease> {
        ensure!(
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "无效账号标识"
        );
        let base = self.root.join("profiles");
        fs::create_dir_all(&base)?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(base.join(format!("{key}.lock")))?;
        f.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("该账号正在登录或执行任务，请等待完成"))?;
        let path = base.join(key);
        Ok(ProfileLease { _file: f, path })
    }
    pub fn add_account(&self, alias: &str, platform: Platform) -> Result<Account> {
        let customized = !alias.trim().is_empty();
        let default_name = match platform {
            Platform::Douyin => "抖音账号",
            Platform::Kuaishou => "快手账号",
            Platform::Xiaohongshu => "小红书账号",
            Platform::Simulation => "模拟测试账号",
        };
        // Removed accounts still count so an existing default name isn't reused.
        let all_accounts: Vec<Account> = self.list_json("SELECT data FROM accounts")?;
        let number = all_accounts
            .iter()
            .filter(|a| a.platform == platform)
            .count()
            + 1;
        let a = Account {
            id: uuid::Uuid::new_v4().to_string(),
            alias: if customized {
                alias.trim().into()
            } else {
                format!("{default_name} {number}")
            },
            platform,
            session: SessionStatus::Unchecked,
            checked_at: None,
            nickname: None,
            alias_customized: customized,
        };
        self.save_account(&a)?;
        Ok(a)
    }
    pub fn save_account(&self, a: &Account) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO accounts VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![a.id, serde_json::to_string(a)?],
        )?;
        Ok(())
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.list_json(
            "SELECT data FROM accounts WHERE NOT EXISTS (
                SELECT 1 FROM removed_accounts WHERE account_id=accounts.id
            ) ORDER BY rowid",
        )
    }
    pub fn remove_account(&self, id: &str) -> Result<()> {
        let mut connection = self.conn()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        ensure!(exists, "账号不存在");

        let lease = self.lock_profile(id)?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let has_pending_jobs: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE account_id=?1
                AND state IN ('Queued','Preparing','Uploading','WaitingUser','Submitting'))",
            [id],
            |row| row.get(0),
        )?;
        ensure!(!has_pending_jobs, "请先停止该账号的排队或进行中任务");
        tx.execute(
            "INSERT OR IGNORE INTO removed_accounts(account_id) VALUES(?1)",
            [id],
        )?;
        tx.commit()?;

        // Keep the lock file itself so concurrent users keep locking the same inode.
        match fs::remove_dir_all(&lease.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("账号已从列表移除，但无法清除本地登录数据"),
        }
    }
    fn ensure_live_account(connection: &Connection, id: &str) -> Result<()> {
        let live: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND NOT EXISTS (
                SELECT 1 FROM removed_accounts WHERE account_id=accounts.id
            ))",
            [id],
            |row| row.get(0),
        )?;
        ensure!(live, "账号已移除或不存在");
        Ok(())
    }
    pub fn rename_account(&self, id: &str, alias: &str) -> Result<()> {
        ensure!(!alias.trim().is_empty(), "请填写显示名称");
        self.update_account(id, |a| {
            a.alias = alias.trim().into();
            a.alias_customized = true;
        })
    }
    pub fn sync_session(
        &self,
        id: &str,
        status: SessionStatus,
        nickname: Option<String>,
    ) -> Result<()> {
        self.update_account(id, |a| {
            a.session = status;
            a.checked_at = Some(now());
            if let Some(name) = nickname.filter(|s| !s.trim().is_empty()) {
                if !a.alias_customized {
                    a.alias = name.clone();
                }
                a.nickname = Some(name);
            }
        })
    }
    fn update_account(&self, id: &str, update: impl FnOnce(&mut Account)) -> Result<()> {
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::ensure_live_account(&tx, id)?;
        let raw: String =
            tx.query_row("SELECT data FROM accounts WHERE id=?1", [id], |r| r.get(0))?;
        let mut account: Account = serde_json::from_str(&raw)?;
        update(&mut account);
        tx.execute(
            "UPDATE accounts SET data=?1 WHERE id=?2",
            params![serde_json::to_string(&account)?, id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn add_asset(&self, path: &Path) -> Result<Asset> {
        self.insert_asset(Asset::inspect(path)?)
    }
    pub fn add_image_asset(&self, paths: &[std::path::PathBuf]) -> Result<Asset> {
        self.insert_asset(Asset::inspect_images(paths)?)
    }
    fn insert_asset(&self, a: Asset) -> Result<Asset> {
        self.conn()?.execute(
            "INSERT INTO assets VALUES(?1,?2)",
            params![a.id, serde_json::to_string(&a)?],
        )?;
        Ok(a)
    }
    pub fn assets(&self) -> Result<Vec<Asset>> {
        self.list_json("SELECT data FROM assets ORDER BY rowid DESC")
    }
    fn list_json<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let c = self.conn()?;
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn save_draft(&self, id: &str, content: &Content) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO drafts VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![id, serde_json::to_string(content)?],
        )?;
        Ok(())
    }
    pub fn draft(&self, id: &str) -> Result<Option<Content>> {
        let raw: Option<String> = self
            .conn()?
            .query_row("SELECT data FROM drafts WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        raw.map(|r| Ok(serde_json::from_str(&r)?)).transpose()
    }
    pub fn enqueue(
        &self,
        request_key: &str,
        accounts: &[Account],
        content: &Content,
    ) -> Result<Vec<Job>> {
        content.validate()?;
        ensure!(!accounts.is_empty(), "请选择至少一个账号");
        for account in accounts {
            content.validate_for_platform(account.platform)?;
        }
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM batches WHERE request_key=?1",
                [request_key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let mut st = tx.prepare(
                "SELECT data FROM jobs WHERE batch_id=?1 AND NOT EXISTS (
                    SELECT 1 FROM removed_jobs WHERE job_id=jobs.id
                )",
            )?;
            return st
                .query_map([id], |r| r.get::<_, String>(0))?
                .map(|r| Ok(serde_json::from_str(&r?)?))
                .collect();
        }
        let batch_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO batches VALUES(?1,?2,?3)",
            params![batch_id, request_key, now()],
        )?;
        let mut jobs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for a in accounts {
            if !seen.insert(&a.id) {
                continue;
            }
            Self::ensure_live_account(&tx, &a.id)?;
            let mut content = content.clone();
            if a.platform != Platform::Douyin {
                content.douyin_video_covers = DouyinVideoCovers::default();
            }
            let j = Job {
                id: uuid::Uuid::new_v4().to_string(),
                batch_id: batch_id.clone(),
                account: a.clone(),
                content,
                state: JobState::Queued,
                message: "等待准备；最终提交需要单独操作".into(),
                created_at: now(),
                result: None,
                ready_to_submit: false,
            };
            tx.execute(
                "INSERT INTO jobs VALUES(?1,?2,?3,?4,?5)",
                params![j.id, j.batch_id, a.id, "Queued", serde_json::to_string(&j)?],
            )?;
            jobs.push(j);
        }
        tx.commit()?;
        Ok(jobs)
    }
    pub fn jobs(&self) -> Result<Vec<Job>> {
        self.list_json(
            "SELECT data FROM jobs WHERE NOT EXISTS (
                SELECT 1 FROM removed_jobs WHERE job_id=jobs.id
            ) ORDER BY rowid DESC",
        )
    }
    pub fn job(&self, id: &str) -> Result<Job> {
        Self::live_job(&self.conn()?, id)
    }
    fn live_job(connection: &Connection, id: &str) -> Result<Job> {
        let raw: String = connection
            .query_row(
                "SELECT data FROM jobs WHERE id=?1 AND NOT EXISTS (
                    SELECT 1 FROM removed_jobs WHERE job_id=jobs.id
                )",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .context("任务已删除或不存在")?;
        Ok(serde_json::from_str(&raw)?)
    }
    pub fn remove_job(&self, id: &str) -> Result<()> {
        let mut connection = self.conn()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: String = tx
            .query_row("SELECT data FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .context("任务不存在")?;
        let job: Job = serde_json::from_str(&raw)?;
        ensure!(
            job.state.can_delete(),
            "只有失败、已停止或平台已接收的任务可以删除"
        );
        // Keep the batch key and any submission evidence for repeat requests.
        tx.execute(
            "INSERT OR IGNORE INTO removed_jobs(job_id) VALUES(?1)",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn claim(&self, id: &str) -> Result<Job> {
        self.change(
            id,
            JobState::Preparing,
            "校验素材并打开独立浏览器",
            SubmissionEvidence::None,
            true,
            false,
        )
    }
    pub fn transition(&self, id: &str, state: JobState, message: &str) -> Result<Job> {
        self.change(id, state, message, SubmissionEvidence::None, false, false)
    }
    pub fn prepared(&self, id: &str) -> Result<Job> {
        self.change(
            id,
            JobState::WaitingUser,
            "准备完成，等待确认发布",
            SubmissionEvidence::None,
            false,
            true,
        )
    }
    pub fn received(&self, id: &str, receipt: Receipt) -> Result<Job> {
        self.change(
            id,
            JobState::Submitted,
            "平台已接收（不代表已公开发布）",
            SubmissionEvidence::Receipt(receipt),
            false,
            false,
        )
    }
    /// Records the user's confirmation after they checked the platform's work history.
    pub fn confirm_not_published(&self, id: &str) -> Result<Job> {
        self.change(
            id,
            JobState::Failed,
            "已核对平台作品管理并确认未发布，可重新准备",
            SubmissionEvidence::UserConfirmedNotPublished,
            false,
            false,
        )
    }
    fn change(
        &self,
        id: &str,
        state: JobState,
        message: &str,
        submission_evidence: SubmissionEvidence,
        claim: bool,
        ready: bool,
    ) -> Result<Job> {
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut job = Self::live_job(&tx, id)?;
        let reconciled = job.state == JobState::Unknown;
        let confirmed_not_published = matches!(
            submission_evidence,
            SubmissionEvidence::UserConfirmedNotPublished
        );
        if confirmed_not_published {
            ensure!(reconciled, "只有结果待确认的任务可以确认未发布");
        } else {
            job.state.check(state)?;
        }
        let message = if confirmed_not_published && job.account.platform == Platform::Douyin {
            "已核对抖音作品管理并确认未发布，可重新准备"
        } else if reconciled && state == JobState::Submitted {
            "已核对平台回执，确认作品已被接收"
        } else {
            message
        };
        if matches!(state, JobState::Queued | JobState::Preparing) {
            Self::ensure_live_account(&tx, &job.account.id)?;
        }
        if state == JobState::Submitting {
            ensure!(job.ready_to_submit, "必须先完成发布准备");
        }
        if state == JobState::Submitted {
            // Some successful platform responses omit the public work identifier.
            let SubmissionEvidence::Receipt(receipt) = &submission_evidence else {
                bail!("平台接收结果缺少依据");
            };
            let idless_acceptance = receipt.url.is_none()
                && match job.account.platform {
                    Platform::Kuaishou => true,
                    Platform::Xiaohongshu => receipt.evidence == XIAOHONGSHU_ACCEPTED_EVIDENCE,
                    _ => false,
                };
            ensure!(
                !receipt.evidence.trim().is_empty()
                    && (!receipt.platform_id.trim().is_empty() || idless_acceptance),
                "平台接收结果缺少依据"
            );
        }
        job.state = state;
        job.ready_to_submit = ready;
        job.message = message.into();
        match submission_evidence {
            SubmissionEvidence::Receipt(receipt) => job.result = Some(receipt),
            SubmissionEvidence::UserConfirmedNotPublished => job.result = None,
            SubmissionEvidence::None => {}
        }
        tx.execute(
            "UPDATE jobs SET state=?1,data=?2 WHERE id=?3",
            params![format!("{state:?}"), serde_json::to_string(&job)?, id],
        )?;
        if claim {
            tx.execute(
                "INSERT INTO attempts(job_id,started_at) VALUES(?1,?2)",
                params![id, now()],
            )?;
        }
        if state == JobState::Submitting {
            ensure!(
                tx.execute(
                    "UPDATE attempts SET submitting_at=?1 WHERE job_id=?2 AND finished_at IS NULL",
                    params![now(), id]
                )? == 1,
                "任务执行记录缺失"
            );
        }
        if matches!(
            state,
            JobState::Submitted | JobState::Failed | JobState::Cancelled | JobState::Unknown
        ) {
            let outcome = format!("{state:?}");
            let evidence = if confirmed_not_published {
                Some("用户确认已核对平台作品管理，未发现已发布作品")
            } else {
                job.result.as_ref().map(|receipt| receipt.evidence.as_str())
            };
            if reconciled {
                ensure!(
                    tx.execute(
                        "UPDATE attempts SET outcome=?1,evidence=?2
                         WHERE id=(SELECT id FROM attempts WHERE job_id=?3 ORDER BY id DESC LIMIT 1)
                           AND outcome='Unknown' AND submitting_at IS NOT NULL AND finished_at IS NOT NULL",
                        params![outcome, evidence, id],
                    )? == 1,
                    "待确认任务缺少可核对的提交记录"
                );
            } else {
                tx.execute(
                    "UPDATE attempts SET finished_at=?1,outcome=?2,evidence=?3
                     WHERE job_id=?4 AND finished_at IS NULL",
                    params![now(), outcome, evidence, id],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO events(job_id,data) VALUES(?1,?2)",
            params![
                id,
                serde_json::to_string(&JobEvent {
                    time: now(),
                    state,
                    message: message.into()
                })?
            ],
        )?;
        tx.commit()?;
        Ok(job)
    }
    pub fn events(&self, id: &str) -> Result<Vec<JobEvent>> {
        let c = self.conn()?;
        let mut st = c.prepare("SELECT data FROM events WHERE job_id=?1 ORDER BY id")?;
        st.query_map([id], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn recover(&self) -> Result<usize> {
        let jobs = self.jobs()?;
        let mut count = 0;
        for j in jobs {
            if j.state.active() {
                let next = if j.state == JobState::Submitting {
                    JobState::Unknown
                } else {
                    JobState::Failed
                };
                self.transition(
                    &j.id,
                    next,
                    if next == JobState::Unknown {
                        "提交时应用中断，请核对平台记录；不会自动重发"
                    } else {
                        "准备时应用中断；未自动重新执行"
                    },
                )?;
                count += 1;
            }
        }
        Ok(count)
    }
}
