//! macOS system WebKit sessions. The native host owns the WKWebView and
//! associated website data store; its only control channel is the child pipe.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

pub fn host_path() -> Result<PathBuf> {
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let bundled = parent.join("airs-webkit-host");
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    let developer = PathBuf::from(env!("AIRS_WEBKIT_HOST_BUILD"));
    ensure!(developer.is_file(), "Airs 的 WebKit 组件缺失，请重新安装");
    Ok(developer)
}

struct Connection {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    sequence: u64,
}

impl Connection {
    async fn request(&mut self, action: &str, arguments: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        let mut command = arguments.as_object().cloned().unwrap_or_default();
        command.insert("id".into(), json!(id));
        command.insert("action".into(), json!(action));
        let mut bytes = serde_json::to_vec(&command)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        loop {
            let line = self
                .output
                .next_line()
                .await?
                .context("WebKit 会话已关闭")?;
            let result: Value = serde_json::from_str(&line)?;
            if result.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = result.get("error").and_then(Value::as_str) {
                if action == "selectFiles" && error == "Unexpected file input" {
                    bail!("文件选择入口不匹配");
                }
                bail!("WebKit 操作失败（{action}）");
            }
            return Ok(result.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ObservedResponse {
    pub endpoint: String,
    pub document: String,
    pub status: u16,
    pub request: Option<String>,
    pub body: String,
}

pub struct Session {
    connection: Mutex<Connection>,
}

impl Session {
    pub async fn launch(profile_id: uuid::Uuid, persistent: bool) -> Result<Self> {
        let mut child = Command::new(host_path()?)
            .arg(profile_id.to_string())
            .arg(if persistent { "persistent" } else { "private" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("无法启动 Airs 的系统 WebKit 组件")?;
        let input = child.stdin.take().context("无法打开 WebKit 请求通道")?;
        let output = child.stdout.take().context("无法打开 WebKit 响应通道")?;
        Ok(Self {
            connection: Mutex::new(Connection {
                child,
                input,
                output: BufReader::new(output).lines(),
                sequence: 0,
            }),
        })
    }

    async fn call<T: DeserializeOwned>(&self, action: &str, arguments: Value) -> Result<T> {
        let mut connection = self.connection.lock().await;
        let result =
            tokio::time::timeout(REQUEST_TIMEOUT, connection.request(action, arguments)).await;
        match result {
            Ok(result) => Ok(serde_json::from_value(result?)?),
            Err(_) => {
                let _ = connection.child.kill().await;
                bail!("WebKit 操作超时，会话已关闭（{action}）")
            }
        }
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.call::<bool>("navigate", json!({ "url": url })).await?;
        Ok(())
    }

    pub async fn reload(&self) -> Result<()> {
        self.call::<bool>("reload", json!({})).await?;
        Ok(())
    }

    pub async fn evaluate<T: DeserializeOwned>(&self, script: &str) -> Result<T> {
        self.call("eval", json!({ "source": script })).await
    }

    pub async fn evaluate_frame<T: DeserializeOwned>(
        &self,
        frame_url: &str,
        script: &str,
    ) -> Result<T> {
        self.call("eval", json!({ "source": script, "frameURL": frame_url }))
            .await
    }

    pub async fn evaluate_frame_at<T: DeserializeOwned>(
        &self,
        x: f64,
        y: f64,
        origin: &str,
        script: &str,
    ) -> Result<T> {
        self.call(
            "evalFrameAt",
            json!({ "x": x, "y": y, "origin": origin, "source": script }),
        )
        .await
    }

    pub async fn arm_receipts(&self, endpoints: &[String]) -> Result<()> {
        self.call::<bool>("armReceipts", json!({ "endpoints": endpoints }))
            .await?;
        Ok(())
    }

    pub async fn take_receipts(&self) -> Result<Vec<ObservedResponse>> {
        self.call("takeReceipts", json!({})).await
    }

    pub async fn choose_files(&self, expression: &str, paths: &[PathBuf]) -> Result<()> {
        self.choose_files_at(expression, paths, None).await
    }

    pub async fn choose_files_at(
        &self,
        expression: &str,
        paths: &[PathBuf],
        point: Option<[f64; 2]>,
    ) -> Result<()> {
        ensure!(!paths.is_empty(), "没有选择要上传的素材");
        for path in paths {
            ensure!(path.is_absolute() && path.is_file(), "素材文件无法读取");
        }
        self.call::<bool>(
            "selectFiles",
            json!({ "element": expression, "paths": paths, "clickPoint": point }),
        )
        .await?;
        Ok(())
    }

    pub async fn click(&self, x: f64, y: f64) -> Result<()> {
        self.call::<bool>("click", json!({ "x": x, "y": y }))
            .await?;
        Ok(())
    }

    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
        self.call::<bool>("moveMouse", json!({ "x": x, "y": y }))
            .await?;
        Ok(())
    }

    pub async fn press_key(&self, key: &str) -> Result<()> {
        self.call::<bool>("key", json!({ "key": key })).await?;
        Ok(())
    }

    pub async fn insert_text(&self, text: &str) -> Result<()> {
        let value = serde_json::to_string(text)?;
        ensure!(
            self.evaluate::<bool>(&format!(
                "document.execCommand('insertText', false, {value})"
            ))
            .await?,
            "网页输入框暂时无法填写"
        );
        Ok(())
    }

    pub(crate) async fn arm_douyin_work_list(
        &self,
        title: &str,
        description: &str,
        since: u64,
        until: u64,
    ) -> Result<()> {
        self.call::<bool>(
            "armDouyinWorkList",
            json!({
                "title": title, "description": description, "since": since, "until": until
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn show_douyin_login(&self) -> Result<()> {
        self.call::<bool>("showDouyinLogin", json!({})).await?;
        Ok(())
    }

    pub async fn hide_douyin_login(&self) -> Result<()> {
        self.call::<bool>("hideDouyinLogin", json!({})).await?;
        Ok(())
    }

    pub async fn screenshot(&self, clip: Option<[f64; 4]>) -> Result<Vec<u8>> {
        let clip = clip.map(|[x, y, width, height]| {
            json!({
                "x": x, "y": y, "width": width, "height": height
            })
        });
        let encoded: String = self.call("snapshot", json!({ "clip": clip })).await?;
        Ok(STANDARD.decode(encoded)?)
    }

    pub async fn close(&self) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            connection.request("close", json!({})),
        )
        .await;
        if tokio::time::timeout(Duration::from_secs(3), connection.child.wait())
            .await
            .is_err()
        {
            connection.child.kill().await?;
        }
        Ok(())
    }
}

pub async fn remove_account_profile(account_id: &str) -> Result<()> {
    remove_profile(uuid::Uuid::parse_str(account_id)?).await
}

pub async fn remove_profile(profile_id: uuid::Uuid) -> Result<()> {
    let result = tokio::time::timeout(
        REQUEST_TIMEOUT,
        Command::new(host_path()?)
            .arg(profile_id.to_string())
            .arg("remove")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        result.status.success(),
        "无法删除 WebKit 账号资料：{}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(())
}

pub async fn wait_response<T>(
    session: &Session,
    limit: Duration,
    verify: impl Fn(ObservedResponse) -> Option<T>,
) -> Result<Option<T>> {
    let wait = async {
        loop {
            let mut responses = session.take_receipts().await?;
            match responses.len() {
                0 => tokio::time::sleep(Duration::from_millis(200)).await,
                1 => return Ok(verify(responses.remove(0))),
                _ => return Ok(None),
            }
        }
    };
    match tokio::time::timeout(limit, wait).await {
        Ok(result) => result,
        Err(_) => Ok(None),
    }
}
