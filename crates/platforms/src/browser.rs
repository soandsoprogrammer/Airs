use anyhow::{Result, ensure};
use chromiumoxide::{Browser as ChromeBrowser, BrowserConfig, handler::viewport::Viewport};
mod page;
use futures::StreamExt;
pub use page::Page;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::task::JoinHandle;

pub fn uses_system_webkit() -> bool {
    cfg!(target_os = "macos")
}

pub fn chrome_path() -> PathBuf {
    std::env::var_os("PUBLISHER_CHROME")
        .map(PathBuf::from)
        .unwrap_or_else(default_chrome_path)
}

#[cfg(not(target_os = "windows"))]
fn default_chrome_path() -> PathBuf {
    PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
}

#[cfg(target_os = "windows")]
fn default_chrome_path() -> PathBuf {
    use chromiumoxide::detection::{DetectionOptions, default_executable};

    if let Ok(path) = default_executable(DetectionOptions {
        msedge: false,
        unstable: false,
    }) && path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("chrome.exe"))
    {
        return path;
    }

    const INSTALL_PATH: &str = r"Google\Chrome\Application\chrome.exe";
    [
        "ProgramW6432",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "LOCALAPPDATA",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .filter(|directory| !directory.is_empty())
    .map(PathBuf::from)
    .chain([
        PathBuf::from(r"C:\Program Files"),
        PathBuf::from(r"C:\Program Files (x86)"),
    ])
    .map(|directory| directory.join(INSTALL_PATH))
    .find(|path| path.is_file())
    .unwrap_or_else(|| PathBuf::from(r"C:\Program Files").join(INSTALL_PATH))
}

pub enum Browser {
    Chrome(Box<ChromeBrowser>),
    #[cfg(target_os = "macos")]
    WebKit {
        profile: uuid::Uuid,
        sessions: tokio::sync::Mutex<Vec<std::sync::Arc<crate::webkit::Session>>>,
    },
}

impl Browser {
    pub async fn new_page(&self, url: impl AsRef<str>) -> Result<Page> {
        match self {
            Self::Chrome(browser) => Ok(Page::Chrome(browser.new_page(url.as_ref()).await?)),
            #[cfg(target_os = "macos")]
            Self::WebKit { profile, sessions } => {
                let session =
                    std::sync::Arc::new(crate::webkit::Session::launch(*profile, true).await?);
                session.navigate(url.as_ref()).await?;
                sessions.lock().await.push(session.clone());
                Ok(Page::WebKit {
                    session,
                    target: serde_json::from_value(serde_json::json!(
                        uuid::Uuid::new_v4().to_string()
                    ))?,
                })
            }
        }
    }
}

pub struct BrowserSession {
    pub browser: Browser,
    handler: Option<JoinHandle<()>>,
}
impl BrowserSession {
    pub async fn launch(profile: &Path) -> Result<Self> {
        #[cfg(target_os = "macos")]
        if uses_system_webkit() {
            let profile_id = match profile
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| uuid::Uuid::parse_str(name).ok())
            {
                Some(id) => id,
                None => {
                    std::fs::create_dir_all(profile)?;
                    let id_path = profile.join("webkit-profile-id");
                    if id_path.is_file() {
                        uuid::Uuid::parse_str(std::fs::read_to_string(id_path)?.trim())?
                    } else {
                        let id = uuid::Uuid::new_v4();
                        std::fs::write(id_path, id.to_string())?;
                        id
                    }
                }
            };
            return Ok(Self {
                browser: Browser::WebKit {
                    profile: profile_id,
                    sessions: Default::default(),
                },
                handler: None,
            });
        }
        let path = profile
            .parent()
            .and_then(|p| p.parent())
            .and_then(|root| std::fs::read(root.join("settings.json")).ok())
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("browser").and_then(|x| x.as_str()).map(PathBuf::from))
            .unwrap_or_else(chrome_path);
        ensure!(path.is_file(), "未找到 Chrome，请在设置中选择浏览器程序");
        let config = BrowserConfig::builder()
            .chrome_executable(path)
            .user_data_dir(profile)
            .new_headless_mode()
            .viewport(Viewport {
                width: 1440,
                height: 1000,
                ..Default::default()
            })
            .request_timeout(Duration::from_secs(30))
            .arg("--no-first-run")
            .arg("--no-default-browser-check");
        let (browser, mut handler) = ChromeBrowser::launch(
            config
                .build()
                .map_err(|_| anyhow::anyhow!("浏览器配置无效"))?,
        )
        .await?;
        let handler = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            browser: Browser::Chrome(Box::new(browser)),
            handler: Some(handler),
        })
    }
    pub async fn close(&mut self) -> Result<()> {
        match &mut self.browser {
            Browser::Chrome(browser) => {
                let _ = tokio::time::timeout(Duration::from_secs(4), browser.close()).await;
                if tokio::time::timeout(Duration::from_secs(4), browser.wait())
                    .await
                    .is_err()
                {
                    let _ = browser.kill().await;
                }
            }
            #[cfg(target_os = "macos")]
            Browser::WebKit { sessions, .. } => {
                for session in sessions.get_mut().drain(..) {
                    session.close().await?;
                }
            }
        }
        if let Some(handler) = self.handler.take() {
            handler.abort();
        }
        Ok(())
    }
}
impl Drop for BrowserSession {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take() {
            handler.abort();
        }
    }
}
pub async fn truth(page: &Page, script: &str) -> Result<bool> {
    Ok(page.evaluate(script).await?.into_value::<bool>()?)
}
pub async fn upload_file(page: &Page, selector: &str, path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(session) = page.webkit() {
        let selector = serde_json::to_string(selector)?;
        return session
            .choose_files(
                &format!("document.querySelector({selector})"),
                &[path.to_owned()],
            )
            .await;
    }
    use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
    let el = page.find_element(selector).await?;
    page.execute(
        SetFileInputFilesParams::builder()
            .files(vec![path.to_string_lossy().into_owned()])
            .backend_node_id(el.backend_node_id)
            .build()
            .map_err(|_| anyhow::anyhow!("无法设置上传文件"))?,
    )
    .await?;
    Ok(())
}
pub async fn fill_fields(page: &Page, title: &str, description: &str) -> Result<()> {
    let description_text = description;
    let title = serde_json::to_string(title)?;
    let description = serde_json::to_string(description)?;
    let script = format!(
        r#"(()=>{{
 const input=document.querySelector('input[placeholder*="填写作品标题"]');
 const editor=document.querySelector('div.zone-container[contenteditable="true"]');
 if(!input||!editor) return false;
 Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,{title});
 input.dispatchEvent(new Event('input',{{bubbles:true}})); input.dispatchEvent(new Event('change',{{bubbles:true}}));
 const clean=(value)=>value.replace(/\u200b/g,'').trim();
 if(clean(editor.innerText)!==clean({description})) {{
   editor.focus();
   const range=document.createRange(); range.selectNodeContents(editor);
   const selection=getSelection();
   if(!selection) return false;
   selection.removeAllRanges(); selection.addRange(range);
 }}
 return true;
 }})()"#
    );
    ensure!(
        truth(page, &script).await?,
        "平台编辑区域已变化，无法确认标题和描述已填写"
    );
    let verification = format!(
        r#"(() => {{
          const input=document.querySelector('input[placeholder*="填写作品标题"]');
          const editor=document.querySelector('div.zone-container[contenteditable="true"]');
          const clean=value=>value.replace(/\u200b/g,'').trim();
          return !!input && !!editor && input.value==={title} &&
            clean(editor.innerText)===clean({description});
        }})()"#
    );
    if !truth(page, &verification).await? {
        if description_text.is_empty() {
            page.find_element("div.zone-container[contenteditable=\"true\"]")
                .await?
                .press_key("Backspace")
                .await?;
        } else {
            page.execute(
                chromiumoxide::cdp::browser_protocol::input::InsertTextParams::new(
                    description_text,
                ),
            )
            .await?;
        }
    }
    let verified = tokio::time::timeout(Duration::from_secs(2), async {
        while !truth(page, &verification).await? {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    ensure!(
        matches!(verified, Ok(Ok(()))),
        "平台编辑区域已变化，无法确认标题和描述已填写"
    );
    Ok(())
}
