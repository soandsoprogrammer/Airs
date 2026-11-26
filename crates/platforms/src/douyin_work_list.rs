//! Read-only confirmation for a WebKit submission whose transport receipt
//! was missed. Never clicks or retries publishing.
use crate::douyin::CREATOR_MANAGE_URL;
use crate::douyin_submit::prepared_description;
use crate::webkit::Session;
use anyhow::Result;
use publisher_core::{Content, MediaKind, Receipt};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Work {
    id: String,
    created_at: u64,
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub(crate) async fn check(
    account_id: &str,
    content: &Content,
    since: u64,
    until: u64,
) -> Result<Option<Receipt>> {
    if content.asset.kind != MediaKind::Video || since == 0 || until < since || until - since > 900
    {
        return Ok(None);
    }
    // The job already holds the account's exclusive profile lease. This
    // short-lived second view shares that same persistent WebKit store.
    let session = Session::launch(uuid::Uuid::parse_str(account_id)?, true).await?;
    let result = async {
        session.arm_douyin_work_list(&content.title, &prepared_description(content), since, until).await?;
        session.navigate(CREATOR_MANAGE_URL).await?;
        let matches = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let works: Option<Vec<Work>> = session.evaluate(
                    "typeof window.__airsDouyinWorkList === 'function' ? window.__airsDouyinWorkList() : null"
                ).await?;
                if let Some(works) = works {
                    return Ok::<_, anyhow::Error>(works);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }).await;
        let Ok(Ok(works)) = matches else { return Ok(None); };
        let [work] = works.as_slice() else { return Ok(None); };
        if work.created_at < since || work.created_at > until || !(11..=25).contains(&work.id.len())
            || !work.id.bytes().all(|byte| byte.is_ascii_digit()) || work.id.starts_with('0')
        {
            return Ok(None);
        }
        Ok(Some(Receipt {
            platform_id: work.id.clone(),
            evidence: "抖音创作者中心作品管理已核实：本次提交后同标题与文案的视频已发布（已取得作品编号）".into(),
            url: None,
        }))
    }.await;
    let _ = session.close().await;
    result
}

pub(crate) async fn wait(account_id: &str, content: &Content, since: u64) -> Option<Receipt> {
    // Give the transport receipt a chance first. Verification-only reads
    // cannot submit again, and ambiguous/no results stay Unknown.
    loop {
        tokio::time::sleep(Duration::from_secs(25)).await;
        let until = unix_now().saturating_add(5);
        if until.saturating_sub(since) > 900 {
            return None;
        }
        if let Ok(Some(receipt)) = check(account_id, content, since, until).await {
            return Some(receipt);
        }
    }
}
