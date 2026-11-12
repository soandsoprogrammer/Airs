use crate::douyin::read_verification_frame;
use crate::sign_in::{PublishSmsPreview, SignInGuard, SignInStatus, SignIns, VerificationAction};
use chromiumoxide::{Page, cdp::js_protocol::runtime::EvaluateParams};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{future::pending, time::Duration};

const CREATOR_ORIGIN: &str = "https://creator.douyin.com";

const SMS_PANEL: &str = r#"async (origin, action) => {
  const visible = (element) => !!element && element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility === 'visible' &&
    !element.closest('[hidden], [aria-hidden="true"], [inert]') &&
    (element.checkVisibility?.({ opacityProperty: true, visibilityProperty: true }) ?? true);
  const unique = (container, selector, matches = visible) => {
    const candidates = [...container.querySelectorAll(selector)].filter(matches);
    return candidates.length === 1 ? candidates[0] : null;
  };
  const findPanel = () => {
    if (location.origin !== origin) return null;
    const path = location.pathname.replace(/\/$/, '');
    const video = ['/creator-micro/content/publish', '/creator-micro/content/post/video'].includes(path);
    const query = new URLSearchParams(location.search);
    const photo = ['/creator-micro/content/post/image', '/creator-micro/content/publish-media/image-text'].includes(path) &&
      query.get('type') === 'new' && query.get('media_type') === 'image';
    if (!video && !photo) return null;
    return unique(document, '.second-verify-panel article.uc-ui-verify_sms-verify');
  };
  const enabled = (element) => visible(element) &&
    !element.closest(':disabled, [disabled], [aria-disabled="true"], .disabled') &&
    getComputedStyle(element).pointerEvents !== 'none';
  const label = (element) => (element?.innerText ?? '').trim();
  const requestLabel = /^(获取验证码|重新获取(?:验证码)?|重新发送)$/;
  const verifyLabel = /^(验证|确认验证)$/;
  const codeInput = 'input[placeholder*="验证码"]';
  const requestCode = '.uc-ui-verify_sms-verify_input .uc-ui-input_right';
  const verifyButton = '.uc-ui-verify_sms-verify_button';
  const supportsText = (element) => ['text', 'tel', 'number', 'password'].includes(element.type);
  const editable = (element) => enabled(element) && !element.readOnly && supportsText(element);
  const codeField = (panel) => unique(panel, 'input[placeholder="请输入验证码"]',
    (element) => visible(element) && supportsText(element));
  const confirmButton = (panel) => unique(panel, verifyButton,
    (element) => visible(element) && verifyLabel.test(label(element)));
  const requestButton = (panel) => unique(panel, requestCode + ' > .uc-ui-typography_description');
  const panel = findPanel();
  if (!panel) return action === null ? null : false;

  if (action === null) {
    const field = codeField(panel);
    const confirm = confirmButton(panel);
    const errors = [...panel.querySelectorAll('.uc-ui-verify_error')].filter(visible).map(label);
    const feedback = errors.some((text) => /(?:过于频繁|请求频繁)/.test(text)) ? 'rate_limited' :
      errors.some((text) => /验证码.{0,8}过期/.test(text)) ? 'code_expired' :
      errors.some((text) => /验证码.{0,8}(?:错误|不正确|无效)/.test(text)) ? 'code_invalid' :
      errors.some((text) => /(?:获取|发送)失败/.test(text)) ? 'request_failed' : null;
    if (!field || !confirm) return { native: null, feedback };
    const request = requestButton(panel);
    const requestText = label(request ?? unique(panel, requestCode));
    const countdown = requestText.match(/^(?:重新获取(?:验证码)?\s*[(（]\s*)?(\d{1,3})\s*(?:[sS]|秒)(?:\s*[)）]|后重新获取(?:验证码)?)?$/);
    return { native: {
      can_request_code: enabled(request) && requestLabel.test(requestText),
      can_verify: editable(field),
      resend_seconds: countdown && Number(countdown[1]) > 0 ? Number(countdown[1]) : null
    }, feedback };
  }
  if (action.request) {
    const request = requestButton(panel);
    if (!enabled(request) || !requestLabel.test(label(request))) return false;
    request.click();
    return true;
  }
  if (action.verify) {
    const field = codeField(panel);
    if (!editable(field) || !confirmButton(panel)) return false;
    field.focus({ preventScroll: true });
    if (findPanel() !== panel || codeField(panel) !== field || !editable(field)) return false;
    if (field.value !== action.verify) {
      field.select();
      document.execCommand('insertText', false, action.verify);
    }
    // Some versions enable Verify on the next render after the input event.
    for (let attempt = 0; attempt < 5; attempt++) {
      if (findPanel() !== panel || codeField(panel)?.value !== action.verify) return false;
      const confirm = confirmButton(panel);
      if (enabled(confirm)) {
        confirm.click();
        return true;
      }
      if (attempt < 4) await new Promise((resolve) => setTimeout(resolve, 75));
    }
    return false;
  }
  if (action.click) {
    const { x, y, width, height } = action.click;
    if (innerWidth !== width || innerHeight !== height) return false;
    const hit = document.elementFromPoint(x, y);
    const control = hit?.closest(codeInput + ', ' + verifyButton + ', ' + requestCode);
    if (!control || !panel.contains(control) || !enabled(control)) return false;
    if (control.matches(codeInput)) {
      control.focus({ preventScroll: true });
    } else {
      const requesting = control.matches(requestCode);
      const target = (requesting && unique(control, ':scope > .uc-ui-typography_description')) || control;
      const text = label(target);
      const allowed = requesting
        ? requestLabel.test(text)
        : verifyLabel.test(text) || text === '取消';
      if (!allowed || !enabled(target)) return false;
      // Keep the hit test and activation in this document so a disappearing
      // dialog cannot send a coordinate click to the main publish button.
      target.click();
    }
    return true;
  }

  const field = document.activeElement;
  if (!field?.matches(codeInput) || !panel.contains(field) || !editable(field)) return false;
  field.select();
  return document.execCommand('insertText', false, action.text);
}"#;

#[derive(serde::Deserialize)]
struct SmsPanel {
    native: Option<PublishSmsPreview>,
    feedback: Option<SmsFeedback>,
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum SmsFeedback {
    RateLimited,
    CodeExpired,
    CodeInvalid,
    RequestFailed,
}

impl SmsFeedback {
    fn message(self) -> &'static str {
        match self {
            Self::RateLimited => "抖音提示操作过于频繁，请稍后再试。",
            Self::CodeExpired => "抖音提示验证码已过期，请重新获取。",
            Self::CodeInvalid => "抖音提示验证码不正确，请核对后再试。",
            Self::RequestFailed => "抖音未能发送验证码，请稍后再试。",
        }
    }
}

async fn evaluate_sms<T: DeserializeOwned>(page: &Page, origin: &str, action: Value) -> Option<T> {
    let params = EvaluateParams::builder()
        .expression(format!("({SMS_PANEL})({origin:?}, {action})"))
        .await_promise(true)
        .return_by_value(true)
        .build()
        .ok()?;
    page.evaluate_expression(params)
        .await
        .ok()?
        .into_value()
        .ok()
}

async fn sms_panel(page: &Page, origin: &str) -> Option<SmsPanel> {
    evaluate_sms::<Option<SmsPanel>>(page, origin, Value::Null)
        .await
        .flatten()
}

async fn sms_action(page: &Page, origin: &str, action: Value) -> bool {
    evaluate_sms(page, origin, action).await.unwrap_or(false)
}

/// Display the official SMS panel, and relay only user actions within that panel.
/// The caller runs this after one publish click and drops it when receipt waiting ends.
pub async fn relay_publish_verification(page: &Page, sign_ins: &SignIns, account_id: &str) {
    let mut active: Option<SignInGuard> = None;
    let mut viewport = None;
    let mut missing_once = false;
    let mut had_platform_feedback = false;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Some(panel) = sms_panel(page, CREATOR_ORIGIN).await {
                    missing_once = false;
                    let preview = active.get_or_insert_with(|| {
                        let preview = sign_ins.begin(account_id);
                        preview.update(SignInStatus::VerifyInApp, None);
                        preview
                    });
                    if panel.feedback.is_some() || had_platform_feedback {
                        preview.set_input_feedback(panel.feedback.map(SmsFeedback::message));
                    }
                    had_platform_feedback = panel.feedback.is_some();
                    if let Some(sms) = panel.native {
                        preview.update_publish_sms(sms);
                        viewport = None;
                    } else if let Ok((png, width, height)) = read_verification_frame(page).await
                        && sms_panel(page, CREATOR_ORIGIN).await.is_some_and(|panel| panel.native.is_none())
                    {
                        preview.update_verification(png, width, height);
                        viewport = Some((width, height));
                    }
                } else if missing_once {
                    active = None;
                    viewport = None;
                    had_platform_feedback = false;
                } else {
                    // The platform can briefly redraw the panel during verification.
                    missing_once = true;
                }
            }
            Some(action) = async {
                match &active {
                    Some(preview) => preview.verification_action().await,
                    None => pending().await,
                }
            } => {
                if let Some(preview) = &active {
                    had_platform_feedback = false;
                    preview.set_input_feedback(
                        forward_sms_action(page, CREATOR_ORIGIN, viewport, action).await
                    );
                    ticker.reset_immediately();
                }
            }
        }
    }
}

async fn forward_sms_action(
    page: &Page,
    origin: &str,
    viewport: Option<(u32, u32)>,
    action: VerificationAction,
) -> Option<&'static str> {
    match action {
        VerificationAction::Click { x, y }
            if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) =>
        {
            let (width, height) = viewport?;
            let action = json!({"click": {
                "x": x * f64::from(width),
                "y": y * f64::from(height),
                "width": width,
                "height": height,
            }});
            (!sms_action(page, origin, action).await)
                .then_some("请点击抖音验证码输入框或验证按钮。")
        }
        VerificationAction::FillText(text) | VerificationAction::VerifySmsCode(text)
            if !(4..=8).contains(&text.len())
                || !text.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Some("请检查要填写的验证码。")
        }
        VerificationAction::FillText(text) => {
            Some(if sms_action(page, origin, json!({"text": text})).await {
                "已填入验证码，请在抖音验证页面核对并确认。"
            } else {
                "请先点击抖音页面中的验证码输入框，再点「填入网页」。"
            })
        }
        VerificationAction::RequestSmsCode => Some(
            if sms_action(page, origin, json!({"request": true})).await {
                "已向抖音请求短信验证码。"
            } else {
                "暂时无法请求验证码，请稍后再试。"
            },
        ),
        VerificationAction::VerifySmsCode(code) => {
            Some(if sms_action(page, origin, json!({"verify": code})).await {
                "已向抖音提交验证码，正在等待平台结果。"
            } else {
                "未能提交验证码，请检查后再试。"
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{BrowserSession, truth};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "需要本机 Chrome"]
    async fn publish_sms_actions_are_manual_and_cannot_reach_the_main_publish_button()
    -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            const HTML: &str = "<!doctype html><meta charset=utf-8><body></body>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{HTML}",
                HTML.len()
            );
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0; 1024];
                if stream.read(&mut request).await.is_ok() {
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        let profile = tempfile::tempdir()?;
        let mut browser = BrowserSession::launch(profile.path()).await?;
        let page = browser
            .browser
            .new_page(format!("{origin}/creator-micro/content/post/video"))
            .await?;
        page.set_content(
            r#"
            <button id="publish" style="position:fixed;left:24px;top:24px;width:160px;height:40px">发布</button>
            <div class="second-verify-panel">
              <article class="uc-ui-verify_sms-verify" style="position:fixed;inset:12px auto auto 12px;width:210px;height:180px;background:white">
                <input id="code" placeholder="请输入验证码" style="position:fixed;left:24px;top:24px;width:160px;height:40px">
                <div class="uc-ui-verify_sms-verify_input">
                  <div id="get" class="uc-ui-input_right" style="position:fixed;left:24px;top:80px;width:160px;height:40px"><span class="uc-ui-typography_description">获取验证码</span></div>
                </div>
                <div id="verify" class="uc-ui-verify_sms-verify_button primary default uc-ui-button disabled" style="position:fixed;left:24px;top:135px;width:160px;height:40px">验证</div>
                <span class="uc-ui-verify_error" hidden></span>
              </article>
            </div>
            <script>
              window.publishes = 0;
              window.codeRequests = 0;
              window.verifications = 0;
              document.querySelector('#publish').onclick = () => window.publishes++;
              document.querySelector('#get span').onclick = () => {
                window.codeRequests++;
                document.querySelector('#get span').textContent = '59s';
              };
              document.querySelector('#code').oninput = () => setTimeout(() => {
                document.querySelector('#verify').classList.remove('disabled');
              }, 50);
              document.querySelector('#verify').onclick = () => {
                window.verifications++;
                window.confirmed = document.querySelector('#code').value;
              };
            </script>
            "#,
        )
        .await?;
        assert!(sms_panel(&page, CREATOR_ORIGIN).await.is_none());
        let native = sms_panel(&page, &origin)
            .await
            .and_then(|panel| panel.native);
        assert_eq!(
            native,
            Some(PublishSmsPreview {
                can_request_code: true,
                can_verify: true,
                resend_seconds: None,
            })
        );
        assert!(
            truth(
                &page,
                "window.codeRequests === 0 && window.verifications === 0"
            )
            .await?
        );
        let request = VerificationAction::RequestSmsCode;
        assert_eq!(
            forward_sms_action(&page, &origin, None, request).await,
            Some("已向抖音请求短信验证码。")
        );
        let native = sms_panel(&page, &origin)
            .await
            .and_then(|panel| panel.native);
        assert_eq!(
            native,
            Some(PublishSmsPreview {
                can_request_code: false,
                can_verify: true,
                resend_seconds: Some(59),
            })
        );
        let request = VerificationAction::RequestSmsCode;
        assert_eq!(
            forward_sms_action(&page, &origin, None, request).await,
            Some("暂时无法请求验证码，请稍后再试。")
        );
        page.evaluate_expression("document.querySelector('#get span').textContent = '重新发送'")
            .await?;
        let native = sms_panel(&page, &origin)
            .await
            .and_then(|panel| panel.native);
        assert_eq!(
            native,
            Some(PublishSmsPreview {
                can_request_code: true,
                can_verify: true,
                resend_seconds: None,
            })
        );
        assert!(truth(&page, "window.codeRequests === 1").await?);
        assert_eq!(
            forward_sms_action(&page, &origin, None, VerificationAction::RequestSmsCode).await,
            Some("已向抖音请求短信验证码。")
        );
        assert!(
            sms_panel(&page, &origin)
                .await
                .and_then(|panel| panel.native)
                .is_some_and(|sms| !sms.can_request_code && sms.resend_seconds == Some(59))
        );
        let verify = VerificationAction::VerifySmsCode("123456".into());
        assert_eq!(
            forward_sms_action(&page, &origin, None, verify).await,
            Some("已向抖音提交验证码，正在等待平台结果。")
        );
        assert!(truth(&page, "window.codeRequests === 2 && window.verifications === 1 && window.confirmed === '123456'").await?);
        page.evaluate_expression(
            "(() => { const error = document.querySelector('.uc-ui-verify_error'); error.hidden = false; error.textContent = '验证码不正确：123456'; })()",
        )
        .await?;
        let feedback = sms_panel(&page, &origin)
            .await
            .and_then(|panel| panel.feedback)
            .map(SmsFeedback::message);
        assert_eq!(feedback, Some("抖音提示验证码不正确，请核对后再试。"));
        let viewport: (u32, u32) = page
            .evaluate_expression("[innerWidth, innerHeight]")
            .await?
            .into_value()?;
        let click = |y: f64| VerificationAction::Click {
            x: 40. / f64::from(viewport.0),
            y: y / f64::from(viewport.1),
        };
        // An alternate platform field can still use the screenshot fallback.
        page.evaluate("document.querySelector('#code').placeholder = '输入验证码'")
            .await?;
        assert!(
            sms_panel(&page, &origin)
                .await
                .is_some_and(|panel| panel.native.is_none())
        );
        page.evaluate_expression("document.querySelector('#get span').textContent = '重新发送'")
            .await?;
        assert_eq!(
            forward_sms_action(&page, &origin, Some(viewport), click(100.)).await,
            None
        );
        assert_eq!(
            forward_sms_action(&page, &origin, Some(viewport), click(42.)).await,
            None
        );
        let fill = VerificationAction::FillText("654321".into());
        assert_eq!(
            forward_sms_action(&page, &origin, Some(viewport), fill).await,
            Some("已填入验证码，请在抖音验证页面核对并确认。")
        );
        assert!(
            truth(
                &page,
                "window.codeRequests === 3 && window.verifications === 1 && document.querySelector('#code').value === '654321'"
            )
            .await?
        );
        assert_eq!(
            forward_sms_action(&page, &origin, Some(viewport), click(150.)).await,
            None
        );
        assert!(
            truth(
                &page,
                "window.verifications === 2 && window.confirmed === '654321'"
            )
            .await?
        );
        page.evaluate("document.querySelector('.second-verify-panel').remove()")
            .await?;
        assert!(
            forward_sms_action(&page, &origin, Some(viewport), click(42.))
                .await
                .is_some()
        );
        let verify = VerificationAction::VerifySmsCode("123456".into());
        assert_eq!(
            forward_sms_action(&page, &origin, None, verify).await,
            Some("未能提交验证码，请检查后再试。")
        );
        assert!(truth(&page, "window.publishes === 0").await?);
        browser.close().await?;
        server.abort();
        Ok(())
    }
}
