use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::{
    network::{
        ConfigureDurableMessagesParams, EnableParams, EventLoadingFinished, EventRequestWillBeSent,
        EventResponseReceived, GetRequestPostDataParams, GetResponseBodyParams,
        GetResponseBodyReturns, LoaderId, RequestId,
    },
    page::{FrameId, GetFrameTreeParams},
    security::SecurityState,
    target::TargetId,
};
use chromiumoxide::listeners::EventStream;
use futures::StreamExt;
use publisher_core::Receipt;
use serde_json::Value;
use std::time::Duration;

const CREATOR_ORIGIN: &str = "https://cp.kuaishou.com/";
const MAX_MESSAGE_BYTES: usize = 128 * 1024;
const MAX_NETWORK_BUFFER_BYTES: i64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AtlasIdentity {
    file_id: String,
    atlas_id: String,
}

pub(crate) struct KuaishouReceiptObserver {
    target_id: TargetId,
    frame_id: FrameId,
    loader_id: LoaderId,
    caption: String,
    atlas: Option<AtlasIdentity>,
    requests: EventStream<EventRequestWillBeSent>,
    responses: EventStream<EventResponseReceived>,
    finishes: EventStream<EventLoadingFinished>,
}

struct PendingReceipt {
    id: RequestId,
    operation: &'static str,
    accepted_transport: bool,
}

impl KuaishouReceiptObserver {
    pub(crate) async fn start(
        page: &Page,
        caption: &str,
        atlas: Option<&AtlasIdentity>,
    ) -> Result<Self> {
        let frame = page
            .execute(GetFrameTreeParams {})
            .await?
            .result
            .frame_tree
            .frame;
        ensure!(
            is_composer(&frame.url, atlas.is_some()),
            "快手发布页已变化，无法确认当前准备的作品"
        );
        let caption = normalize_caption(caption);
        ensure!(!caption.is_empty(), "快手作品描述为空");
        let requests = page.event_listener::<EventRequestWillBeSent>().await?;
        let responses = page.event_listener::<EventResponseReceived>().await?;
        let finishes = page.event_listener::<EventLoadingFinished>().await?;
        page.execute(EnableParams {
            max_post_data_size: Some(0),
            max_total_buffer_size: Some(MAX_NETWORK_BUFFER_BYTES),
            max_resource_buffer_size: Some(MAX_MESSAGE_BYTES as i64),
            ..EnableParams::default()
        })
        .await?;
        // The creator UI navigates to management after accepting the submission.
        let _ = page
            .execute(ConfigureDurableMessagesParams {
                max_total_buffer_size: Some(MAX_NETWORK_BUFFER_BYTES),
                max_resource_buffer_size: Some(MAX_MESSAGE_BYTES as i64),
            })
            .await;
        Ok(Self {
            target_id: page.target_id().clone(),
            frame_id: frame.id,
            loader_id: frame.loader_id,
            caption,
            atlas: atlas.cloned(),
            requests,
            responses,
            finishes,
        })
    }

    pub(crate) async fn wait_for(
        mut self,
        page: &Page,
        timeout: Duration,
    ) -> Result<Option<Receipt>> {
        ensure!(
            page.target_id() == &self.target_id,
            "快手回执与当前发布页面不匹配"
        );
        match tokio::time::timeout(timeout, self.wait_for_receipt(page)).await {
            Ok(result) => result,
            Err(_) => Ok(None),
        }
    }

    async fn wait_for_receipt(&mut self, page: &Page) -> Result<Option<Receipt>> {
        let mut pending = None::<PendingReceipt>;
        loop {
            tokio::select! {
                biased;
                event = self.requests.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if pending.as_ref().is_some_and(|request| request.id == event.request_id) {
                        // Redirects reuse the request identifier.
                        return Ok(None);
                    }
                    if event.redirect_response.is_none()
                        && event.request.method == "POST"
                        && event.frame_id.as_ref() == Some(&self.frame_id)
                        && event.loader_id == self.loader_id
                        && is_composer(&event.document_url, self.atlas.is_some())
                        && let Some(operation) = publish_operation(&event.request.url)
                        && (operation == "atlas") == self.atlas.is_some()
                    {
                        if pending.is_some() {
                            return Ok(None);
                        }
                        // The official body contains the prepared caption and its uploaded file ID.
                        // Read only this scoped request, and never log or store its body.
                        let Ok(post) = page.execute(GetRequestPostDataParams::new(event.request_id.clone())).await
                        else { return Ok(None) };
                        if !matches_prepared_work(&post.result.post_data, &self.caption, self.atlas.as_ref()) {
                            return Ok(None);
                        }
                        pending = Some(PendingReceipt {
                            id: event.request_id.clone(),
                            operation,
                            accepted_transport: false,
                        });
                    }
                }
                event = self.responses.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(receipt) = pending.as_mut().filter(|receipt| receipt.id == event.request_id) {
                        let response = &event.response;
                        receipt.accepted_transport = event.loader_id == self.loader_id
                            && event.frame_id.as_ref().is_none_or(|id| id == &self.frame_id)
                            && publish_operation(&response.url) == Some(receipt.operation)
                            && is_secure_response(response);
                    }
                }
                event = self.finishes.next() => {
                    let Some(event) = event else { return Ok(None) };
                    if let Some(receipt) = pending.as_ref().filter(|receipt| receipt.id == event.request_id) {
                        if receipt.accepted_transport
                            && (0.0..=MAX_MESSAGE_BYTES as f64).contains(&event.encoded_data_length)
                            && let Ok(body) = page.execute(GetResponseBodyParams::new(event.request_id.clone())).await
                        {
                            return Ok(receipt_from_body(body.result));
                        }
                        return Ok(None);
                    }
                }
            }
        }
    }
}

fn creator_path(url: &str) -> Option<&str> {
    url.strip_prefix(CREATOR_ORIGIN)?.split(['?', '#']).next()
}

fn is_composer(url: &str, atlas: bool) -> bool {
    if !matches!(
        creator_path(url),
        Some("article/publish/video" | "article/publish/video/")
    ) {
        return false;
    }
    let query = url.split('#').next().and_then(|url| url.split_once('?'));
    let mut tabs = query
        .into_iter()
        .flat_map(|(_, query)| query.split('&'))
        .filter_map(|field| field.strip_prefix("tabType="));
    let tab = tabs.next();
    tabs.next().is_none()
        && if atlas {
            tab == Some("2")
        } else {
            matches!(tab, None | Some("1"))
        }
}

fn is_secure_response(response: &chromiumoxide::cdp::browser_protocol::network::Response) -> bool {
    (200..300).contains(&response.status)
        && response.security_state == SecurityState::Secure
        && !response.from_disk_cache.unwrap_or(false)
        && !response.from_prefetch_cache.unwrap_or(false)
        && !response.from_service_worker.unwrap_or(false)
}

fn publish_operation(url: &str) -> Option<&'static str> {
    let path = creator_path(url)?;
    match path.strip_prefix("forward/cp/").unwrap_or(path) {
        "rest/cp/works/v2/video/pc/submit" => Some("v2"),
        "rest/cp/works/v3/video/pc/submit" => Some("v3"),
        "rest/cp/works/v2/video/pc/isolation/submit" => Some("isolation"),
        "rest/cp/works/atlas/pc/publish/submit" => Some("atlas"),
        _ => None,
    }
}

fn normalize_caption(caption: &str) -> String {
    // The official composer inserts a space before an attached hashtag.
    let mut separated = String::with_capacity(caption.len());
    let mut attached = false;
    for character in caption.chars() {
        if character == '#' && attached {
            separated.push(' ');
        }
        separated.push(character);
        attached = !character.is_whitespace();
    }
    separated.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn matches_prepared_work(body: &str, caption: &str, atlas: Option<&AtlasIdentity>) -> bool {
    if body.len() > MAX_MESSAGE_BYTES {
        return false;
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|body| {
            let matches_caption = body
                .get("caption")
                .and_then(Value::as_str)
                .is_some_and(|actual| normalize_caption(actual) == caption);
            if let Some(atlas) = atlas {
                matches_caption
                    && atlas_identity(&body).as_ref() == Some(atlas)
                    && body.get("photoStatus").and_then(Value::as_i64) == Some(1)
                    && body.get("publishTime").and_then(Value::as_i64) == Some(0)
            } else {
                matches_caption && body.get("fileId").and_then(valid_id).is_some()
            }
        })
}

fn atlas_identity(value: &Value) -> Option<AtlasIdentity> {
    Some(AtlasIdentity {
        file_id: value.get("fileId").and_then(valid_id)?,
        atlas_id: value.get("atlasId").and_then(valid_id)?,
    })
}

fn valid_id(value: &Value) -> Option<String> {
    if let Some(number) = value.as_u64().filter(|&number| number != 0) {
        return Some(number.to_string());
    }
    let id = value.as_str()?;
    (!id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && !matches!(id, "null" | "undefined")
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        && id.bytes().any(|c| c.is_ascii_alphanumeric() && c != b'0'))
    .then(|| id.to_owned())
}

fn receipt_from_body(body: GetResponseBodyReturns) -> Option<Receipt> {
    receipt_from_response(&decode_response(body)?)
}

fn decode_response(body: GetResponseBodyReturns) -> Option<Value> {
    let bytes = if body.base64_encoded {
        if body.body.len() > MAX_MESSAGE_BYTES * 2 {
            return None;
        }
        STANDARD.decode(body.body).ok()?
    } else {
        body.body.into_bytes()
    };
    if bytes.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub(crate) struct AtlasUploadObserver {
    frame_id: FrameId,
    loader_id: LoaderId,
    requests: EventStream<EventRequestWillBeSent>,
    responses: EventStream<EventResponseReceived>,
    finishes: EventStream<EventLoadingFinished>,
}

impl AtlasUploadObserver {
    pub(crate) async fn start(page: &Page) -> Result<Self> {
        let frame = page
            .execute(GetFrameTreeParams {})
            .await?
            .result
            .frame_tree
            .frame;
        ensure!(is_composer(&frame.url, true), "当前页面不是快手图文编辑页");
        let requests = page.event_listener::<EventRequestWillBeSent>().await?;
        let responses = page.event_listener::<EventResponseReceived>().await?;
        let finishes = page.event_listener::<EventLoadingFinished>().await?;
        page.execute(EnableParams {
            max_post_data_size: Some(0),
            max_total_buffer_size: Some(MAX_NETWORK_BUFFER_BYTES),
            max_resource_buffer_size: Some(MAX_MESSAGE_BYTES as i64),
            ..EnableParams::default()
        })
        .await?;
        Ok(Self {
            frame_id: frame.id,
            loader_id: frame.loader_id,
            requests,
            responses,
            finishes,
        })
    }

    pub(crate) async fn wait(mut self, page: &Page, count: usize) -> Option<AtlasIdentity> {
        let mut pending = None;
        let mut secure = false;
        loop {
            tokio::select! {
                biased;
                event = self.requests.next() => {
                    let event = event?;
                    if pending.as_ref() == Some(&event.request_id) {
                        return None;
                    }
                    if event.redirect_response.is_none()
                        && event.frame_id.as_ref() == Some(&self.frame_id)
                        && event.loader_id == self.loader_id
                        && is_composer(&event.document_url, true)
                        && event.request.method == "POST"
                        && is_atlas_upload_pre(&event.request.url)
                    {
                        if pending.is_some() { return None; }
                        let body = page.execute(GetRequestPostDataParams::new(event.request_id.clone())).await.ok()?;
                        if !is_fresh_atlas_request(&body.result.post_data, count) { return None; }
                        pending = Some(event.request_id.clone());
                    }
                }
                event = self.responses.next() => {
                    let event = event?;
                    if pending.as_ref() == Some(&event.request_id) {
                        secure = event.loader_id == self.loader_id
                            && event.frame_id.as_ref().is_none_or(|id| id == &self.frame_id)
                            && is_atlas_upload_pre(&event.response.url)
                            && is_secure_response(&event.response);
                    }
                }
                event = self.finishes.next() => {
                    let event = event?;
                    if pending.as_ref() == Some(&event.request_id) {
                        if !secure || !(0.0..=MAX_MESSAGE_BYTES as f64).contains(&event.encoded_data_length) {
                            return None;
                        }
                        let body = page.execute(GetResponseBodyParams::new(event.request_id.clone())).await.ok()?;
                        return atlas_upload_identity(&decode_response(body.result)?, count);
                    }
                }
            }
        }
    }
}

fn is_atlas_upload_pre(url: &str) -> bool {
    let Some(path) = creator_path(url) else {
        return false;
    };
    path.strip_prefix("forward/cp/").unwrap_or(path) == "rest/cp/works/atlas/pc/upload/pre"
}

fn is_fresh_atlas_request(body: &str, count: usize) -> bool {
    if body.len() > MAX_MESSAGE_BYTES {
        return false;
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|body| {
            body.get("uploadType").and_then(Value::as_u64) == Some(10)
                && body.get("pictureCount").and_then(Value::as_u64) == Some(count as u64)
                && body.get("fileId").is_none_or(Value::is_null)
                && body.get("atlasId").is_none_or(Value::is_null)
        })
}

fn atlas_upload_identity(body: &Value, count: usize) -> Option<AtlasIdentity> {
    if body.get("result")?.as_i64()? != 1 {
        return None;
    }
    let data = body.get("data")?;
    if data.get("uploadInfo")?.as_array()?.len() != count {
        return None;
    }
    atlas_identity(data)
}

fn receipt_from_response(response: &Value) -> Option<Receipt> {
    if response.get("result")?.as_i64()? != 1 {
        return None;
    }
    // The official page treats result:1 as accepted. A work ID is optional.
    let mut work_id = None;
    for path in ["/workId", "/photoId", "/data/workId", "/data/photoId"] {
        let Some(id) = response.pointer(path).and_then(valid_id) else {
            continue;
        };
        if work_id.as_ref().is_some_and(|previous| previous != &id) {
            work_id = None;
            break;
        }
        work_id = Some(id);
    }
    Some(Receipt {
        platform_id: work_id.unwrap_or_default(),
        evidence: "快手创作者服务平台的发布接口确认已接收作品".into(),
        url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn video_submit_endpoints_are_exact_and_same_origin() {
        for route in [
            "rest/cp/works/v2/video/pc/submit",
            "rest/cp/works/v3/video/pc/submit",
            "forward/cp/rest/cp/works/v2/video/pc/isolation/submit",
        ] {
            assert!(publish_operation(&format!("{CREATOR_ORIGIN}{route}?sig=fixture")).is_some());
        }
        for url in [
            "https://cp.kuaishou.com.evil.invalid/rest/cp/works/v2/video/pc/submit",
            "http://cp.kuaishou.com/rest/cp/works/v2/video/pc/submit",
            "https://cp.kuaishou.com/rest/cp/works/v2/video/pc/upload/finish",
            "https://cp.kuaishou.com/rest/cp/works/v2/atlas/pc/submit",
        ] {
            assert!(publish_operation(url).is_none());
        }
    }

    #[test]
    fn caption_and_uploaded_file_are_required_to_match_request() {
        let expected = normalize_caption("测试标题#话题\n说明");
        let body = json!({"fileId": 123, "caption": "测试标题 #话题\n说明"});
        assert!(matches_prepared_work(&body.to_string(), &expected, None));
        for body in [
            json!({"fileId": -1, "caption": "测试标题 #话题\n说明"}),
            json!({"caption": "测试标题 #话题\n说明"}),
            json!({"fileId": 123, "caption": "不同标题 #话题\n说明"}),
        ] {
            assert!(!matches_prepared_work(&body.to_string(), &expected, None));
        }
    }

    #[test]
    fn atlas_receipt_matches_the_fresh_upload_and_public_immediate_settings() {
        let url = format!("{CREATOR_ORIGIN}article/publish/video?tabType=2");
        assert!(is_composer(&url, true));
        assert!(!is_composer(&url, false));
        assert!(!is_composer(&format!("{url}&tabType=1"), true));
        let upload = "rest/cp/works/atlas/pc/upload/pre";
        assert!(is_atlas_upload_pre(&format!("{CREATOR_ORIGIN}{upload}")));
        assert!(is_atlas_upload_pre(&format!(
            "{CREATOR_ORIGIN}forward/cp/{upload}"
        )));
        assert!(publish_operation(&format!("{CREATOR_ORIGIN}{upload}")).is_none());
        assert_eq!(
            publish_operation(&format!(
                "{CREATOR_ORIGIN}rest/cp/works/atlas/pc/publish/submit"
            )),
            Some("atlas")
        );
        let pre = json!({"pictureCount": 2, "uploadType": 10});
        assert!(is_fresh_atlas_request(&pre.to_string(), 2));
        assert!(!is_fresh_atlas_request(&pre.to_string(), 3));
        let mut draft = pre;
        draft["atlasId"] = json!("another-draft");
        assert!(!is_fresh_atlas_request(&draft.to_string(), 2));
        let uploaded = json!({
            "result": 1, "data": {"atlasId": "a123", "fileId": 321, "uploadInfo": [{}, {}]}
        });
        assert!(atlas_upload_identity(&uploaded, 3).is_none());
        let atlas = atlas_upload_identity(&uploaded, 2).unwrap();
        let exact = json!({
            "atlasId": "a123", "fileId": "321", "caption": "本地图文",
            "photoStatus": 1, "publishTime": 0
        });
        assert!(matches_prepared_work(
            &exact.to_string(),
            "本地图文",
            Some(&atlas)
        ));
        for (key, value) in [
            ("atlasId", json!("a999")),
            ("fileId", json!(999)),
            ("caption", json!("其他图文")),
            ("photoStatus", json!(2)),
            ("publishTime", json!(1000)),
        ] {
            let mut invalid = exact.clone();
            invalid[key] = value;
            assert!(!matches_prepared_work(
                &invalid.to_string(),
                "本地图文",
                Some(&atlas)
            ));
        }
    }

    #[test]
    fn official_success_can_omit_a_work_id_without_inventing_one() {
        let no_id = receipt_from_response(&json!({"result": 1, "data": null})).unwrap();
        assert!(no_id.platform_id.is_empty());
        assert!(no_id.url.is_none());
        let receipt =
            receipt_from_response(&json!({"result": 1, "data": {"photoId": "3x123"}})).unwrap();
        assert_eq!(receipt.platform_id, "3x123");
        assert!(
            receipt_from_response(&json!({"result": -1, "data": {"photoId": "3x123"}})).is_none()
        );
        assert!(receipt_from_response(&json!({"data": {"photoId": "3x123"}})).is_none());
    }

    #[tokio::test]
    #[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
    async fn request_body_can_be_read_only_after_matching_the_post() -> Result<()> {
        use crate::browser::BrowserSession;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", server.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((mut connection, _)) = server.accept().await {
                tokio::spawn(async move {
                    let _ = connection.read(&mut [0u8; 4096]).await;
                    let _ = connection
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nOK",
                        )
                        .await;
                });
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser.browser.new_page(url).await?;
        let mut requests = page.event_listener::<EventRequestWillBeSent>().await?;
        page.execute(EnableParams {
            max_post_data_size: Some(0),
            ..EnableParams::default()
        })
        .await?;
        page.evaluate_expression(
            "fetch('/fixture', {method:'POST', body:JSON.stringify({fileId:321,caption:'本地测试'})})"
        ).await?;
        let event = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(request) = requests.next().await {
                if request.request.method == "POST" && request.request.url.ends_with("/fixture") {
                    return Some(request);
                }
            }
            None
        })
        .await?
        .expect("local POST");
        let body = page
            .execute(GetRequestPostDataParams::new(event.request_id.clone()))
            .await?;
        assert!(matches_prepared_work(
            &body.result.post_data,
            "本地测试",
            None
        ));
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
