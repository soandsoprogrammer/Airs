use crate::{Asset, MediaKind};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

const MAX_TEXT_FILE_BYTES: u64 = 64 * 1024;
const MAX_TEXT_SET_BYTES: usize = 128 * 1024;
const MAX_TEXT_FILES: usize = 12;
const IMPLIED_TITLE_LENGTH: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialSet {
    pub id: String,
    pub title: String,
    pub description: String,
    pub videos: Vec<Asset>,
    pub images: Option<Asset>,
    pub text_files: Vec<PathBuf>,
}

impl MaterialSet {
    pub fn assets(&self) -> impl Iterator<Item = &Asset> {
        self.videos.iter().chain(self.images.iter())
    }

    pub(crate) fn from_legacy(asset: Asset) -> Self {
        let mut material = Self::empty(asset.id.clone());
        material.title = file_stem(&asset.path).unwrap_or("素材").to_owned();
        match asset.kind {
            MediaKind::Video => material.videos.push(asset),
            MediaKind::Images => material.images = Some(asset),
        }
        material
    }

    pub(crate) fn inspect(paths: &[PathBuf]) -> Result<Self> {
        ensure!(!paths.is_empty(), "请选择要导入的素材或文案");
        let mut material = Self::empty(uuid::Uuid::new_v4().to_string());
        let mut seen = HashSet::with_capacity(paths.len());
        let mut image_paths = Vec::new();
        let mut copy = Vec::new();
        let mut text_bytes = 0;
        for path in paths {
            let name = file_name(path);
            let path = path
                .canonicalize()
                .with_context(|| format!("无法访问「{name}」"))?;
            ensure!(seen.insert(path.clone()), "不能重复导入「{name}」");
            match extension(&path).as_str() {
                "mp4" | "mov" | "m4v" | "webm" => {
                    let video = Asset::inspect(&path)
                        .map_err(|error| anyhow::anyhow!("无法导入「{name}」，{error}"))?;
                    material.videos.push(video);
                }
                "jpg" | "jpeg" | "png" | "webp" => {
                    image_paths.push(path);
                }
                "txt" | "md" | "markdown" => {
                    ensure!(
                        material.text_files.len() < MAX_TEXT_FILES,
                        "每组最多导入 {MAX_TEXT_FILES} 个文案文件"
                    );
                    let text = read_copy(&path, &mut text_bytes)?;
                    copy.push(parse_copy(&path, &text));
                    material.text_files.push(path);
                }
                _ => bail!("不支持导入「{name}」，请选择视频、图片或 TXT、Markdown 文案"),
            }
        }
        if !image_paths.is_empty() {
            material.images = Some(Asset::inspect_images(&image_paths)?);
        }
        let mut labelled_videos = material
            .videos
            .iter()
            .enumerate()
            .filter(|(_, asset)| names_primary_video(&asset.path));
        if let Some((index, _)) = labelled_videos.next()
            && index > 0
            && labelled_videos.next().is_none()
        {
            material.videos[..=index].rotate_right(1);
        }

        let primary = copy
            .iter()
            .position(|part| part.title.as_ref().is_some_and(|title| title.declared))
            .or_else(|| copy.iter().position(|part| part.title.is_some()));
        let title = primary
            .and_then(|index| copy[index].title.as_ref())
            .map(|title| title.text.clone());
        let copy_file_title = copy
            .iter()
            .zip(&material.text_files)
            .filter(|(part, _)| !part.description.is_empty())
            .find_map(|(_, path)| file_stem(path))
            .map(str::to_owned);
        material.description = copy
            .into_iter()
            .enumerate()
            .flat_map(|(index, part)| {
                let extra_title = part
                    .title
                    .filter(|_| Some(index) != primary)
                    .map(|title| title.text);
                [extra_title, Some(part.description)].into_iter().flatten()
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        material.title = title
            .or_else(|| {
                let first_line = material
                    .description
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())?;
                if first_line.chars().count() <= IMPLIED_TITLE_LENGTH {
                    Some(first_line.to_owned())
                } else {
                    copy_file_title
                }
            })
            .or_else(|| {
                material
                    .assets()
                    .next()
                    .map(|asset| file_stem(&asset.path).unwrap_or("素材").to_owned())
            })
            .context("所选文案没有可导入的文字")?;
        Ok(material)
    }

    fn empty(id: String) -> Self {
        Self {
            id,
            title: String::new(),
            description: String::new(),
            videos: Vec::new(),
            images: None,
            text_files: Vec::new(),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn file_stem(path: &Path) -> Option<&str> {
    path.file_stem()?
        .to_str()
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
}

fn names_primary_video(path: &Path) -> bool {
    let Some(stem) = file_stem(path) else {
        return false;
    };
    let label = stem.trim_start_matches(|character: char| {
        character.is_ascii_digit()
            || character.is_whitespace()
            || matches!(character, '-' | '_' | '.')
    });
    matches!(label, "主体" | "视频主体" | "主体视频" | "主视频" | "正片")
        || label.eq_ignore_ascii_case("main")
        || label.eq_ignore_ascii_case("primary")
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn read_copy(path: &Path, total_bytes: &mut usize) -> Result<String> {
    let name = file_name(path);
    ensure!(path.is_file(), "「{name}」不是可导入的文案文件");
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("无法读取文案「{name}」"))?
        .take(MAX_TEXT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("无法读取文案「{name}」"))?;
    ensure!(
        bytes.len() as u64 <= MAX_TEXT_FILE_BYTES,
        "文案「{name}」超过 64 KB"
    );
    *total_bytes += bytes.len();
    ensure!(
        *total_bytes <= MAX_TEXT_SET_BYTES,
        "每组文案文件合计最多 128 KB"
    );
    let text =
        String::from_utf8(bytes).with_context(|| format!("请将文案「{name}」保存为 UTF-8 编码"))?;
    Ok(text
        .strip_prefix('\u{feff}')
        .unwrap_or(&text)
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyRole {
    Title,
    Description,
    Generic,
}

fn filename_role(path: &Path) -> CopyRole {
    let stem = file_stem(path).unwrap_or_default().to_ascii_lowercase();
    let words = || stem.split(|character: char| !character.is_alphanumeric());
    if stem.starts_with("标题") || stem.ends_with("标题") || words().any(|word| word == "title")
    {
        CopyRole::Title
    } else if ["简介", "描述", "正文"]
        .iter()
        .any(|&word| stem.starts_with(word) || stem.ends_with(word))
        || words().any(|word| matches!(word, "description" | "intro" | "caption"))
    {
        CopyRole::Description
    } else {
        CopyRole::Generic
    }
}

struct CopyTitle {
    text: String,
    lines: RangeInclusive<usize>,
    declared: bool,
}

struct CopyPart {
    title: Option<CopyTitle>,
    description: String,
}

fn parse_copy(path: &Path, text: &str) -> CopyPart {
    let role = filename_role(path);
    let lines: Vec<_> = text.lines().collect();
    let explicit_title = lines.iter().enumerate().find_map(|(index, line)| {
        let (role, inline) = labelled_line(line)?;
        if role != CopyRole::Title {
            return None;
        }
        let (end, value) = if inline.is_empty() {
            lines
                .iter()
                .enumerate()
                .skip(index + 1)
                .take_while(|(_, line)| labelled_line(line).is_none())
                .find(|(_, line)| !line.trim().is_empty())
                .map(|(end, value)| (end, markdown_heading(value).unwrap_or(value).trim()))?
        } else {
            (index, inline)
        };
        Some(CopyTitle {
            text: value.to_owned(),
            lines: index..=end,
            declared: true,
        })
    });
    let title = explicit_title.or_else(|| {
        if role == CopyRole::Description {
            return None;
        }
        let before_description = lines
            .iter()
            .position(|line| {
                labelled_line(line).is_some_and(|(role, _)| role == CopyRole::Description)
            })
            .unwrap_or(lines.len());
        let candidates = &lines[..before_description];
        let heading = matches!(extension(path).as_str(), "md" | "markdown")
            .then(|| first_markdown_heading(candidates))
            .flatten();
        let (span, value) = heading.or_else(|| {
            candidates
                .iter()
                .enumerate()
                .find(|(_, line)| !line.trim().is_empty())
                .filter(|(_, line)| {
                    role == CopyRole::Title || line.trim().chars().count() <= IMPLIED_TITLE_LENGTH
                })
                .map(|(index, line)| (index..=index, line.trim()))
        })?;
        Some(CopyTitle {
            text: value.to_owned(),
            lines: span,
            declared: role == CopyRole::Title,
        })
    });
    let description = lines
        .into_iter()
        .enumerate()
        .filter(|(index, _)| {
            !title
                .as_ref()
                .is_some_and(|title| title.lines.contains(index))
        })
        .filter_map(|(_, line)| match labelled_line(line) {
            Some((CopyRole::Description, "")) | Some((CopyRole::Title, "")) => None,
            Some((CopyRole::Description, value)) => Some(value),
            _ => Some(line),
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    CopyPart { title, description }
}

fn labelled_line(line: &str) -> Option<(CopyRole, &str)> {
    let (label, value) = markdown_heading(line)
        .unwrap_or(line)
        .split_once(['：', ':'])?;
    let role = match label.trim() {
        "标题" => CopyRole::Title,
        "简介" | "描述" | "正文" => CopyRole::Description,
        _ => return None,
    };
    Some((role, value.trim()))
}

fn markdown_heading(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let hashes = line.bytes().take_while(|&byte| byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let heading = line[hashes..].strip_prefix([' ', '\t'])?.trim();
    let without_hashes = heading.trim_end_matches('#');
    let heading = if without_hashes.ends_with([' ', '\t']) {
        without_hashes.trim_end()
    } else {
        heading
    };
    (!heading.is_empty()).then_some(heading)
}

fn first_markdown_heading<'a>(lines: &[&'a str]) -> Option<(RangeInclusive<usize>, &'a str)> {
    let mut fence = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(marker) = trimmed
            .chars()
            .next()
            .filter(|character| matches!(character, '`' | '~'))
        {
            let count = trimmed
                .chars()
                .take_while(|&character| character == marker)
                .count();
            if count >= 3 {
                match fence {
                    None => fence = Some((marker, count)),
                    Some((opening, minimum))
                        if marker == opening && count >= minimum && trimmed.len() == count =>
                    {
                        fence = None
                    }
                    _ => {}
                }
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        if let Some(heading) = markdown_heading(line) {
            return Some((index..=index, heading));
        }
        if !trimmed.is_empty()
            && !matches!(trimmed, "---" | "***" | "___")
            && let Some(underline) = lines.get(index + 1).map(|line| line.trim())
            && !underline.is_empty()
            && (underline.bytes().all(|byte| byte == b'=')
                || underline.bytes().all(|byte| byte == b'-'))
        {
            return Some((index..=index + 1, trimmed));
        }
    }
    None
}
