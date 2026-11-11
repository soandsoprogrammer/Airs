use super::{CREATOR_ORIGIN, editor::Expected};
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
        page::{FrameId, GetFrameTreeParams},
        security::SecurityState,
        target::TargetId,
    },
    listeners::EventStream,
};
use futures::StreamExt;
use publisher_core::{MediaKind, Receipt, XIAOHONGSHU_ACCEPTED_EVIDENCE};
use serde_json::Value;
use std::time::Duration;

const PUBLISH_URL: &str = "https://edith.xiaohongshu.com/web_api/sns/v2/note";
const MAX_MESSAGE_BYTES: usize = 128 * 1024;
const NETWORK_BUFFER_BYTES: i64 = 4 * 1024 * 1024;

pub(super) struct Observer {
    target_id: TargetId,
    frame_id: FrameId,
    loader_id: LoaderId,
    requests: EventStream<EventRequestWillBeSent>,
    responses: EventStream<EventResponseReceived>,
    finishes: EventStream<EventLoadingFinished>,
}

struct Pending {
    id: RequestId,
    accepted_transport: bool,
}

impl Observer {
    pub(super) async fn start(page: &Page) -> Result<Self> {
        let frame = page
            .execute(GetFrameTreeParams {})
            .await?
            .result
            .frame_tree
            .frame;
        ensure!(is_composer(&frame.url), "小红书发布页已变化");
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
            target_id: page.target_id().clone(),
            frame_id: frame.id,
            loader_id: frame.loader_id,
            requests,
            responses,
            finishes,
        })
    }

    pub(super) async fn wait_for(
        mut self,
        page: &Page,
        expected: &Expected,
        limit: Duration,
    ) -> Result<Option<Receipt>> {
        ensure!(
            page.target_id() == &self.target_id,
            "小红书回执与发布页面不匹配"
        );
        match tokio::time::timeout(limit, self.receive(page, expected)).await {
            Ok(result) => result,
            Err(_) => Ok(None),
        }
    }

    async fn receive(&mut self, page: &Page, expected: &Expected) -> Result<Option<Receipt>> {
        let mut pending = None::<Pending>;
        loop {
            tokio::select! {
                biased;
                event = self.requests.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if pending.as_ref().is_some_and(|request| request.id == event.request_id) {
                        return Ok(None);
                    }
                    if event.redirect_response.is_none() &&
                        event.request.method == "POST" &&
                        event.frame_id.as_ref() == Some(&self.frame_id) &&
                        event.loader_id == self.loader_id &&
                        is_composer(&event.document_url) &&
                        is_publish(&event.request.url)
                    {
                        if pending.is_some() {
                            return Ok(None);
                        }
                        let Ok(post) = page.execute(GetRequestPostDataParams::new(event.request_id.clone())).await
                        else { return Ok(None) };
                        if !matches_work(&post.result.post_data, expected) {
                            return Ok(None);
                        }
                        pending = Some(Pending { id: event.request_id.clone(), accepted_transport: false });
                    }
                }
                event = self.responses.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(pending) = pending.as_mut().filter(|pending| pending.id == event.request_id) {
                        let response = &event.response;
                        pending.accepted_transport = event.loader_id == self.loader_id &&
                            event.frame_id.as_ref().is_none_or(|id| id == &self.frame_id) &&
                            is_publish(&response.url) &&
                            (200..300).contains(&response.status) &&
                            response.security_state == SecurityState::Secure &&
                            !response.from_disk_cache.unwrap_or(false) &&
                            !response.from_prefetch_cache.unwrap_or(false) &&
                            !response.from_service_worker.unwrap_or(false);
                    }
                }
                event = self.finishes.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(pending) = pending.as_ref().filter(|pending| pending.id == event.request_id) {
                        if pending.accepted_transport &&
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

fn is_composer(url: &str) -> bool {
    url.strip_prefix(CREATOR_ORIGIN)
        .and_then(|rest| rest.split(['?', '#']).next())
        == Some("/publish/publish")
}

fn is_publish(url: &str) -> bool {
    url.split('?').next() == Some(PUBLISH_URL)
}

fn matches_work(body: &str, expected: &Expected) -> bool {
    if body.len() > MAX_MESSAGE_BYTES {
        return false;
    }
    let Ok(body) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let Some(common) = body.get("common") else {
        return false;
    };
    if common.get("title").and_then(Value::as_str) != Some(&expected.title)
        || !common
            .get("desc")
            .and_then(Value::as_str)
            .is_some_and(|desc| desc.split_whitespace().eq(expected.body.split_whitespace()))
        || common
            .get("note_id")
            .is_some_and(|id| !id.is_null() && id.as_str() != Some(""))
        || common
            .pointer("/privacy_info/type")
            .is_some_and(|value| value.as_i64() != Some(0))
    {
        return false;
    }
    let work_type = common.get("type").and_then(Value::as_str);
    let valid_id = |value: &Value| value.as_str().is_some_and(|text| !text.trim().is_empty());
    match expected.kind {
        MediaKind::Video => {
            work_type == Some("video")
                && body
                    .pointer("/video_info/file_id")
                    .or_else(|| body.pointer("/video_info/fileid"))
                    .is_some_and(valid_id)
        }
        MediaKind::Images => {
            work_type == Some("normal")
                && body
                    .pointer("/image_info/images")
                    .and_then(Value::as_array)
                    .is_some_and(|images| {
                        images.len() == expected.image_count
                            && images
                                .iter()
                                .all(|image| image.get("file_id").is_some_and(valid_id))
                    })
        }
    }
}

fn parse_receipt(response: GetResponseBodyReturns) -> Option<Receipt> {
    let bytes = if response.base64_encoded {
        if response.body.len() > MAX_MESSAGE_BYTES.div_ceil(3) * 4 {
            return None;
        }
        STANDARD.decode(response.body).ok()?
    } else {
        response.body.into_bytes()
    };
    if bytes.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("success").and_then(Value::as_bool) != Some(true)
        || value
            .get("code")
            .is_some_and(|code| code.as_i64() != Some(0))
    {
        return None;
    }
    // The composer opens its success page even when a note identifier is omitted.
    let data = value.get("data").unwrap_or(&Value::Null);
    let id = ["id", "noteId", "note_id"]
        .iter()
        .find_map(|field| {
            data.get(field)
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        })
        .unwrap_or_default();
    if !id.is_empty()
        && (!(12..=128).contains(&id.len())
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    {
        return None;
    }
    Some(Receipt {
        platform_id: id.to_owned(),
        evidence: XIAOHONGSHU_ACCEPTED_EVIDENCE.into(),
        url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn receipt_requires_explicit_acceptance_and_keeps_any_valid_note_id() {
        let id = "abcdef0123456789abcdef01";
        let parse = |value: Value| {
            parse_receipt(GetResponseBodyReturns {
                body: value.to_string(),
                base64_encoded: false,
            })
        };
        assert_eq!(
            parse(json!({"success":true,"code":0,"data":{"note_id":id}}))
                .unwrap()
                .platform_id,
            id
        );
        assert!(
            parse(json!({"success":true,"code":0,"data":{}}))
                .unwrap()
                .platform_id
                .is_empty()
        );
        for value in [
            json!({"success":false,"code":0,"data":{"id":id}}),
            json!({"success":true,"code":500,"data":{"id":id}}),
            json!({"success":true,"code":0,"data":{"id":"https://example.com/"}}),
            json!({"code":0,"data":{"id":id}}),
        ] {
            assert!(parse(value).is_none());
        }
    }

    #[test]
    fn receipt_request_matches_the_title_body_and_photo_count() {
        let expected = Expected {
            kind: MediaKind::Images,
            title: "图文标题".into(),
            body: "第一行\n第二行 #标签".into(),
            image_count: 2,
        };
        let mut body = json!({
            "common":{"note_id":"","title": "图文标题", "desc":"第一行\n第二行 #标签", "type":"normal", "privacy_info":{"type":0}},
            "image_info":{"images":[{"file_id":"image1"},{"file_id":"image2"}]}
        });
        assert!(matches_work(&body.to_string(), &expected));
        body["common"]["privacy_info"]["type"] = json!(1);
        assert!(!matches_work(&body.to_string(), &expected));
        body["common"]["privacy_info"]["type"] = json!(0);
        body["image_info"]["images"].as_array_mut().unwrap().pop();
        assert!(!matches_work(&body.to_string(), &expected));
        assert!(!is_composer(
            "https://creator.xiaohongshu.com.example/publish/publish"
        ));
        assert!(!is_publish(
            "https://edith.xiaohongshu.com/web_api/sns/v2/note/edit"
        ));
    }
}
