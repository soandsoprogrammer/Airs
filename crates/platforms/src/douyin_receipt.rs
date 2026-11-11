use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::{
    network::{
        ConfigureDurableMessagesParams, EnableParams, EventLoadingFinished, EventRequestWillBeSent,
        EventResponseReceived, GetResponseBodyParams, GetResponseBodyReturns, LoaderId, RequestId,
    },
    page::{FrameId, GetFrameTreeParams},
    security::SecurityState,
    target::TargetId,
};
use chromiumoxide::listeners::EventStream;
use futures::StreamExt;
use publisher_core::{MediaKind, Receipt};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

const CREATOR_ORIGIN: &str = "https://creator.douyin.com/";
const MAX_RECEIPT_BYTES: usize = 256 * 1024;
const MAX_NETWORK_BUFFER_BYTES: i64 = 4 * 1024 * 1024;

/// Correlates a publish response with its POST in the prepared top-level page.
/// The observer does not click, submit, navigate, or retry any platform operation.
pub struct DouyinReceiptObserver {
    target_id: TargetId,
    frame_id: FrameId,
    loader_id: LoaderId,
    media_kind: MediaKind,
    requests: EventStream<EventRequestWillBeSent>,
    responses: EventStream<EventResponseReceived>,
    finishes: EventStream<EventLoadingFinished>,
}

struct PendingReceipt {
    operation: &'static str,
    successful_response: bool,
}

impl DouyinReceiptObserver {
    /// Arm this observer immediately before clicking the prepared page's publish button.
    pub async fn start(page: &Page) -> Result<Self> {
        let frame = page
            .execute(GetFrameTreeParams {})
            .await?
            .result
            .frame_tree
            .frame;
        let media_kind = composer_kind(&frame.url)
            .ok_or_else(|| anyhow::anyhow!("抖音发布页已变化，无法确认当前准备的作品"))?;
        let requests = page.event_listener::<EventRequestWillBeSent>().await?;
        let responses = page.event_listener::<EventResponseReceived>().await?;
        let finishes = page.event_listener::<EventLoadingFinished>().await?;
        page.execute(EnableParams {
            max_post_data_size: Some(0),
            max_total_buffer_size: Some(MAX_NETWORK_BUFFER_BYTES),
            max_resource_buffer_size: Some(MAX_RECEIPT_BYTES as i64),
            ..EnableParams::default()
        })
        .await?;
        // Newer Chrome can keep bounded response bodies through a navigation.
        // A browser without this optional command still observes same-page receipts.
        let _ = page
            .execute(ConfigureDurableMessagesParams {
                max_total_buffer_size: Some(MAX_NETWORK_BUFFER_BYTES),
                max_resource_buffer_size: Some(MAX_RECEIPT_BYTES as i64),
            })
            .await;
        Ok(Self {
            target_id: page.target_id().clone(),
            frame_id: frame.id,
            loader_id: frame.loader_id,
            media_kind,
            requests,
            responses,
            finishes,
        })
    }

    /// Wait for evidence of platform acceptance. Missing evidence remains unknown.
    pub async fn wait(self, page: &Page) -> Result<Option<Receipt>> {
        self.wait_for(page, Duration::from_secs(90)).await
    }

    /// Allow more time for a manually completed platform verification.
    pub async fn wait_for(mut self, page: &Page, timeout: Duration) -> Result<Option<Receipt>> {
        ensure!(
            page.target_id() == &self.target_id,
            "抖音回执与当前发布页面不匹配"
        );
        match tokio::time::timeout(timeout, self.wait_for_receipt(page)).await {
            Ok(receipt) => receipt,
            Err(_) => Ok(None),
        }
    }

    async fn wait_for_receipt(&mut self, page: &Page) -> Result<Option<Receipt>> {
        let mut pending = HashMap::<RequestId, PendingReceipt>::new();
        loop {
            // The events come from one page in protocol order. Prioritizing earlier
            // event types preserves that order when multiple streams are buffered.
            tokio::select! {
                biased;
                event = self.requests.next() => {
                    let Some(event) = event else { return Ok(None) };
                    // Redirects reuse the request ID. Their bodies are not publish receipts.
                    pending.remove(&event.request_id);
                    if event.redirect_response.is_none()
                        && event.request.method == "POST"
                        && event.frame_id.as_ref() == Some(&self.frame_id)
                        && event.loader_id == self.loader_id
                        && composer_kind(&event.document_url) == Some(self.media_kind)
                        && let Some(operation) = publish_operation(&event.request.url)
                    {
                        pending.insert(event.request_id.clone(), PendingReceipt {
                            operation,
                            successful_response: false,
                        });
                    }
                }
                event = self.responses.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(receipt) = pending.get_mut(&event.request_id) {
                        let response = &event.response;
                        receipt.successful_response = event.loader_id == self.loader_id
                            && event.frame_id.as_ref().is_none_or(|id| id == &self.frame_id)
                            && publish_operation(&response.url) == Some(receipt.operation)
                            && (200..300).contains(&response.status)
                            && response.security_state == SecurityState::Secure
                            && !response.from_disk_cache.unwrap_or(false)
                            && !response.from_prefetch_cache.unwrap_or(false)
                            && !response.from_service_worker.unwrap_or(false);
                    }
                }
                event = self.finishes.next() => {
                    let Some(event) = event else { return Ok(None) };
                    let Some(receipt) = pending.remove(&event.request_id) else { continue };
                    if receipt.successful_response
                        && (0.0..=MAX_RECEIPT_BYTES as f64).contains(&event.encoded_data_length)
                        && let Ok(body) = page.execute(GetResponseBodyParams::new(event.request_id.clone())).await
                        && let Some(receipt) = receipt_from_body(body.result)
                    {
                        return Ok(Some(receipt));
                    }
                }
            }
        }
    }
}

fn creator_path(url: &str) -> Option<&str> {
    let path = url.strip_prefix(CREATOR_ORIGIN)?.split(['?', '#']).next()?;
    Some(path.strip_suffix('/').unwrap_or(path))
}

fn composer_kind(url: &str) -> Option<MediaKind> {
    match creator_path(url)? {
        "creator-micro/content/publish" | "creator-micro/content/post/video" => {
            Some(MediaKind::Video)
        }
        "creator-micro/content/publish-media/image-text" | "creator-micro/content/post/image" => {
            let query = url.split_once('?')?.1.split('#').next()?;
            let mut kind = None;
            let mut action = None;
            for (key, value) in query.split('&').filter_map(|pair| pair.split_once('=')) {
                match key {
                    "media_type" if kind.is_none() => kind = Some(value),
                    "type" if action.is_none() => action = Some(value),
                    "media_type" | "type" | "iid" | "aweme_id" | "item_id" => return None,
                    _ => {}
                }
            }
            (kind == Some("image") && action == Some("new")).then_some(MediaKind::Images)
        }
        _ => None,
    }
}

fn publish_operation(url: &str) -> Option<&'static str> {
    // These separate create operations appear in the official creator web bundle.
    match creator_path(url)? {
        "web/api/media/aweme/create" => Some("create"),
        "web/api/media/aweme/create_v2" => Some("create_v2"),
        _ => None,
    }
}

fn receipt_from_body(body: GetResponseBodyReturns) -> Option<Receipt> {
    let bytes = if body.base64_encoded {
        if body.body.len() > MAX_RECEIPT_BYTES * 2 {
            return None;
        }
        STANDARD.decode(body.body).ok()?
    } else {
        body.body.into_bytes()
    };
    if bytes.len() > MAX_RECEIPT_BYTES {
        return None;
    }
    let response = serde_json::from_slice(&bytes).ok()?;
    receipt_from_response(&response)
}

fn receipt_from_response(response: &Value) -> Option<Receipt> {
    if response.get("status_code")?.as_i64()? != 0
        || response
            .pointer("/data/status_code")
            .is_some_and(|status| status.as_i64() != Some(0))
    {
        return None;
    }
    let mut item_id = None;
    for path in [
        "/aweme_id",
        "/item_id",
        "/aweme/aweme_id",
        "/data/aweme_id",
        "/data/item_id",
        "/data/aweme/aweme_id",
    ] {
        let Some(field) = response.pointer(path).filter(|field| !field.is_null()) else {
            continue;
        };
        let id = numeric_item_id(field)?;
        if item_id.is_some_and(|previous| previous != id) {
            return None;
        }
        item_id = Some(id);
    }
    Some(Receipt {
        platform_id: item_id?.to_string(),
        evidence: "抖音创作者中心的发布接口返回成功及作品编号".into(),
        url: None,
    })
}

fn numeric_item_id(value: &Value) -> Option<u64> {
    let id = if let Some(text) = value.as_str() {
        if text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()?
    } else {
        value.as_u64()?
    };
    (id > 0).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn receipt_requires_platform_success_and_one_consistent_numeric_id() {
        let receipt = receipt_from_response(&json!({
            "status_code": 0,
            "data": {"aweme_id": "1234567890123456789"},
        }))
        .unwrap();
        assert_eq!(receipt.platform_id, "1234567890123456789");
        for response in [
            json!({"status_code": 0}),
            json!({"status_code": 1, "aweme_id": "12345"}),
            json!({"status_code": 0, "data": {"status_code": 1, "aweme_id": "12345"}}),
            json!({"status_code": 0, "aweme_id": "0"}),
            json!({"status_code": 0, "aweme_id": "12345", "item_id": "54321"}),
        ] {
            assert!(receipt_from_response(&response).is_none(), "{response}");
        }
    }

    #[test]
    fn only_known_creator_operations_can_supply_a_receipt() {
        assert_eq!(
            publish_operation(
                "https://creator.douyin.com/web/api/media/aweme/create_v2/?source=pc"
            ),
            Some("create_v2")
        );
        for url in [
            "https://creator.douyin.com.evil.test/web/api/media/aweme/create_v2/",
            "http://creator.douyin.com/web/api/media/aweme/create_v2/",
            "https://creator.douyin.com/web/api/media/aweme/draft/",
            "https://creator.douyin.com/web/api/media/aweme/create_v20/",
            "https://creator.douyin.com/creator-micro/content/manage",
        ] {
            assert_eq!(publish_operation(url), None, "{url}");
        }
    }

    #[test]
    fn image_receipts_require_the_new_photo_composer() {
        for path in ["post/image", "publish-media/image-text"] {
            let url = format!(
                "{CREATOR_ORIGIN}creator-micro/content/{path}?default-tab=3&media_type=image&type=new"
            );
            assert_eq!(composer_kind(&url), Some(MediaKind::Images));
            assert_eq!(composer_kind(&format!("{url}&iid=123")), None);
            assert_eq!(composer_kind(&format!("{url}&type=edit")), None);
            assert_eq!(composer_kind(&url.replace("type=new", "type=edit")), None);
        }
        assert_eq!(
            composer_kind("https://creator.douyin.com/creator-micro/content/post/video?type=new"),
            Some(MediaKind::Video)
        );
    }
}
