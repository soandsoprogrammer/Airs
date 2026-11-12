use super::CREATOR_ORIGIN;
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Page,
    cdp::browser_protocol::{
        network::{
            ConfigureDurableMessagesParams, EnableParams, EventLoadingFinished,
            EventRequestWillBeSent, EventResponseReceived, GetRequestPostDataParams,
            GetResponseBodyParams, GetResponseBodyReturns, LoaderId, RequestId,
        },
        page::{FrameId, FrameTree, GetFrameTreeParams},
        security::SecurityState,
        target::TargetId,
    },
    listeners::EventStream,
};
use futures::StreamExt;
use publisher_core::{Receipt, WECHAT_CHANNELS_ACCEPTED_EVIDENCE};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

const MAX_MESSAGE_BYTES: usize = 128 * 1024;
const NETWORK_BUFFER_BYTES: i64 = 4 * 1024 * 1024;

pub(super) struct Observer {
    target: TargetId,
    frames: HashMap<FrameId, LoaderId>,
    requests: EventStream<EventRequestWillBeSent>,
    responses: EventStream<EventResponseReceived>,
    finishes: EventStream<EventLoadingFinished>,
}

struct Pending {
    id: RequestId,
    frame: FrameId,
    loader: LoaderId,
    accepted_transport: bool,
}

impl Observer {
    pub(super) async fn start(page: &Page) -> Result<Self> {
        let tree = page.execute(GetFrameTreeParams {}).await?.result.frame_tree;
        ensure!(
            trusted_path(&tree.frame.url) == Some("/platform/post/create"),
            "视频号发布页已变化"
        );
        let mut frames = HashMap::new();
        collect_frames(&tree, &mut frames);
        ensure!(
            frames.contains_key(&tree.frame.id),
            "无法核对视频号回执页面"
        );
        let requests = page.event_listener::<EventRequestWillBeSent>().await?;
        let responses = page.event_listener::<EventResponseReceived>().await?;
        let finishes = page.event_listener::<EventLoadingFinished>().await?;
        page.execute(EnableParams {
            max_post_data_size: Some(0),
            max_total_buffer_size: Some(NETWORK_BUFFER_BYTES),
            max_resource_buffer_size: Some(MAX_MESSAGE_BYTES as i64),
            ..EnableParams::default()
        })
        .await?;
        let _ = page
            .execute(ConfigureDurableMessagesParams {
                max_total_buffer_size: Some(NETWORK_BUFFER_BYTES),
                max_resource_buffer_size: Some(MAX_MESSAGE_BYTES as i64),
            })
            .await;
        Ok(Self {
            target: page.target_id().clone(),
            frames,
            requests,
            responses,
            finishes,
        })
    }

    pub(super) async fn wait_for(
        mut self,
        page: &Page,
        description: &str,
        short_title: Option<&str>,
        limit: Duration,
    ) -> Result<Option<Receipt>> {
        ensure!(page.target_id() == &self.target, "视频号回执页面已变化");
        match tokio::time::timeout(limit, self.receive(page, description, short_title)).await {
            Ok(result) => result,
            Err(_) => Ok(None),
        }
    }

    async fn receive(
        &mut self,
        page: &Page,
        description: &str,
        short_title: Option<&str>,
    ) -> Result<Option<Receipt>> {
        let mut pending = None::<Pending>;
        loop {
            tokio::select! {
                biased;
                event = self.requests.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if pending.as_ref().is_some_and(|request| request.id == event.request_id) {
                        return Ok(None);
                    }
                    let Some(frame) = event.frame_id.as_ref() else { continue };
                    if event.redirect_response.is_some() || event.request.method != "POST" ||
                        self.frames.get(frame) != Some(&event.loader_id) ||
                        !is_composer_document(&event.document_url) ||
                        !is_create_request(&event.request.url)
                    {
                        continue;
                    }
                    if pending.is_some() {
                        return Ok(None);
                    }
                    let Ok(body) = page.execute(GetRequestPostDataParams::new(event.request_id.clone())).await
                    else { return Ok(None) };
                    if !matches_work(&body.result.post_data, description, short_title) {
                        return Ok(None);
                    }
                    pending = Some(Pending {
                        id: event.request_id.clone(),
                        frame: frame.clone(),
                        loader: event.loader_id.clone(),
                        accepted_transport: false,
                    });
                }
                event = self.responses.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(request) = pending.as_mut().filter(|request| request.id == event.request_id) {
                        let response = &event.response;
                        request.accepted_transport = event.loader_id == request.loader &&
                            event.frame_id.as_ref().is_none_or(|frame| frame == &request.frame) &&
                            is_create_request(&response.url) &&
                            (200..300).contains(&response.status) &&
                            response.security_state == SecurityState::Secure &&
                            !response.from_disk_cache.unwrap_or(false) &&
                            !response.from_prefetch_cache.unwrap_or(false) &&
                            !response.from_service_worker.unwrap_or(false);
                    }
                }
                event = self.finishes.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(request) = pending.as_ref().filter(|request| request.id == event.request_id) {
                        if request.accepted_transport &&
                            (0.0..=MAX_MESSAGE_BYTES as f64).contains(&event.encoded_data_length) &&
                            let Ok(body) = page.execute(GetResponseBodyParams::new(event.request_id.clone())).await
                        {
                            return Ok(parse_receipt(body.result));
                        }
                        return Ok(None);
                    }
                }
            }
        }
    }
}

fn trusted_path(url: &str) -> Option<&str> {
    let rest = url.strip_prefix(CREATOR_ORIGIN)?;
    rest.starts_with('/')
        .then(|| rest.split(['?', '#']).next().unwrap_or_default())
}

fn is_composer_document(url: &str) -> bool {
    matches!(
        trusted_path(url),
        Some("/platform/post/create" | "/micro/content/post/create" | "/micro/content/")
    ) || url == "about:blank"
}

fn collect_frames(tree: &FrameTree, frames: &mut HashMap<FrameId, LoaderId>) {
    let frame = &tree.frame;
    if frame.security_origin == CREATOR_ORIGIN && is_composer_document(&frame.url) {
        frames.insert(frame.id.clone(), frame.loader_id.clone());
    }
    for child in tree.child_frames.iter().flatten() {
        collect_frames(child, frames);
    }
}

fn is_create_request(url: &str) -> bool {
    matches!(
        trusted_path(url),
        Some(
            "/micro/content/cgi-bin/mmfinderassistant-bin/post/post_create"
                | "/cgi-bin/mmfinderassistant-bin/post/post_create"
        )
    )
}

fn matches_work(body: &str, description: &str, short_title: Option<&str>) -> bool {
    if body.len() > MAX_MESSAGE_BYTES {
        return false;
    }
    let Ok(body) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let Some(work) = body.get("objectDesc") else {
        return false;
    };
    let actual_title = work
        .pointer("/shortTitle/0/shortTitle")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let media_matches = work
        .get("media")
        .and_then(Value::as_array)
        .is_some_and(|media| {
            media.len() == 1
                && media[0].get("mediaType").and_then(Value::as_i64) == Some(4)
                && media[0]
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| !url.is_empty())
        });
    body.get("mode").and_then(Value::as_i64) == Some(1)
        && body
            .get("clientid")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
        && body
            .get("effectiveTime")
            .is_none_or(|time| time.is_null() || time.as_i64() == Some(0))
        && work.get("mediaType").and_then(Value::as_i64) == Some(4)
        && work
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|actual| actual.split_whitespace().eq(description.split_whitespace()))
        && short_title.filter(|title| !title.is_empty()) == actual_title
        && media_matches
}

fn parse_receipt(body: GetResponseBodyReturns) -> Option<Receipt> {
    let bytes = if body.base64_encoded {
        if body.body.len() > MAX_MESSAGE_BYTES.div_ceil(3) * 4 {
            return None;
        }
        STANDARD.decode(body.body).ok()?
    } else {
        body.body.into_bytes()
    };
    if bytes.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    (value.get("errCode").and_then(Value::as_i64) == Some(0)
        && value
            .pointer("/data/baseResp/errcode")
            .and_then(Value::as_i64)
            == Some(0))
    .then(|| Receipt {
        platform_id: String::new(),
        evidence: WECHAT_CHANNELS_ACCEPTED_EVIDENCE.into(),
        url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn receipt_requires_both_explicit_success_codes() {
        let body = |json: Value| GetResponseBodyReturns {
            body: json.to_string(),
            base64_encoded: false,
        };
        let receipt = parse_receipt(body(json!({"errCode":0,"data":{"baseResp":{"errcode":0}}})))
            .expect("the creator service accepted this video");
        assert_eq!(receipt.evidence, WECHAT_CHANNELS_ACCEPTED_EVIDENCE);
        assert!(receipt.platform_id.is_empty() && receipt.url.is_none());
        for value in [
            json!({"errCode":0,"data":{}}),
            json!({"errCode":0,"data":{"baseResp":{"errcode":-11233}}}),
            json!({"errCode":1,"data":{"baseResp":{"errcode":0}}}),
        ] {
            assert!(parse_receipt(body(value)).is_none());
        }
    }

    #[test]
    fn receipt_observer_accepts_only_the_expected_immediate_video() {
        let endpoint = format!(
            "{CREATOR_ORIGIN}/micro/content/cgi-bin/mmfinderassistant-bin/post/post_create"
        );
        assert!(is_create_request(&endpoint));
        assert!(!is_create_request(
            &endpoint.replace("post_create", "post_draft")
        ));
        assert!(!is_create_request(
            &endpoint.replace("weixin.qq.com", "weixin.qq.com.invalid")
        ));
        let mut body = json!({
            "mode": 1,
            "clientid": "fixture",
            "objectDesc": {
                "mediaType": 4,
                "description": "测试标题\n发布简介",
                "shortTitle": [{"shortTitle": "测试发布短标题"}],
                "media": [{"mediaType": 4, "url": "fixture-video"}],
            }
        });
        let expected_title = Some("测试发布短标题");
        assert!(matches_work(
            &body.to_string(),
            "测试标题\n发布简介",
            expected_title
        ));
        assert!(!matches_work(
            &body.to_string(),
            "另一个作品",
            expected_title
        ));
        assert!(!matches_work(&body.to_string(), "测试标题\n发布简介", None));
        body["effectiveTime"] = json!(1_900_000_000);
        assert!(!matches_work(
            &body.to_string(),
            "测试标题\n发布简介",
            expected_title
        ));
    }
}
