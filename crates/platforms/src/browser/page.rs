//! Shared browser operations. WebKit-specific network evidence is handled
//! separately; it must not be represented as Chrome transport events.
use anyhow::{Result, bail};
use chromiumoxide::{
    Command,
    cdp::IntoEventKind,
    cdp::{
        browser_protocol::{dom::BackendNodeId, page::FrameId, target::TargetId},
        js_protocol::runtime::{EvaluateParams, ExecutionContextId, RemoteObject},
    },
    js::{Evaluation, EvaluationResult},
    layout::Point,
    listeners::EventStream,
    page::ScreenshotParams,
    types::CommandResponse,
};
use serde_json::Value;

#[cfg(target_os = "macos")]
use std::sync::Arc;

#[derive(Clone)]
pub enum Page {
    Chrome(chromiumoxide::Page),
    #[cfg(target_os = "macos")]
    WebKit {
        session: Arc<crate::webkit::Session>,
        target: TargetId,
    },
}

impl Page {
    #[cfg(target_os = "macos")]
    pub fn webkit(&self) -> Option<&crate::webkit::Session> {
        match self {
            Self::WebKit { session, .. } => Some(session),
            Self::Chrome(_) => None,
        }
    }

    pub fn target_id(&self) -> &TargetId {
        match self {
            Self::Chrome(page) => page.target_id(),
            #[cfg(target_os = "macos")]
            Self::WebKit { target, .. } => target,
        }
    }

    pub async fn goto(&self, url: impl AsRef<str>) -> Result<&Self> {
        match self {
            Self::Chrome(page) => {
                page.goto(url.as_ref()).await?;
            }
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => session.navigate(url.as_ref()).await?,
        }
        Ok(self)
    }

    pub async fn reload(&self) -> Result<&Self> {
        match self {
            Self::Chrome(page) => {
                page.reload().await?;
            }
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => session.reload().await?,
        }
        Ok(self)
    }

    pub async fn evaluate_expression(
        &self,
        params: impl Into<EvaluateParams>,
    ) -> Result<EvaluationResult> {
        let params = params.into();
        match self {
            Self::Chrome(page) => Ok(page.evaluate_expression(params).await?),
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => {
                if params.context_id.is_some() {
                    bail!("WebKit 子框架尚未接入")
                }
                let arguments = serde_json::json!({"expression": params.expression});
                let mut object: RemoteObject = webkit_dom(session, "evaluate", arguments).await?;
                if object.object_id.is_none() && object.value.is_none() {
                    object.value = Some(Value::Null);
                }
                Ok(EvaluationResult::new(object))
            }
        }
    }

    pub async fn evaluate(&self, params: impl Into<Evaluation>) -> Result<EvaluationResult> {
        let params = params.into();
        match self {
            Self::Chrome(page) => Ok(page.evaluate(params).await?),
            #[cfg(target_os = "macos")]
            Self::WebKit { .. } => match params {
                Evaluation::Expression(expression) => self.evaluate_expression(expression).await,
                Evaluation::Function(function) => {
                    self.evaluate_expression(format!("({})()", function.function_declaration))
                        .await
                }
            },
        }
    }

    pub async fn set_content(&self, html: impl AsRef<str>) -> Result<&Self> {
        match self {
            Self::Chrome(page) => {
                page.set_content(html.as_ref()).await?;
            }
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => {
                let html = serde_json::to_string(html.as_ref())?;
                session
                    .evaluate::<Value>(&format!(
                        "document.open();document.write({html});document.close();null"
                    ))
                    .await?;
            }
        }
        Ok(self)
    }

    pub async fn click(&self, point: Point) -> Result<&Self> {
        match self {
            Self::Chrome(page) => {
                page.click(point).await?;
            }
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => session.click(point.x, point.y).await?,
        }
        Ok(self)
    }

    pub async fn move_mouse(&self, point: Point) -> Result<&Self> {
        match self {
            Self::Chrome(page) => {
                page.move_mouse(point).await?;
            }
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => session.move_mouse(point.x, point.y).await?,
        }
        Ok(self)
    }

    pub async fn screenshot(&self, params: impl Into<ScreenshotParams>) -> Result<Vec<u8>> {
        let params = params.into();
        match self {
            Self::Chrome(page) => Ok(page.screenshot(params).await?),
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => {
                let clip = params
                    .cdp_params
                    .clip
                    .map(|clip| [clip.x, clip.y, clip.width, clip.height]);
                session.screenshot(clip).await
            }
        }
    }

    pub async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
        match self {
            Self::Chrome(page) => Ok(page.execute(cmd).await?),
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => {
                let method = cmd.identifier();
                let arguments = serde_json::to_value(&cmd)?;
                let result = match method.as_ref() {
                    "Input.insertText" => {
                        let text = arguments
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("缺少网页输入文字"))?;
                        session.insert_text(text).await?;
                        serde_json::json!({})
                    }
                    "DOM.describeNode" => webkit_dom(session, "describe", arguments).await?,
                    "DOM.resolveNode" => webkit_dom(session, "resolve", arguments).await?,
                    "Runtime.callFunctionOn" => webkit_dom(session, "call", arguments).await?,
                    "Runtime.releaseObject" => webkit_dom(session, "release", arguments).await?,
                    "DOM.setFileInputFiles" => {
                        let id = arguments
                            .get("objectId")
                            .and_then(Value::as_str)
                            .and_then(|value| value.parse::<u32>().ok())
                            .or_else(|| {
                                arguments
                                    .get("backendNodeId")
                                    .and_then(Value::as_u64)
                                    .and_then(|value| u32::try_from(value).ok())
                            })
                            .ok_or_else(|| anyhow::anyhow!("无法识别 WebKit 文件输入框"))?;
                        let paths: Vec<std::path::PathBuf> =
                            serde_json::from_value(arguments["files"].clone())?;
                        session
                            .choose_files(&format!("globalThis.__airsDomRefs?.get({id})"), &paths)
                            .await?;
                        serde_json::json!({})
                    }
                    _ => bail!("WebKit 尚未支持此操作：{method}"),
                };
                Ok(CommandResponse {
                    id: chromiumoxide::types::CallId::new(0),
                    result: serde_json::from_value(result)?,
                    method,
                })
            }
        }
    }

    pub async fn find_element(&self, selector: impl Into<String>) -> Result<Element> {
        let selector = selector.into();
        match self {
            Self::Chrome(page) => {
                let element = page.find_element(selector).await?;
                Ok(Element {
                    backend_node_id: element.backend_node_id,
                    inner: ElementInner::Chrome(element),
                })
            }
            #[cfg(target_os = "macos")]
            Self::WebKit { session, .. } => {
                let selector = serde_json::to_string(&selector)?;
                let found: bool = session
                    .evaluate(&format!("!!document.querySelector({selector})"))
                    .await?;
                anyhow::ensure!(found, "网页控件不存在");
                Ok(Element {
                    // Only the Chrome file handoff reads backend_node_id. WebKit uses
                    // the selected input expression rather than a protocol node ID.
                    backend_node_id: serde_json::from_value(serde_json::json!(0))?,
                    inner: ElementInner::WebKit {
                        session: session.clone(),
                        selector,
                    },
                })
            }
        }
    }

    pub async fn event_listener<T: IntoEventKind>(&self) -> Result<EventStream<T>> {
        match self {
            Self::Chrome(page) => Ok(page.event_listener::<T>().await?),
            #[cfg(target_os = "macos")]
            Self::WebKit { .. } => bail!("WebKit 不提供 Chrome 事件监听接口"),
        }
    }

    pub async fn frame_execution_context(
        &self,
        frame: FrameId,
    ) -> Result<Option<ExecutionContextId>> {
        match self {
            Self::Chrome(page) => Ok(page.frame_execution_context(frame).await?),
            #[cfg(target_os = "macos")]
            Self::WebKit { .. } => Ok(None),
        }
    }
}

pub struct Element {
    pub backend_node_id: BackendNodeId,
    inner: ElementInner,
}

enum ElementInner {
    Chrome(chromiumoxide::Element),
    #[cfg(target_os = "macos")]
    WebKit {
        session: Arc<crate::webkit::Session>,
        selector: String,
    },
}

impl Element {
    pub async fn press_key(&self, key: &str) -> Result<()> {
        match &self.inner {
            ElementInner::Chrome(element) => {
                element.press_key(key).await?;
            }
            #[cfg(target_os = "macos")]
            ElementInner::WebKit { session, selector } => {
                session
                    .evaluate::<Value>(&format!("document.querySelector({selector})?.focus();null"))
                    .await?;
                session.press_key(key).await?;
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
async fn webkit_dom<T: serde::de::DeserializeOwned>(
    session: &crate::webkit::Session,
    action: &str,
    arguments: Value,
) -> Result<T> {
    let action = serde_json::to_string(action)?;
    session
        .evaluate(&format!(
            "({})({action}, {arguments})",
            include_str!("webkit_dom.js")
        ))
        .await
}
