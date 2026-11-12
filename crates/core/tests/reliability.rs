use publisher_core::*;
fn setup() -> (tempfile::TempDir, Store, Account, Content) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.add_account("主号", Platform::Douyin).unwrap();
    let video = dir.path().join("test.mp4");
    std::fs::write(&video, b"fixture: not a real video").unwrap();
    let content = Content {
        asset: store.add_asset(&video).unwrap(),
        title: "测试标题".into(),
        description: "说明".into(),
        tags: vec!["测试".into()],
        douyin_video_covers: DouyinVideoCovers::default(),
    };
    (dir, store, a, content)
}

#[test]
fn douyin_covers_are_optional_and_only_persist_on_douyin_video_jobs() {
    let (dir, store, douyin, mut content) = setup();
    let kuaishou = store.add_account("", Platform::Kuaishou).unwrap();
    let old_json = serde_json::to_value(&content).unwrap();
    assert!(old_json.get("douyin_video_covers").is_none());
    assert_eq!(
        serde_json::from_value::<Content>(old_json).unwrap(),
        content
    );

    let portrait = dir.path().join("portrait.png");
    let landscape = dir.path().join("landscape.jpg");
    std::fs::write(&portrait, b"portrait cover").unwrap();
    std::fs::write(&landscape, b"landscape cover").unwrap();
    content.douyin_video_covers = DouyinVideoCovers {
        portrait: Some(ImageFile::inspect(&portrait).unwrap()),
        landscape: Some(ImageFile::inspect(&landscape).unwrap()),
    };
    store.save_draft("cover-draft", &content).unwrap();
    assert_eq!(store.draft("cover-draft").unwrap(), Some(content.clone()));
    let jobs = store
        .enqueue("covers", &[douyin.clone(), kuaishou.clone()], &content)
        .unwrap();
    assert_eq!(store.job(&jobs[0].id).unwrap().content, content);
    assert!(
        store
            .job(&jobs[1].id)
            .unwrap()
            .content
            .douyin_video_covers
            .is_empty()
    );

    let mut image_post = content.clone();
    image_post.asset = store
        .add_image_asset(std::slice::from_ref(&portrait))
        .unwrap();
    assert!(image_post.validate_for_platform(Platform::Douyin).is_err());
    std::fs::write(&landscape, b"replaced cover with a different file").unwrap();
    let error = store
        .enqueue("changed-cover", &[douyin], &content)
        .unwrap_err();
    assert!(error.to_string().contains("抖音横版封面"));
    assert!(
        store
            .enqueue("other-platform", &[kuaishou], &content)
            .is_ok()
    );
}
#[test]
fn multiple_accounts_same_site_are_isolated_and_persisted() {
    let (dir, s, a, _) = setup();
    let b = s.add_account("小号", Platform::Douyin).unwrap();
    assert_ne!(a.id, b.id);
    let la = s.lease(&a.id).unwrap();
    let lb = s.lease(&b.id).unwrap();
    assert_ne!(la.path, lb.path);
    assert!(s.lease(&a.id).is_err());
    drop(la);
    assert!(s.lease(&a.id).is_ok());
    assert_eq!(
        Store::open(dir.path()).unwrap().accounts().unwrap().len(),
        2
    );
}
#[test]
fn double_click_creates_one_batch_and_one_job_per_account() {
    let (_dir, s, a, c) = setup();
    let b = s.add_account("小号", Platform::Douyin).unwrap();
    let jobs = s
        .enqueue("click-1", &[a.clone(), b.clone(), a.clone()], &c)
        .unwrap();
    assert_eq!(jobs.len(), 2);
    let same = s.enqueue("click-1", &[a, b], &c).unwrap();
    assert_eq!(same.len(), 2);
    assert_eq!(s.jobs().unwrap().len(), 2);
}
#[test]
fn deleted_tasks_stay_hidden_and_keep_batch_deduplication_and_receipts() {
    let (dir, s, a, c) = setup();
    let b = s.add_account("小号", Platform::Douyin).unwrap();
    let accounts = [a, b];
    let jobs = s.enqueue("original", &accounts, &c).unwrap();
    let [published, sibling] = jobs.as_slice() else {
        panic!("expected two jobs in the batch");
    };
    s.claim(&published.id).unwrap();
    s.prepared(&published.id).unwrap();
    s.transition(&published.id, JobState::Submitting, "before action")
        .unwrap();
    let receipt = Receipt {
        platform_id: "fixture-verified-id".into(),
        evidence: "已核对本地夹具的提交回执".into(),
        url: None,
    };
    s.received(&published.id, receipt.clone()).unwrap();
    let history = s.events(&published.id).unwrap();
    s.remove_job(&published.id).unwrap();
    s.remove_job(&published.id).unwrap();
    assert_eq!(s.jobs().unwrap().as_slice(), std::slice::from_ref(sibling));
    assert_eq!(
        s.enqueue("original", &accounts, &c).unwrap().as_slice(),
        std::slice::from_ref(sibling)
    );
    assert!(s.job(&published.id).is_err());

    s.transition(&sibling.id, JobState::Cancelled, "排队任务已取消")
        .unwrap();
    s.remove_job(&sibling.id).unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    assert!(reopened.jobs().unwrap().is_empty());
    assert!(
        reopened
            .enqueue("original", &accounts, &c)
            .unwrap()
            .is_empty()
    );
    assert!(
        reopened
            .transition(&sibling.id, JobState::Queued, "retry")
            .is_err()
    );
    assert!(reopened.claim(&sibling.id).is_err());
    assert_eq!(reopened.events(&published.id).unwrap(), history);

    let connection = rusqlite::Connection::open(dir.path().join("publisher.sqlite3")).unwrap();
    let saved: String = connection
        .query_row(
            "SELECT data FROM jobs WHERE id=?1",
            [&published.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Job>(&saved).unwrap().result,
        Some(receipt)
    );
}
#[test]
fn queued_active_and_unresolved_tasks_cannot_be_deleted() {
    let (_dir, s, a, c) = setup();
    let job = s.enqueue("pending", &[a], &c).unwrap().remove(0);
    assert!(s.remove_job(&job.id).is_err());
    s.claim(&job.id).unwrap();
    assert!(s.remove_job(&job.id).is_err());
    s.transition(&job.id, JobState::Uploading, "上传中")
        .unwrap();
    assert!(s.remove_job(&job.id).is_err());
    s.prepared(&job.id).unwrap();
    assert!(s.remove_job(&job.id).is_err());
    s.transition(&job.id, JobState::Submitting, "before action")
        .unwrap();
    assert!(s.remove_job(&job.id).is_err());
    s.recover().unwrap();
    assert_eq!(s.job(&job.id).unwrap().state, JobState::Unknown);
    assert!(s.remove_job(&job.id).is_err());

    s.confirm_not_published(&job.id).unwrap();
    assert_eq!(s.job(&job.id).unwrap().state, JobState::Failed);
    s.remove_job(&job.id).unwrap();
    assert!(s.jobs().unwrap().is_empty());
}
#[test]
fn interrupted_submission_is_unknown_and_cannot_be_requeued() {
    let (dir, s, a, c) = setup();
    let j = s.enqueue("one", &[a], &c).unwrap().remove(0);
    s.claim(&j.id).unwrap();
    s.prepared(&j.id).unwrap();
    s.transition(&j.id, JobState::Submitting, "before action")
        .unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.recover().unwrap(), 1);
    assert_eq!(reopened.job(&j.id).unwrap().state, JobState::Unknown);
    assert!(
        reopened
            .transition(&j.id, JobState::Queued, "retry")
            .is_err()
    );
    assert_eq!(reopened.recover().unwrap(), 0);
    assert!(
        reopened
            .transition(&j.id, JobState::Submitted, "missing receipt")
            .is_err()
    );
    let receipt = Receipt {
        platform_id: "fixture-verified-id".into(),
        evidence: "已核对本地夹具的提交回执".into(),
        url: None,
    };
    let resolved = reopened.received(&j.id, receipt.clone()).unwrap();
    assert_eq!(resolved.state, JobState::Submitted);
    assert_eq!(resolved.result, Some(receipt));
    assert!(
        reopened
            .events(&j.id)
            .unwrap()
            .iter()
            .any(|event| event.state == JobState::Unknown)
    );
}

#[test]
fn recording_idless_acceptance_requires_platform_evidence() {
    let (_dir, store, douyin, content) = setup();
    let kuaishou = store.add_account("快手", Platform::Kuaishou).unwrap();
    let xiaohongshu = store.add_account("小红书", Platform::Xiaohongshu).unwrap();
    let wechat_channels = store
        .add_account("视频号", Platform::WechatChannels)
        .unwrap();
    let jobs = store
        .enqueue(
            "platform-acknowledgements",
            &[douyin, kuaishou, xiaohongshu, wechat_channels],
            &content,
        )
        .unwrap();
    for job in &jobs {
        store.claim(&job.id).unwrap();
        store.prepared(&job.id).unwrap();
        store
            .transition(&job.id, JobState::Submitting, "确认发布")
            .unwrap();
    }
    let acknowledgement = Receipt {
        platform_id: String::new(),
        evidence: "快手发布接口确认已接收".into(),
        url: None,
    };
    assert!(
        store
            .received(&jobs[0].id, acknowledgement.clone())
            .is_err()
    );
    let accepted = store
        .received(&jobs[1].id, acknowledgement.clone())
        .unwrap();
    assert_eq!(accepted.state, JobState::Submitted);
    assert_eq!(accepted.result, Some(acknowledgement.clone()));
    assert!(store.received(&jobs[2].id, acknowledgement).is_err());
    let xhs_acknowledgement = Receipt {
        platform_id: String::new(),
        evidence: XIAOHONGSHU_ACCEPTED_EVIDENCE.into(),
        url: None,
    };
    assert!(
        store
            .received(&jobs[3].id, xhs_acknowledgement.clone())
            .is_err()
    );
    let accepted = store
        .received(&jobs[2].id, xhs_acknowledgement.clone())
        .unwrap();
    assert_eq!(accepted.state, JobState::Submitted);
    assert_eq!(accepted.result, Some(xhs_acknowledgement));
    let wechat_acknowledgement = Receipt {
        platform_id: String::new(),
        evidence: WECHAT_CHANNELS_ACCEPTED_EVIDENCE.into(),
        url: None,
    };
    assert!(
        store
            .received(&jobs[0].id, wechat_acknowledgement.clone())
            .is_err()
    );
    assert!(
        store
            .received(
                &jobs[3].id,
                Receipt {
                    url: Some("https://channels.weixin.qq.com/platform/post/list".into()),
                    ..wechat_acknowledgement.clone()
                }
            )
            .is_err()
    );
    let accepted = store
        .received(&jobs[3].id, wechat_acknowledgement.clone())
        .unwrap();
    assert_eq!(accepted.state, JobState::Submitted);
    assert_eq!(accepted.result, Some(wechat_acknowledgement));
}
#[test]
fn confirming_no_publish_records_evidence_before_retry_is_allowed() {
    let (dir, s, a, c) = setup();
    let job = s
        .enqueue("manual-confirmation", &[a], &c)
        .unwrap()
        .remove(0);
    assert!(s.confirm_not_published(&job.id).is_err());
    s.claim(&job.id).unwrap();
    s.prepared(&job.id).unwrap();
    s.transition(&job.id, JobState::Submitting, "before action")
        .unwrap();
    assert!(s.confirm_not_published(&job.id).is_err());
    s.recover().unwrap();
    let unknown = s.events(&job.id).unwrap();
    assert_eq!(unknown.last().unwrap().state, JobState::Unknown);
    assert!(s.transition(&job.id, JobState::Failed, "retry").is_err());
    assert!(s.transition(&job.id, JobState::Queued, "retry").is_err());

    let connection = rusqlite::Connection::open(dir.path().join("publisher.sqlite3")).unwrap();
    let read_attempt = || {
        connection
            .query_row(
                "SELECT id,started_at,submitting_at,finished_at,outcome,evidence
                 FROM attempts WHERE job_id=?1 ORDER BY id LIMIT 1",
                [&job.id],
                |row| {
                    Ok((
                        (
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ),
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .unwrap()
    };
    let before = read_attempt();
    assert_eq!(before.1, "Unknown");
    assert_eq!(before.2, None);

    let resolved = s.confirm_not_published(&job.id).unwrap();
    assert_eq!(resolved.state, JobState::Failed);
    assert_eq!(
        resolved.message,
        "已核对抖音作品管理并确认未发布，可重新准备"
    );
    assert!(resolved.result.is_none());
    assert!(!resolved.ready_to_submit);
    assert_eq!(s.job(&job.id).unwrap(), resolved);
    assert!(s.confirm_not_published(&job.id).is_err());
    let events = s.events(&job.id).unwrap();
    assert_eq!(&events[..unknown.len()], &unknown);
    assert_eq!(events.len(), unknown.len() + 1);
    assert_eq!(events.last().unwrap().state, JobState::Failed);
    assert_eq!(events.last().unwrap().message, resolved.message);
    let after = read_attempt();
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, "Failed");
    assert_eq!(
        after.2.as_deref(),
        Some("用户确认已核对平台作品管理，未发现已发布作品")
    );

    s.transition(&job.id, JobState::Queued, "retry").unwrap();
    s.claim(&job.id).unwrap();
    assert_eq!(read_attempt(), after);
}
#[test]
fn same_account_cannot_have_two_active_jobs() {
    let (_dir, s, a, c) = setup();
    let j1 = s
        .enqueue("a", std::slice::from_ref(&a), &c)
        .unwrap()
        .remove(0);
    let j2 = s.enqueue("b", &[a], &c).unwrap().remove(0);
    s.claim(&j1.id).unwrap();
    assert!(s.claim(&j2.id).is_err());
    assert_eq!(s.job(&j2.id).unwrap().state, JobState::Queued);
    s.transition(&j1.id, JobState::Cancelled, "cancel").unwrap();
    s.claim(&j2.id).unwrap();
}
#[test]
fn interrupted_preparation_is_failed_queued_jobs_are_not_restarted() {
    let (_dir, s, a, c) = setup();
    let j = s
        .enqueue("a", std::slice::from_ref(&a), &c)
        .unwrap()
        .remove(0);
    let queued = s.enqueue("b", &[a], &c).unwrap().remove(0);
    s.claim(&j.id).unwrap();
    s.recover().unwrap();
    assert_eq!(s.job(&j.id).unwrap().state, JobState::Failed);
    assert_eq!(s.job(&queued.id).unwrap().state, JobState::Queued);
}
#[test]
fn source_changes_and_missing_files_are_rejected() {
    let (_dir, _, _, c) = setup();
    std::fs::write(&c.asset.path, b"changed").unwrap();
    assert!(c.validate().is_err());
    std::fs::remove_file(&c.asset.path).unwrap();
    assert!(c.validate().is_err());
}
#[test]
fn image_order_and_media_kind_survive_draft_and_cross_platform_batch() {
    let (dir, store, douyin, mut content) = setup();
    let kuaishou = store.add_account("", Platform::Kuaishou).unwrap();
    let xiaohongshu = store.add_account("", Platform::Xiaohongshu).unwrap();
    let wechat_channels = store.add_account("", Platform::WechatChannels).unwrap();
    assert_eq!(xiaohongshu.alias, "小红书账号 1");
    assert_eq!(wechat_channels.alias, "视频号账号 1");
    let second = dir.path().join("second.png");
    let first = dir.path().join("first.JPG");
    std::fs::write(&first, b"fixture first").unwrap();
    std::fs::write(&second, b"fixture second").unwrap();
    content.asset = store
        .add_image_asset(&[second.clone(), first.clone()])
        .unwrap();
    assert_eq!(content.asset.kind, MediaKind::Images);
    assert_eq!(content.asset.images[0].path, second.canonicalize().unwrap());
    assert_eq!(content.asset.images[1].path, first.canonicalize().unwrap());
    assert!(
        store
            .add_image_asset(&[first.clone(), first.clone()])
            .is_err()
    );
    store.save_draft("photo-draft", &content).unwrap();
    assert_eq!(store.draft("photo-draft").unwrap(), Some(content.clone()));
    let error = store
        .enqueue(
            "unsupported-photos",
            &[douyin.clone(), wechat_channels],
            &content,
        )
        .unwrap_err();
    assert!(error.to_string().contains("视频号图文发布尚未接入"));
    assert!(store.jobs().unwrap().is_empty());
    let jobs = store
        .enqueue("photos", &[douyin, kuaishou, xiaohongshu], &content)
        .unwrap();
    assert_eq!(jobs.len(), 3);
    for job in jobs {
        assert_eq!(store.job(&job.id).unwrap().content, content);
    }
    std::fs::remove_file(&first).unwrap();
    assert!(
        content
            .asset
            .verify()
            .unwrap_err()
            .to_string()
            .contains("first.JPG")
    );
}

#[test]
fn old_video_assets_and_platform_specific_title_limits_are_preserved() {
    let (_dir, store, douyin, mut content) = setup();
    let xiaohongshu = store.add_account("", Platform::Xiaohongshu).unwrap();
    let mut legacy = serde_json::to_value(&content.asset).unwrap();
    legacy.as_object_mut().unwrap().remove("kind");
    assert_eq!(
        serde_json::from_value::<Asset>(legacy).unwrap(),
        content.asset
    );
    content.title = "a".repeat(40);
    assert!(
        store
            .enqueue("too-long-douyin", &[douyin], &content)
            .is_err()
    );
    assert!(
        store
            .enqueue("ascii-title", std::slice::from_ref(&xiaohongshu), &content)
            .is_ok()
    );
    content.title = "图".repeat(21);
    assert!(
        store
            .enqueue("too-long-xhs", &[xiaohongshu], &content)
            .is_err()
    );
}
#[test]
fn submission_cannot_turn_into_cancelled_or_published_without_evidence() {
    assert!(!JobState::Submitting.can_transition(JobState::Cancelled));
    assert!(!JobState::Uploading.can_transition(JobState::Submitted));
    assert!(!JobState::Unknown.can_transition(JobState::Queued));
    assert!(!JobState::Unknown.can_transition(JobState::Failed));
    assert!(!JobState::Submitted.can_transition(JobState::Queued));
}

#[test]
fn automatic_nickname_defaults_and_custom_name_survive_session_refresh() {
    let (_dir, s, _, _) = setup();
    let a = s.add_account("", Platform::Douyin).unwrap();
    let b = s.add_account("", Platform::Douyin).unwrap();
    assert_ne!(a.alias, b.alias);
    assert!(!a.alias_customized);
    s.sync_session(&a.id, SessionStatus::Ready, Some("平台昵称".into()))
        .unwrap();
    let a = s
        .accounts()
        .unwrap()
        .into_iter()
        .find(|x| x.id == a.id)
        .unwrap();
    assert_eq!(a.alias, "平台昵称");
    s.rename_account(&a.id, "品牌主号").unwrap();
    s.sync_session(&a.id, SessionStatus::Ready, Some("新平台昵称".into()))
        .unwrap();
    let a = s
        .accounts()
        .unwrap()
        .into_iter()
        .find(|x| x.id == a.id)
        .unwrap();
    assert_eq!(a.alias, "品牌主号");
    assert_eq!(a.nickname.as_deref(), Some("新平台昵称"));
    assert!(a.alias_customized);
}

#[test]
fn account_notes_persist_without_becoming_part_of_job_history() {
    let (dir, store, account, content) = setup();
    let legacy = serde_json::to_value(&account).unwrap();
    assert!(legacy.get("note").is_none());
    assert!(
        serde_json::from_value::<Account>(legacy)
            .unwrap()
            .note
            .is_empty()
    );

    let note = "  运营账号🙂\n审核后使用\n";
    store.save_account_note(&account.id, note).unwrap();
    store.rename_account(&account.id, "品牌号").unwrap();
    store
        .sync_session(&account.id, SessionStatus::Ready, Some("平台昵称".into()))
        .unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    let saved = reopened.accounts().unwrap().remove(0);
    assert_eq!(saved.note, note);
    assert_eq!(saved.alias, "品牌号");
    assert_eq!(saved.nickname.as_deref(), Some("平台昵称"));
    assert_eq!(saved.session, SessionStatus::Ready);

    let oversized = "字".repeat(ACCOUNT_NOTE_LIMIT + 1);
    assert!(reopened.save_account_note(&saved.id, &oversized).is_err());
    assert_eq!(reopened.accounts().unwrap()[0].note, note);
    let job = reopened
        .enqueue("account-note", std::slice::from_ref(&saved), &content)
        .unwrap()
        .remove(0);
    assert!(job.account.note.is_empty());
    assert!(reopened.job(&job.id).unwrap().account.note.is_empty());

    reopened.save_account_note(&saved.id, "").unwrap();
    assert!(store.accounts().unwrap()[0].note.is_empty());
    reopened
        .transition(&job.id, JobState::Cancelled, "取消测试任务")
        .unwrap();
    reopened.remove_account(&saved.id).unwrap();
    assert!(reopened.save_account_note(&saved.id, note).is_err());
    assert!(store.accounts().unwrap().is_empty());
}

#[test]
fn kuaishou_accounts_are_numbered_and_isolated_per_platform() {
    let (dir, store, douyin, _) = setup();
    let first = store.add_account("", Platform::Kuaishou).unwrap();
    let second = store.add_account("", Platform::Kuaishou).unwrap();
    assert_eq!(first.alias, "快手账号 1");
    assert_eq!(second.alias, "快手账号 2");
    let first_profile = store.lease(&first.id).unwrap();
    let second_profile = store.lease(&second.id).unwrap();
    let douyin_profile = store.lease(&douyin.id).unwrap();
    assert_ne!(first_profile.path, second_profile.path);
    assert_ne!(first_profile.path, douyin_profile.path);
    assert!(store.lease(&first.id).is_err());

    store
        .sync_session(&first.id, SessionStatus::Ready, Some("快手测试昵称".into()))
        .unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    let accounts = reopened.accounts().unwrap();
    let first = accounts
        .iter()
        .find(|account| account.id == first.id)
        .unwrap();
    let second = accounts
        .iter()
        .find(|account| account.id == second.id)
        .unwrap();
    assert_eq!(first.platform, Platform::Kuaishou);
    assert_eq!(first.alias, "快手测试昵称");
    assert_eq!(second.session, SessionStatus::Unchecked);
}

#[test]
fn removed_account_keeps_history_and_only_clears_its_own_profile() {
    let (dir, s, a, c) = setup();
    let b = s.add_account("", Platform::Douyin).unwrap();
    let a_profile = s.lease(&a.id).unwrap().path.clone();
    let b_profile = s.lease(&b.id).unwrap().path.clone();
    std::fs::write(a_profile.join("session-fixture"), b"first account").unwrap();
    std::fs::write(b_profile.join("session-fixture"), b"second account").unwrap();
    let job = s
        .enqueue("first", std::slice::from_ref(&a), &c)
        .unwrap()
        .remove(0);
    s.transition(&job.id, JobState::Cancelled, "已取消")
        .unwrap();

    s.remove_account(&a.id).unwrap();
    let reopened = Store::open(dir.path()).unwrap();
    let remaining = reopened.accounts().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, b.id);
    assert!(!a_profile.exists());
    assert!(b_profile.join("session-fixture").is_file());
    assert!(reopened.lease(&a.id).is_err());
    assert!(!a_profile.exists());

    let historical = reopened.job(&job.id).unwrap();
    assert_eq!(historical.account.id, a.id);
    assert_eq!(historical.state, JobState::Cancelled);
    assert_eq!(reopened.events(&job.id).unwrap().len(), 1);
    assert!(reopened.rename_account(&a.id, "另一个名称").is_err());
    assert!(
        reopened
            .sync_session(&a.id, SessionStatus::Ready, None)
            .is_err()
    );
    assert!(reopened.enqueue("stale", &[a], &c).is_err());
    assert!(
        reopened
            .transition(&job.id, JobState::Queued, "重新准备")
            .is_err()
    );
    assert_eq!(reopened.jobs().unwrap().len(), 1);

    let replacement = reopened.add_account("", Platform::Douyin).unwrap();
    assert_eq!(b.alias, "抖音账号 2");
    assert_eq!(replacement.alias, "抖音账号 3");
}

#[test]
fn removal_waits_for_the_profile_lease_and_pending_jobs() {
    let (_dir, s, a, c) = setup();
    let lease = s.lease(&a.id).unwrap();
    assert!(s.remove_account(&a.id).is_err());
    assert!(lease.path.is_dir());
    drop(lease);

    let job = s
        .enqueue("pending", std::slice::from_ref(&a), &c)
        .unwrap()
        .remove(0);
    assert!(s.remove_account(&a.id).is_err());
    s.claim(&job.id).unwrap();
    assert!(s.remove_account(&a.id).is_err());
    assert_eq!(s.accounts().unwrap().len(), 1);
    assert_eq!(s.job(&job.id).unwrap().state, JobState::Preparing);

    s.transition(&job.id, JobState::Failed, "准备失败").unwrap();
    s.remove_account(&a.id).unwrap();
    assert!(s.accounts().unwrap().is_empty());
}

#[test]
fn removal_reports_if_only_local_profile_cleanup_fails() {
    let (_dir, s, a, _) = setup();
    let profile = s.lease(&a.id).unwrap().path.clone();
    std::fs::remove_dir(&profile).unwrap();
    std::fs::write(&profile, b"unexpected profile file").unwrap();

    let error = s.remove_account(&a.id).unwrap_err();
    assert_eq!(
        error.to_string(),
        "账号已从列表移除，但无法清除本地登录数据"
    );
    assert!(s.accounts().unwrap().is_empty());
    assert!(s.lease(&a.id).is_err());

    std::fs::remove_file(profile).unwrap();
    s.remove_account(&a.id).unwrap();
}
