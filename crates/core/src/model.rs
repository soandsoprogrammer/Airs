use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    Douyin,
    Kuaishou,
    Xiaohongshu,
    Simulation,
}
impl Platform {
    pub const ALL: [Self; 4] = [
        Self::Douyin,
        Self::Kuaishou,
        Self::Xiaohongshu,
        Self::Simulation,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Douyin => "抖音",
            Self::Kuaishou => "快手",
            Self::Xiaohongshu => "小红书",
            Self::Simulation => "模拟测试",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Simulation => "模拟测试（不会发布）",
            _ => self.name(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Unchecked,
    Ready,
    NeedsLogin,
}
impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "尚未检查",
            Self::Ready => "登录状态已验证",
            Self::NeedsLogin => "需要登录",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub alias: String,
    pub platform: Platform,
    pub session: SessionStatus,
    pub checked_at: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default = "preserve_existing_alias")]
    pub alias_customized: bool,
}
fn preserve_existing_alias() -> bool {
    true
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified_ms: u128,
    #[serde(default)]
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageFile>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    #[default]
    Video,
    Images,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ms: u128,
}

impl Asset {
    pub fn inspect(path: &Path) -> Result<Self> {
        let (path, size, modified_ms) = inspect_file(path, MediaKind::Video)?;
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            path,
            size,
            modified_ms,
            kind: MediaKind::Video,
            images: Vec::new(),
        })
    }

    pub fn inspect_images(paths: &[PathBuf]) -> Result<Self> {
        ensure!(!paths.is_empty(), "请选择至少一张图片");
        ensure!(paths.len() <= 35, "每组最多导入 35 张图片");
        let mut seen = std::collections::HashSet::with_capacity(paths.len());
        let mut images = Vec::with_capacity(paths.len());
        let mut size = 0_u64;
        for path in paths {
            let (path, bytes, modified_ms) = inspect_file(path, MediaKind::Images)?;
            ensure!(
                seen.insert(path.clone()),
                "同一组图文不能重复添加同一张图片"
            );
            size = size
                .checked_add(bytes)
                .ok_or_else(|| anyhow::anyhow!("图片组过大"))?;
            images.push(ImageFile {
                path,
                size: bytes,
                modified_ms,
            });
        }
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            path: images[0].path.clone(),
            size,
            modified_ms: images[0].modified_ms,
            kind: MediaKind::Images,
            images,
        })
    }

    pub fn verify(&self) -> Result<()> {
        match self.kind {
            MediaKind::Video => {
                let (_, size, modified_ms) = inspect_file(&self.path, self.kind)?;
                ensure!(
                    size == self.size && modified_ms == self.modified_ms,
                    "素材在导入后已改变，请重新导入"
                );
            }
            MediaKind::Images => {
                ensure!(!self.images.is_empty(), "请选择至少一张图片");
                for image in &self.images {
                    let (_, size, modified_ms) =
                        inspect_file(&image.path, self.kind).map_err(|error| {
                            anyhow::anyhow!(
                                "图片 {}：{error}",
                                image.path.file_name().unwrap_or_default().to_string_lossy()
                            )
                        })?;
                    ensure!(
                        size == image.size && modified_ms == image.modified_ms,
                        "图片 {} 在导入后已改变，请重新导入",
                        image.path.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }
        }
        Ok(())
    }
}

fn inspect_file(path: &Path, kind: MediaKind) -> Result<(PathBuf, u64, u128)> {
    let path = path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("素材不存在或无法访问"))?;
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match kind {
        MediaKind::Video => ensure!(
            ["mp4", "mov", "m4v", "webm"].contains(&ext.as_str()),
            "请选择 MP4、MOV、M4V 或 WebM 视频"
        ),
        MediaKind::Images => ensure!(
            ["jpg", "jpeg", "png", "webp"].contains(&ext.as_str()),
            "请选择 JPG、PNG 或 WebP 图片"
        ),
    }
    let metadata = path.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "素材必须是非空文件"
    );
    let modified_ms = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    Ok((path, metadata.len(), modified_ms))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    pub asset: Asset,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
}
impl Content {
    pub fn title_limit(platform: Platform, kind: MediaKind) -> usize {
        match (platform, kind) {
            (Platform::Xiaohongshu, _) | (Platform::Douyin, MediaKind::Images) => 20,
            _ => 30,
        }
    }

    pub fn title_length(title: &str, platform: Platform, kind: MediaKind) -> usize {
        if platform == Platform::Xiaohongshu {
            title
                .encode_utf16()
                .map(|unit| {
                    if unit < 128 && unit != u16::from(b'^') {
                        1
                    } else {
                        2
                    }
                })
                .sum::<usize>()
                .div_ceil(2)
        } else if platform == Platform::Douyin && kind == MediaKind::Images {
            title.encode_utf16().count()
        } else {
            title.chars().count()
        }
    }

    pub fn validate_for_platform(&self, platform: Platform) -> Result<()> {
        let limit = Self::title_limit(platform, self.asset.kind);
        ensure!(
            Self::title_length(self.title.trim(), platform, self.asset.kind) <= limit,
            "{}标题最多 {limit} 字",
            platform.name()
        );
        if platform == Platform::Xiaohongshu {
            let body_units = self
                .description
                .trim()
                .encode_utf16()
                .filter(|&unit| unit != u16::from(b'\n') && unit != u16::from(b'\r'))
                .count();
            let tag_units = self
                .tags
                .iter()
                .map(|tag| tag.encode_utf16().count() + 1)
                .sum::<usize>()
                + self.tags.len().saturating_sub(1);
            ensure!(
                body_units + tag_units <= 1000,
                "小红书正文和标签合计最多 1000 字"
            );
        }
        if self.asset.kind == MediaKind::Images {
            if platform == Platform::Douyin {
                let body_units = self.description.encode_utf16().count()
                    + self
                        .tags
                        .iter()
                        .map(|tag| tag.encode_utf16().count() + 2)
                        .sum::<usize>();
                ensure!(body_units <= 1000, "抖音图文描述和标签合计最多 1000 字");
            }
            const MIB: u64 = 1024 * 1024;
            let (max_images, max_bytes, size_label) = match platform {
                Platform::Douyin => (35, 50 * MIB, "最多 50 MB"),
                Platform::Kuaishou => (31, 15 * MIB - 1, "须小于 15 MB"),
                Platform::Xiaohongshu => (18, 32 * MIB, "最多 32 MB"),
                Platform::Simulation => return Ok(()),
            };
            ensure!(
                self.asset.images.len() <= max_images,
                "{}图文最多支持 {max_images} 张图片",
                platform.name()
            );
            if let Some(image) = self
                .asset
                .images
                .iter()
                .find(|image| image.size > max_bytes)
            {
                bail!(
                    "{}的单张图片{size_label}，请缩小「{}」",
                    platform.name(),
                    image.path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.asset.verify()?;
        ensure!(!self.title.trim().is_empty(), "请填写标题");
        ensure!(self.title.chars().count() <= 40, "标题过长，请缩短后再发布");
        ensure!(self.tags.len() <= 5, "当前最多支持 5 个标签");
        ensure!(
            self.tags
                .iter()
                .all(|s| !s.is_empty() && s.chars().all(|c| !c.is_whitespace() && c != '#')),
            "标签不能包含空格或 #"
        );
        ensure!(
            self.description.chars().count()
                + self
                    .tags
                    .iter()
                    .map(|x| x.chars().count() + 2)
                    .sum::<usize>()
                <= 1000,
            "描述和标签合计不能超过 1000 字符"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Draft,
    Queued,
    Preparing,
    Uploading,
    WaitingUser,
    Submitting,
    Submitted,
    Failed,
    Unknown,
    Cancelled,
}
impl JobState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "草稿",
            Self::Queued => "排队中",
            Self::Preparing => "准备中",
            Self::Uploading => "上传中",
            Self::WaitingUser => "等待操作",
            Self::Submitting => "提交中",
            Self::Submitted => "平台已接收",
            Self::Failed => "失败",
            Self::Unknown => "结果待确认",
            Self::Cancelled => "已停止",
        }
    }
    pub fn can_transition(self, next: Self) -> bool {
        use JobState::*;
        matches!(
            (self, next),
            (Draft, Queued)
                | (Queued, Preparing)
                | (Queued, Cancelled)
                | (Preparing, Uploading)
                | (Preparing, WaitingUser)
                | (Preparing, Failed)
                | (Preparing, Cancelled)
                | (Uploading, WaitingUser)
                | (Uploading, Failed)
                | (Uploading, Cancelled)
                | (WaitingUser, Uploading)
                | (WaitingUser, Submitting)
                | (WaitingUser, Failed)
                | (WaitingUser, Cancelled)
                | (Submitting, Submitted)
                | (Submitting, Unknown)
                | (Submitting, Failed)
                | (Unknown, Submitted)
                | (Failed, Queued)
                | (Cancelled, Queued)
        )
    }
    pub fn check(self, next: Self) -> Result<()> {
        if !self.can_transition(next) {
            bail!("不允许的任务状态变更：{self:?} → {next:?}")
        }
        Ok(())
    }
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Uploading | Self::WaitingUser | Self::Submitting
        )
    }
    pub fn can_delete(self) -> bool {
        matches!(self, Self::Submitted | Self::Failed | Self::Cancelled)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub batch_id: String,
    pub account: Account,
    pub content: Content,
    pub state: JobState,
    pub message: String,
    pub created_at: String,
    pub result: Option<Receipt>,
    #[serde(default)]
    pub ready_to_submit: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub platform_id: String,
    pub evidence: String,
    pub url: Option<String>,
}
pub const XIAOHONGSHU_ACCEPTED_EVIDENCE: &str = "小红书创作服务平台确认已接收笔记";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub time: String,
    pub state: JobState,
    pub message: String,
}
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub name: &'static str,
    pub verified: bool,
    pub separate_prepare: bool,
    pub custom_cover: bool,
    pub submit_enabled: bool,
    pub image_posts: bool,
}
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// A deliberately authored explanation safe to show in the UI and store in logs.
#[derive(Debug)]
pub struct UserError(pub String);
impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UserError {}
