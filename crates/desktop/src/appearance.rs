use gpui_kit::{assets::IconName, component::*, prelude::FluentBuilder, *};
use publisher_core::{JobState, Platform};
use publisher_platforms::upload_progress::UploadProgress;

pub const CANVAS: u32 = 0xf6f7fb;
pub const SURFACE: u32 = 0xffffff;
pub const BORDER: u32 = 0xe4e8f0;
pub const INK: u32 = 0x182237;
pub const MUTED: u32 = 0x65718a;
pub const ACCENT: u32 = 0x5146d9;
pub const ACCENT_WASH: u32 = 0xefedff;

pub fn install_theme(cx: &mut App) {
    Theme::change(ThemeMode::Light, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = "PingFang SC".into();
        theme.font_size = px(16.);
        theme.radius = px(8.);
        theme.radius_lg = px(14.);
        let colors = &mut theme.colors;
        colors.background = rgb(SURFACE).into();
        colors.foreground = rgb(INK).into();
        colors.border = rgb(BORDER).into();
        colors.input = rgb(0xdce2ec).into();
        colors.muted = rgb(CANVAS).into();
        colors.muted_foreground = rgb(MUTED).into();
        colors.ring = rgb(ACCENT).into();
        colors.accent = rgb(ACCENT_WASH).into();
        colors.accent_foreground = rgb(ACCENT).into();
        colors.selection = rgb(0xe0dbff).into();
        colors.primary = rgb(ACCENT).into();
        colors.primary_hover = rgb(0x463bc8).into();
        colors.primary_active = rgb(0x3c31b5).into();
        colors.primary_foreground = rgb(SURFACE).into();
        colors.button_primary = colors.primary;
        colors.button_primary_hover = colors.primary_hover;
        colors.button_primary_active = colors.primary_active;
        colors.button_primary_foreground = colors.primary_foreground;
        colors.button = rgb(SURFACE).into();
        colors.button_foreground = rgb(INK).into();
        colors.button_hover = rgb(0xf0f2fa).into();
        colors.button_active = rgb(0xe8ebf6).into();
    });
}

pub fn surface() -> Div {
    v_flex()
        .min_w_0()
        .gap_4()
        .p_5()
        .rounded(px(16.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(SURFACE))
}

pub fn glyph(name: IconName, size: f32, ink: u32) -> Svg {
    svg()
        .path(name.path())
        .size(px(size))
        .flex_shrink_0()
        .text_color(rgb(ink))
}

pub fn icon_tile(name: IconName, size: f32, ink: u32, background: u32) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(10.))
        .bg(rgb(background))
        .child(glyph(name, size * 0.48, ink))
}

pub fn state_pill(state: JobState) -> Div {
    let (ink, background) = match state {
        JobState::Draft | JobState::Queued => (MUTED, 0xf1f3f8),
        JobState::Preparing | JobState::Uploading | JobState::Submitting => (ACCENT, ACCENT_WASH),
        JobState::WaitingUser | JobState::Unknown => (0x8b4f10, 0xfff2df),
        JobState::Submitted => (0x147154, 0xe7f6ef),
        JobState::Failed => (0xa83646, 0xfceef1),
        JobState::Cancelled => (MUTED, 0xf1f3f8),
    };
    h_flex()
        .flex_shrink_0()
        .gap_2()
        .px(px(10.))
        .py_1()
        .rounded_full()
        .bg(rgb(background))
        .text_xs()
        .text_color(rgb(ink))
        .child(div().size(px(6.)).rounded_full().bg(rgb(ink)))
        .child(state.label())
}

pub fn upload_progress_panel(job_id: &str, platform: Platform, progress: UploadProgress) -> Div {
    let name = platform.name();
    let (label, percent) = match progress {
        UploadProgress::ValidatingFile => (format!("{name}网页已收到素材，正在等待开始上传"), None),
        UploadProgress::Uploading { percent: Some(100) } => {
            (format!("等待{name}确认上传完成"), Some(100))
        }
        UploadProgress::Uploading {
            percent: Some(percent),
        } => (format!("{name}上传进度"), Some(percent)),
        UploadProgress::Uploading { percent: None } => (format!("等待{name}返回上传进度"), None),
        UploadProgress::Processing => (format!("{name}正在处理素材"), None),
        UploadProgress::EditorMissing => ("上传完成，正在等待作品编辑区".into(), None),
        UploadProgress::FillingForm => ("正在填写作品信息".into(), None),
    };
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded(px(10.))
        .bg(rgb(ACCENT_WASH))
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(ACCENT))
                .child(label.clone())
                .when_some(percent, |row, percent| row.child(format!("{percent}%"))),
        )
        .when_some(percent, |panel, percent| {
            panel.child(
                progress::Progress::new(SharedString::from(format!("upload-progress-{job_id}")))
                    .accessibility_label(label)
                    .color(rgb(ACCENT))
                    .value(f32::from(percent))
                    .small(),
            )
        })
}
