mod accounts;
mod appearance;
mod backend;
mod compose;
mod materials;
mod video_preview;
extern crate gpui_kit as gpui;
use accounts::AccountDetails;
use appearance::surface;
use appearance::*;
use backend::*;
use compose::media_icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    assets::{AllAssets, IconName as AppIcon},
    component::{
        button::*,
        input::{Input, InputContentType, InputEvent, InputState, TextareaState},
        *,
    },
    *,
};
use publisher_core::Platform;
use publisher_core::*;
use publisher_platforms::douyin::{CREATOR_MANAGE_URL, UPLOAD_URL as DOUYIN_UPLOAD_URL};
use publisher_platforms::kuaishou::CREATOR_MANAGE_URL as KUAISHOU_MANAGE_URL;
use publisher_platforms::sign_in::{
    PublishSmsPreview, SignInPreview, SignInStatus, VerificationAction, WechatChannelChoice,
    WechatChannelsPreview,
};
use publisher_platforms::upload_progress::UploadProgress;
use publisher_platforms::wechat_channels::CREATOR_MANAGE_URL as WECHAT_CHANNELS_MANAGE_URL;
use publisher_platforms::xiaohongshu::CREATOR_MANAGE_URL as XIAOHONGSHU_MANAGE_URL;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use video_preview::VideoPreview;
gpui_kit::actions!(publisher, [Quit]);

const PRODUCT_NAME: &str = "Airs";
const SIDEBAR_WIDTH: f32 = 184.;
const WORKSPACE_MAX_WIDTH: f32 = 1240.;
const WORKSPACE_PADDING: f32 = 24.;

fn workspace_content_width(window: &Window) -> f32 {
    (window.viewport_size().width.as_f32() - SIDEBAR_WIDTH).min(WORKSPACE_MAX_WIDTH)
        - WORKSPACE_PADDING * 2.
}

fn workspace_frame() -> Div {
    v_flex()
        .w_full()
        .max_w(px(WORKSPACE_MAX_WIDTH))
        .mx_auto()
        .px(px(WORKSPACE_PADDING))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Page {
    Overview,
    Accounts,
    Assets,
    Compose,
    Jobs,
    Settings,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Overview => "工作台",
            Self::Accounts => "账号管理",
            Self::Assets => "素材库",
            Self::Compose => "新建发布",
            Self::Jobs => "任务与记录",
            Self::Settings => "设置",
        }
    }

    fn icon(self) -> AppIcon {
        match self {
            Self::Overview => AppIcon::LayoutDashboard,
            Self::Accounts => AppIcon::Users,
            Self::Assets => AppIcon::FolderOpen,
            Self::Compose => AppIcon::SquarePen,
            Self::Jobs => AppIcon::ListChecks,
            Self::Settings => AppIcon::Settings,
        }
    }
}
struct Publisher {
    backend: Backend,
    snapshot: Snapshot,
    product_icon: Arc<Image>,
    sign_in_images: HashMap<String, Arc<Image>>,
    verification_images: HashMap<String, VerificationImage>,
    verification_inputs: HashMap<String, (Entity<InputState>, Subscription)>,
    publish_sms_inputs: HashMap<u64, PublishSmsInput>,
    page: Page,
    message: String,
    alias: Entity<InputState>,
    title: Entity<InputState>,
    description: Entity<TextareaState>,
    tags: Entity<InputState>,
    browser: Entity<InputState>,
    selected_accounts: HashSet<String>,
    material_details: Option<String>,
    selected_material_set: Option<String>,
    material_autofill: [Option<String>; 2],
    selected_asset: Option<String>,
    compose_media_kind: MediaKind,
    compose_image_order: Vec<usize>,
    douyin_video_covers: DouyinVideoCovers,
    photo_preview: Option<PhotoPreview>,
    video_preview: VideoPreview,
    preview_check: Option<PreviewCheck>,
    compose_account_filter: Option<Platform>,
    compose_asset_picker_open: bool,
    batch_key: String,
    pending_batch: Option<String>,
    queued: bool,
    browser_loaded: bool,
    account_filter: Option<Platform>,
    account_details: Option<AccountDetails>,
    job_account_filter: Option<AccountJobFilter>,
    confirming_publish: Option<String>,
    confirming_not_published: Option<String>,
    confirming_delete_job: Option<String>,
    _subscriptions: Vec<Subscription>,
}

struct AccountJobFilter {
    id: String,
    alias: String,
    platform: Platform,
}

struct PreviewCheck {
    asset_id: String,
    _task: Task<()>,
}

struct PhotoPreview {
    page: Page,
    paths: Vec<std::path::PathBuf>,
    index: usize,
}

#[derive(Default)]
struct VerificationImage {
    displayed: Option<DisplayedVerificationImage>,
    _decoding: Option<Task<()>>,
}

struct DisplayedVerificationImage {
    image: Arc<RenderImage>,
    width: u32,
    height: u32,
}

struct PublishSmsInput {
    input: Entity<InputState>,
    valid: bool,
    _subscription: Subscription,
}

fn valid_sms_code(value: &str) -> bool {
    let value = value.trim();
    (4..=8).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn publish_sms_challenge(
    snapshot: &Snapshot,
    job: &Job,
) -> Option<(u64, PublishSmsPreview, Option<&'static str>)> {
    if job.state != JobState::Submitting || job.account.platform != Platform::Douyin {
        return None;
    }
    let preview = snapshot.sign_ins.get(&job.account.id)?;
    Some((
        preview.verification_id?,
        preview.publish_sms?,
        preview.input_feedback,
    ))
}

fn creator_content_management(platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::Douyin => Some(CREATOR_MANAGE_URL),
        Platform::Kuaishou => Some(KUAISHOU_MANAGE_URL),
        Platform::Xiaohongshu => Some(XIAOHONGSHU_MANAGE_URL),
        Platform::WechatChannels => Some(WECHAT_CHANNELS_MANAGE_URL),
        Platform::Simulation => None,
    }
}

fn creator_management_label(platform: Platform) -> String {
    if platform == Platform::Xiaohongshu {
        "打开小红书笔记管理".into()
    } else {
        format!("打开{}内容管理", platform.name())
    }
}

impl Publisher {
    fn new(backend: Backend, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if this
                    .update_in(cx, |s, window, cx| {
                        while let Ok(u) = s.backend.rx.try_recv() {
                            match u {
                                Update::Snapshot(data) if *data != s.snapshot => {
                                    s.apply_snapshot(*data, cx);
                                    cx.notify();
                                }
                                Update::Message(m) if m != s.message => {
                                    s.message = m;
                                    cx.notify();
                                }
                                Update::AccountAdded(id) if s.page == Page::Accounts => {
                                    s.open_account(id);
                                    cx.notify();
                                }
                                Update::ComposeMaterialImported(id) if s.page == Page::Compose => {
                                    s.use_material_set(&id, None, window, cx);
                                }
                                Update::BatchEnqueueResult { key, result } => {
                                    s.finish_compose_batch(key, result);
                                    cx.notify();
                                }
                                _ => {}
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let description = cx.new(|cx| TextareaState::new(window, cx).placeholder("填写作品描述"));
        let mut input = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let mut view = Self {
            backend,
            snapshot: Snapshot::default(),
            product_icon: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../assets/AirsIcon.png").to_vec(),
            )),
            sign_in_images: HashMap::new(),
            verification_images: HashMap::new(),
            verification_inputs: HashMap::new(),
            publish_sms_inputs: HashMap::new(),
            page: Page::Overview,
            message: String::new(),
            alias: input("修改此账号的显示名称", cx),
            title: input("作品标题", cx),
            description,
            tags: input("用空格分隔，最多 5 个标签", cx),
            browser: input("Chrome 可执行程序的完整路径", cx),
            selected_accounts: HashSet::new(),
            material_details: None,
            selected_material_set: None,
            material_autofill: Default::default(),
            selected_asset: None,
            compose_media_kind: MediaKind::Video,
            compose_image_order: Vec::new(),
            douyin_video_covers: DouyinVideoCovers::default(),
            photo_preview: None,
            video_preview: VideoPreview::default(),
            preview_check: None,
            compose_account_filter: None,
            compose_asset_picker_open: false,
            batch_key: new_key(),
            pending_batch: None,
            queued: false,
            browser_loaded: false,
            account_filter: None,
            account_details: None,
            job_account_filter: None,
            confirming_publish: None,
            confirming_not_published: None,
            confirming_delete_job: None,
            _subscriptions: Vec::new(),
        };
        for field in [&view.title, &view.tags] {
            view._subscriptions
                .push(cx.subscribe(field, |s, _, event: &InputEvent, cx| {
                    s.compose_input_changed(event, cx);
                }));
        }
        view._subscriptions.push(
            cx.subscribe(&view.description, |s, _, event: &InputEvent, cx| {
                s.compose_input_changed(event, cx)
            }),
        );
        view
    }

    fn navigate(&mut self, page: Page) {
        if page != Page::Jobs || self.page != Page::Jobs || self.job_account_filter.is_some() {
            self.confirming_publish = None;
            self.confirming_not_published = None;
            self.confirming_delete_job = None;
        }
        self.page = page;
        self.photo_preview = None;
        self.account_details = None;
        self.material_details = None;
        self.job_account_filter = None;
    }

    fn open_account_jobs(&mut self, account: &Account) {
        self.navigate(Page::Jobs);
        self.job_account_filter = Some(AccountJobFilter {
            id: account.id.clone(),
            alias: account.alias.clone(),
            platform: account.platform,
        });
    }

    fn job_in_scope(&self, job: &Job) -> bool {
        self.job_account_filter
            .as_ref()
            .is_none_or(|account| job.account.id == account.id)
    }

    fn apply_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let previous = &self.snapshot.sign_ins;
        self.sign_in_images.retain(|id, image| {
            let unchanged = previous
                .get(id)
                .and_then(|preview| preview.png.as_ref())
                .zip(
                    snapshot
                        .sign_ins
                        .get(id)
                        .and_then(|preview| preview.png.as_ref()),
                )
                .is_some_and(|(old, new)| Arc::ptr_eq(old, new));
            if !unchanged {
                image.clone().remove_asset(cx);
            }
            unchanged
        });
        let same_frame_verification = |id: &String| {
            snapshot
                .sign_ins
                .get(id)
                .filter(|preview| preview.publish_sms.is_none())
                .and_then(|preview| preview.verification_id)
                .is_some_and(|generation| {
                    previous.get(id).and_then(|preview| preview.verification_id) == Some(generation)
                })
        };
        self.verification_images.retain(|id, entry| {
            let keep = same_frame_verification(id);
            if !keep && let Some(frame) = entry.displayed.take() {
                cx.drop_image(frame.image, None);
            }
            keep
        });
        self.verification_inputs
            .retain(|id, _| same_frame_verification(id));
        self.publish_sms_inputs.retain(|verification_id, _| {
            snapshot.sign_ins.values().any(|preview| {
                preview.publish_sms.is_some() && preview.verification_id == Some(*verification_id)
            })
        });
        for (id, preview) in &snapshot.sign_ins {
            if let Some(png) = &preview.png {
                self.sign_in_images.entry(id.clone()).or_insert_with(|| {
                    Arc::new(Image::from_bytes(ImageFormat::Png, png.as_ref().clone()))
                });
            }
            let Some(frame) = &preview.verification else {
                continue;
            };
            if same_frame_verification(id)
                && previous
                    .get(id)
                    .and_then(|preview| preview.verification.as_ref())
                    .is_some_and(|previous| Arc::ptr_eq(&previous.png, &frame.png))
            {
                continue;
            }

            let id = id.clone();
            let frame = frame.clone();
            let source = frame.png.clone();
            let verification_id = preview.verification_id;
            let svg_renderer = cx.svg_renderer();
            let decoded = cx.background_executor().spawn(async move {
                let image = Image::from_bytes(ImageFormat::Png, frame.png.as_ref().clone())
                    .to_image_data(svg_renderer)
                    .ok()?;
                Some(DisplayedVerificationImage {
                    image,
                    width: frame.width,
                    height: frame.height,
                })
            });
            // Retain the displayed frame while its replacement decodes.
            let entry = self.verification_images.entry(id.clone()).or_default();
            entry._decoding = Some(cx.spawn(async move |this, cx| {
                if let Some(frame) = decoded.await {
                    let _ = this.update(cx, |view, cx| {
                        let current = view.snapshot.sign_ins.get(&id).is_some_and(|preview| {
                            preview.verification_id == verification_id
                                && preview
                                    .verification
                                    .as_ref()
                                    .is_some_and(|current| Arc::ptr_eq(&current.png, &source))
                        });
                        if current && let Some(entry) = view.verification_images.get_mut(&id) {
                            if let Some(old) = entry.displayed.replace(frame) {
                                cx.drop_image(old.image, None);
                            }
                            cx.notify();
                        }
                    });
                }
            }));
        }
        let selected_count = self.selected_accounts.len();
        self.selected_accounts.retain(|id| {
            snapshot.accounts.iter().any(|account| {
                account.id == *id && snapshot.can_prepare(account.platform, MediaKind::Video)
            })
        });
        if self.selected_accounts.len() != selected_count {
            self.reset_compose_batch();
        }
        if self
            .account_details
            .as_ref()
            .is_some_and(|details| !snapshot.accounts.iter().any(|a| a.id == details.id))
        {
            self.account_details = None;
        }
        if self.confirming_publish.as_ref().is_some_and(|id| {
            !snapshot
                .jobs
                .iter()
                .any(|job| &job.id == id && snapshot.can_submit(job))
        }) {
            self.confirming_publish = None;
        }
        if self.confirming_not_published.as_ref().is_some_and(|id| {
            !snapshot.jobs.iter().any(|job| {
                &job.id == id
                    && job.state == JobState::Unknown
                    && creator_content_management(job.account.platform).is_some()
                    && snapshot
                        .accounts
                        .iter()
                        .any(|account| account.id == job.account.id)
            })
        }) {
            self.confirming_not_published = None;
        }
        if self.confirming_delete_job.as_ref().is_some_and(|id| {
            !snapshot
                .jobs
                .iter()
                .any(|job| &job.id == id && job.state.can_delete())
        }) {
            self.confirming_delete_job = None;
        }
        self.snapshot = snapshot;
    }

    fn sign_in_panel(
        &self,
        account_id: &str,
        panel_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self.snapshot.sign_ins.get(account_id)?;
        if preview.publish_sms.is_some() {
            return None;
        }
        let platform = self
            .snapshot
            .accounts
            .iter()
            .find(|account| account.id == account_id)?
            .platform;
        let mut panel = h_flex()
            .items_center()
            .flex_wrap()
            .gap_5()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(0xe2defb))
            .bg(rgb(0xf7f6ff));
        if platform == Platform::WechatChannels
            && let (Some(mode), Some(verification_id)) =
                (&preview.wechat_channels, preview.verification_id)
        {
            let (title, detail, scan_label) = match mode {
                WechatChannelsPreview::Choice { nickname } => (
                    "登录视频号",
                    Some(format!("使用电脑上的微信「{nickname}」确认登录。")),
                    "使用微信扫码",
                ),
                WechatChannelsPreview::WaitingForDesktop => {
                    ("正在等待桌面微信确认", None, "改用扫码")
                }
                WechatChannelsPreview::SelectChannel { channels } => {
                    let picker = self.wechat_channel_picker(
                        account_id,
                        panel_id,
                        verification_id,
                        channels,
                        cx,
                    );
                    return Some(
                        panel
                            .child(
                                picker.when_some(preview.input_feedback, |picker, feedback| {
                                    picker.child(
                                        div().text_sm().text_color(rgb(MUTED)).child(feedback),
                                    )
                                }),
                            )
                            .into_any_element(),
                    );
                }
            };
            let scan_account = account_id.to_owned();
            let actions = h_flex()
                .flex_wrap()
                .gap_2()
                .when(
                    matches!(mode, WechatChannelsPreview::Choice { .. }),
                    |row| {
                        let quick_account = account_id.to_owned();
                        row.child(
                            Button::new(eid("wechat-quick-login", panel_id))
                                .primary()
                                .label("微信快捷登录")
                                .on_click(cx.listener(move |s, _, _, _| {
                                    s.backend.send(Command::VerificationInput(
                                        quick_account.clone(),
                                        verification_id,
                                        VerificationAction::WechatQuickLogin,
                                    ));
                                })),
                        )
                    },
                )
                .child(
                    Button::new(eid("wechat-scan-login", panel_id))
                        .label(scan_label)
                        .on_click(cx.listener(move |s, _, _, _| {
                            s.backend.send(Command::VerificationInput(
                                scan_account.clone(),
                                verification_id,
                                VerificationAction::WechatScanLogin,
                            ));
                        })),
                );
            return Some(
                panel
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                            .when_some(detail, |info, detail| {
                                info.child(div().text_sm().text_color(rgb(0x635c78)).child(detail))
                            })
                            .child(actions)
                            .when_some(preview.input_feedback, |info, feedback| {
                                info.child(
                                    div().text_sm().text_color(rgb(0x635c78)).child(feedback),
                                )
                            }),
                    )
                    .into_any_element(),
            );
        }
        let name = platform.name();
        let login_app = if platform == Platform::WechatChannels {
            "微信"
        } else {
            name
        };
        let (title, detail, refresh_label): (_, SharedString, _) = match preview.status {
            SignInStatus::Loading if platform == Platform::Xiaohongshu => (
                "正在打开小红书登录页",
                "正在检查扫码入口，二维码不可用时可使用短信登录。".into(),
                None,
            ),
            SignInStatus::Loading => (
                "正在获取登录二维码",
                "二维码就绪后会显示在这里。".into(),
                Some("重新获取二维码"),
            ),
            SignInStatus::WaitingForScan => (
                "扫码登录",
                format!("打开{login_app} App 扫码，按手机上的提示完成登录。").into(),
                Some("刷新二维码"),
            ),
            SignInStatus::ConfirmOnPhone if platform == Platform::WechatChannels => {
                ("等待微信确认", "请按微信中的提示确认登录。".into(), None)
            }
            SignInStatus::ConfirmOnPhone => (
                "已扫码，等待手机确认",
                format!("请在{login_app} App 确认这次登录。").into(),
                None,
            ),
            SignInStatus::Expired => (
                "二维码已过期",
                "获取新二维码后，请重新扫码。".into(),
                Some("获取新二维码"),
            ),
            SignInStatus::VerifyInApp => {
                return Some(self.verification_panel(account_id, panel_id, platform, preview, cx));
            }
            SignInStatus::CheckingAccount => (
                "正在确认登录结果",
                format!("正在读取{name}创作者平台的账号状态。").into(),
                None,
            ),
        };
        if preview.status == SignInStatus::WaitingForScan
            && let Some(image) = self.sign_in_images.get(account_id)
        {
            panel = panel.child(
                div()
                    .flex_shrink_0()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0xe8e3fb))
                    .bg(rgb(0xffffff))
                    .child(img(image.clone()).size(px(224.))),
            );
        }
        let mut info = v_flex()
            .items_start()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(rgb(0x635c78)).child(detail));
        if let Some(label) = refresh_label {
            let id = account_id.to_string();
            info = info.child(
                Button::new(eid("refresh-sign-in", panel_id))
                    .small()
                    .rounded(px(8.))
                    .icon(IconName::RefreshCw)
                    .label(label)
                    .on_click(cx.listener(move |s, _, _, _| {
                        s.backend.send(Command::RefreshSignIn(id.clone()))
                    })),
            );
        }
        Some(panel.child(info).into_any_element())
    }

    fn wechat_channel_picker(
        &self,
        account_id: &str,
        panel_id: &str,
        verification_id: u64,
        channels: &[WechatChannelChoice],
        cx: &mut Context<Self>,
    ) -> Div {
        let picker_id = format!("{panel_id}-{verification_id}");
        let choices = channels.iter().enumerate().map(|(index, channel)| {
            let label = if channel.role.is_empty() {
                channel.nickname.clone()
            } else {
                format!("{}，{}", channel.nickname, channel.role)
            };
            Button::new(eid(
                "wechat-select-channel",
                &format!("{picker_id}-{index}"),
            ))
            .h(px(44.))
            .w_full()
            .disabled(!channel.available)
            .accessibility_label(label)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_left()
                            .font_weight(FontWeight::MEDIUM)
                            .child(channel.nickname.clone()),
                    )
                    .when(!channel.role.is_empty(), |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(channel.role.clone()),
                        )
                    })
                    .when(!channel.available, |row| {
                        row.child(div().text_xs().text_color(rgb(MUTED)).child("不可用"))
                    }),
            )
            .when(channel.available, |button| {
                let account_id = account_id.to_owned();
                button.on_click(cx.listener(move |s, _, _, _| {
                    s.backend.send(Command::VerificationInput(
                        account_id.clone(),
                        verification_id,
                        VerificationAction::WechatSelectChannel { index },
                    ));
                }))
            })
        });
        v_flex()
            .w_full()
            .max_w(px(540.))
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("选择要登录的视频号"),
            )
            .child(
                v_flex()
                    .id(eid("wechat-channel-options", &picker_id))
                    .gap_2()
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .children(choices),
            )
            .when(
                !channels.iter().any(|channel| channel.available),
                |picker| {
                    picker.child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("没有可选的视频号。"),
                    )
                },
            )
    }

    fn verification_panel(
        &self,
        account_id: &str,
        panel_id: &str,
        platform: Platform,
        preview: &SignInPreview,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let role_selection = platform == Platform::Kuaishou;
        let panel = v_flex()
            .items_start()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(0xe2defb))
            .bg(rgb(0xf7f6ff))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(match platform {
                        Platform::Kuaishou => "选择快手登录角色",
                        Platform::Xiaohongshu => "小红书短信登录",
                        Platform::WechatChannels => "完成视频号登录验证",
                        _ => "完成抖音安全验证",
                    }),
            );
        let (Some(frame), Some(verification_id)) = (
            self.verification_images
                .get(account_id)
                .and_then(|entry| entry.displayed.as_ref()),
            preview.verification_id,
        ) else {
            return panel
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x635c78))
                        .child("正在载入平台验证页面。"),
                )
                .into_any_element();
        };
        if frame.width == 0 || frame.height == 0 {
            return panel.into_any_element();
        }

        let ratio = frame.width as f32 / frame.height as f32;
        let bounds = Rc::new(Cell::<Option<Bounds<Pixels>>>::new(None));
        let measured_bounds = bounds.clone();
        let click_account = account_id.to_string();
        let viewport = div()
            .on_children_prepainted(move |children, _, _| {
                measured_bounds.set(children.first().copied());
            })
            .id(eid("verification-viewport", panel_id))
            .w_full()
            .max_w(px(620.))
            .aspect_ratio(ratio)
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0xe2defb))
            .bg(rgb(0xffffff))
            .child(
                img(frame.image.clone())
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |s, event: &MouseDownEvent, _, cx| {
                    let Some(bounds) = bounds.get() else {
                        return;
                    };
                    if !bounds.contains(&event.position)
                        || bounds.size.width <= px(0.)
                        || bounds.size.height <= px(0.)
                    {
                        return;
                    }
                    let x = f64::from(f32::from(event.position.x - bounds.origin.x))
                        / f64::from(f32::from(bounds.size.width));
                    let y = f64::from(f32::from(event.position.y - bounds.origin.y))
                        / f64::from(f32::from(bounds.size.height));
                    if !(0. ..1.).contains(&x) || !(0. ..1.).contains(&y) {
                        return;
                    }
                    s.backend.send(Command::VerificationInput(
                        click_account.clone(),
                        verification_id,
                        VerificationAction::Click { x, y },
                    ));
                    cx.stop_propagation();
                }),
            );
        panel
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x635c78))
                    .child(if role_selection {
                        "在下方选择本次登录的角色，再点击「确定」。"
                    } else if platform == Platform::Xiaohongshu {
                        "按小红书页面提示登录。先点手机号或验证码输入框，再从这里填入。"
                    } else {
                        "按下方网页提示操作。需要输入时，先点网页里的输入框，再在这里输入或粘贴。"
                    }),
            )
            .when_some(
                self.verification_inputs
                    .get(account_id)
                    .filter(|_| !role_selection),
                |panel, (input, _)| {
                    let fill_account = account_id.to_string();
                    let fill_input = input.clone();
                    panel.child(
                        h_flex()
                            .w_full()
                            .max_w(px(420.))
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(input)))
                            .child(
                                Button::new(eid("fill-verification", panel_id))
                                    .primary()
                                    .label("填入网页")
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.fill_verification(
                                            &fill_account,
                                            verification_id,
                                            &fill_input,
                                            cx,
                                        );
                                    })),
                            ),
                    )
                },
            )
            .when_some(
                preview
                    .input_feedback
                    .or_else(|| (!role_selection).then_some("填入后，在下方网页继续验证。")),
                |panel, feedback| {
                    panel.child(div().text_sm().text_color(rgb(0x635c78)).child(feedback))
                },
            )
            .child(viewport)
            .into_any_element()
    }

    fn fill_verification(
        &self,
        id: &str,
        verification_id: u64,
        input: &Entity<InputState>,
        cx: &App,
    ) {
        let value = input.read(cx).value();
        if !value.trim().is_empty() {
            self.backend.send(Command::VerificationInput(
                id.to_owned(),
                verification_id,
                VerificationAction::FillText(value.to_string()),
            ));
        }
    }

    fn publish_sms_panel(
        &mut self,
        job: &Job,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (verification_id, sms, feedback) = publish_sms_challenge(&self.snapshot, job)?;
        let field = self
            .publish_sms_inputs
            .entry(verification_id)
            .or_insert_with(|| {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("短信验证码"));
                let subscription = cx.subscribe(&input, move |view, input, event, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Some(field) = view.publish_sms_inputs.get_mut(&verification_id)
                    {
                        let valid = valid_sms_code(&input.read(cx).value());
                        if field.valid != valid {
                            field.valid = valid;
                            cx.notify();
                        }
                    }
                });
                PublishSmsInput {
                    input,
                    valid: false,
                    _subscription: subscription,
                }
            });
        let input = field.input.clone();
        let request_account = job.account.id.clone();
        let verify_account = job.account.id.clone();
        let verify_input = input.clone();
        let request_label = match sms.resend_seconds {
            Some(seconds) if !sms.can_request_code => format!("{seconds} 秒后重试"),
            _ => "获取验证码".into(),
        };

        Some(
            h_flex()
                .items_start()
                .gap_3()
                .p_4()
                .flex_shrink_0()
                .rounded_xl()
                .border_1()
                .border_color(rgb(0xe2defb))
                .bg(rgb(0xf7f6ff))
                .child(icon_tile(AppIcon::ShieldCheck, 36., ACCENT, ACCENT_WASH))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("抖音发布需要短信验证"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .truncate()
                                .child(format!("{} · {}", job.account.alias, job.content.title)),
                        )
                        .child(
                            h_flex()
                                .max_w(px(500.))
                                .w_full()
                                .gap_2()
                                .child(
                                    Button::new(eid("publish-sms-request", &job.id))
                                        .label(request_label)
                                        .disabled(!sms.can_request_code)
                                        .on_click(cx.listener(move |s, _, _, _| {
                                            s.backend.send(Command::VerificationInput(
                                                request_account.clone(),
                                                verification_id,
                                                VerificationAction::RequestSmsCode,
                                            ));
                                        })),
                                )
                                .child(
                                    div().min_w_0().flex_1().child(
                                        Input::new(&input)
                                            .content_type(InputContentType::OneTimeCode)
                                            .aria_label("短信验证码"),
                                    ),
                                )
                                .child(
                                    Button::new(eid("publish-sms-verify", &job.id))
                                        .primary()
                                        .label("验证")
                                        .disabled(!sms.can_verify || !field.valid)
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            let value = verify_input.read(cx).value();
                                            if valid_sms_code(&value) {
                                                s.backend.send(Command::VerificationInput(
                                                    verify_account.clone(),
                                                    verification_id,
                                                    VerificationAction::VerifySmsCode(
                                                        value.trim().to_owned(),
                                                    ),
                                                ));
                                                verify_input.update(cx, |input, cx| {
                                                    input.set_value("", window, cx);
                                                });
                                                if let Some(field) =
                                                    s.publish_sms_inputs.get_mut(&verification_id)
                                                {
                                                    field.valid = false;
                                                    cx.notify();
                                                }
                                            }
                                        })),
                                ),
                        )
                        .when_some(feedback, |panel, feedback| {
                            panel.child(div().text_sm().text_color(rgb(MUTED)).child(feedback))
                        }),
                ),
        )
    }

    fn publish_sms_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let pending = self
            .snapshot
            .jobs
            .iter()
            .filter(|job| self.page != Page::Jobs || !self.job_in_scope(job))
            .filter(|job| publish_sms_challenge(&self.snapshot, job).is_some())
            .count();
        (pending > 0).then(|| {
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(ACCENT_WASH))
                .child(glyph(AppIcon::ShieldCheck, 18., ACCENT))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(format!("{pending} 个抖音任务需要短信验证")),
                )
                .child(
                    Button::new("open-publish-sms-jobs")
                        .small()
                        .label("查看任务")
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.navigate(Page::Jobs);
                            cx.notify();
                        })),
                )
        })
    }

    fn pick_materials(&mut self, cx: &mut Context<Self>) {
        let select_for_compose = self.page == Page::Compose;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("导入素材".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && !paths.is_empty()
            {
                let _ = this.update(cx, |s, _| {
                    s.backend.send(Command::Import {
                        paths,
                        select_for_compose,
                    });
                });
            }
        })
        .detach();
    }

    fn preview_button(&self, asset: &publisher_core::Asset, cx: &mut Context<Self>) -> Button {
        let id = asset.id.clone();
        if asset.kind == MediaKind::Images {
            let paths: Vec<_> = asset
                .images
                .iter()
                .map(|image| image.path.clone())
                .collect();
            return Button::new(eid("preview-photos", &id))
                .icon(AppIcon::Image)
                .label("预览")
                .accessibility_label("预览图文图片")
                .disabled(paths.is_empty())
                .on_click(cx.listener(move |s, _, _, cx| {
                    s.photo_preview = Some(PhotoPreview {
                        page: s.page,
                        paths: paths.clone(),
                        index: 0,
                    });
                    cx.notify();
                }));
        }
        let path = asset.path.clone();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let checking = self
            .preview_check
            .as_ref()
            .is_some_and(|preview| preview.asset_id == id);
        Button::new(eid("preview-video", &id))
            .icon(AppIcon::Play)
            .label(if checking { "正在打开" } else { "预览" })
            .accessibility_label(format!("预览视频 {name}"))
            .disabled(checking)
            .on_click(cx.listener(move |s, _, _, cx| {
                let path = path.clone();
                let check = cx.background_executor().spawn(async move {
                    let result = VideoPreview::check(&path);
                    (path, result)
                });
                let task = cx.spawn(async move |this, cx| {
                    let (path, result) = check.await;
                    let _ = this.update(cx, |s, cx| {
                        s.preview_check = None;
                        if let Err(error) = result.and_then(|()| s.video_preview.open(&path)) {
                            s.message = error.to_string();
                        }
                        cx.notify();
                    });
                });
                s.preview_check = Some(PreviewCheck {
                    asset_id: id.clone(),
                    _task: task,
                });
                cx.notify();
            }))
    }

    fn content(&self, cx: &App) -> Option<Content> {
        let asset = self.compose_ordered_asset()?;
        let douyin_video_covers = if asset.kind == MediaKind::Video {
            self.douyin_video_covers.clone()
        } else {
            DouyinVideoCovers::default()
        };
        Some(Content {
            asset,
            title: self.title.read(cx).value().to_string(),
            description: self.description.read(cx).value().to_string(),
            tags: self
                .tags
                .read(cx)
                .value()
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            douyin_video_covers,
        })
    }

    fn photo_gallery(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let preview = self
            .photo_preview
            .as_ref()
            .filter(|preview| preview.page == self.page)?;
        let path = preview.paths.get(preview.index)?;
        let index = preview.index;
        let count = preview.paths.len();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let height = (window.viewport_size().height.as_f32() - 315.).clamp(200., 650.);
        Some(
            surface()
                .gap_3()
                .child(
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(
                            Button::new("close-photo-preview")
                                .ghost()
                                .label("关闭预览")
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.photo_preview = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    img(path.clone())
                        .w_full()
                        .h(px(height))
                        .rounded_lg()
                        .bg(rgb(CANVAS))
                        .object_fit(ObjectFit::Contain),
                )
                .child(
                    h_flex()
                        .when(count == 1, |controls| controls.hidden())
                        .justify_center()
                        .gap_3()
                        .child(
                            Button::new("previous-photo")
                                .icon(AppIcon::ArrowLeft)
                                .label("上一张")
                                .disabled(index == 0)
                                .on_click(cx.listener(|s, _, _, cx| {
                                    if let Some(preview) = &mut s.photo_preview {
                                        preview.index = preview.index.saturating_sub(1);
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child(format!("{} / {count}", index + 1)),
                        )
                        .child(
                            Button::new("next-photo")
                                .icon(AppIcon::ArrowRight)
                                .label("下一张")
                                .disabled(index + 1 == count)
                                .on_click(cx.listener(|s, _, _, cx| {
                                    if let Some(preview) = &mut s.photo_preview {
                                        preview.index =
                                            (preview.index + 1).min(preview.paths.len() - 1);
                                        cx.notify();
                                    }
                                })),
                        ),
                ),
        )
    }

    fn overview(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let wide = workspace_content_width(window) >= 760.;
        let publishing_accounts = self.snapshot.accounts.iter().filter(|account| {
            self.snapshot
                .can_prepare(account.platform, MediaKind::Video)
        });
        let publishing_account_count = publishing_accounts.clone().count();
        let verified_accounts = publishing_accounts
            .filter(|account| account.session == SessionStatus::Ready)
            .count();
        let asset_count = self.snapshot.material_sets.len();
        let needs_attention = |job: &Job| {
            matches!(
                job.state,
                JobState::WaitingUser | JobState::Unknown | JobState::Failed
            )
        };
        let attention_count = self
            .snapshot
            .jobs
            .iter()
            .filter(|job| needs_attention(job))
            .count();
        let active_count = self
            .snapshot
            .jobs
            .iter()
            .filter(|job| {
                !needs_attention(job) && (job.state.active() || job.state == JobState::Queued)
            })
            .count();
        let (heading, detail, action, icon, destination) = if publishing_account_count == 0 {
            (
                "添加一个发布账号",
                "在账号管理选择平台，添加账号并登录。",
                "前往账号管理",
                AppIcon::Users,
                Page::Accounts,
            )
        } else if verified_accounts == 0 {
            (
                "验证发布账号",
                "在账号管理中登录或检查会话，即可确认账号是否可用。",
                "检查账号",
                AppIcon::UserCheck,
                Page::Accounts,
            )
        } else if asset_count == 0 {
            (
                "导入要发布的素材",
                "一次选择文本、视频和图片，组成一个素材集。",
                "打开素材库",
                AppIcon::FolderOpen,
                Page::Assets,
            )
        } else {
            (
                "准备下一篇作品",
                "选择发布账号与素材，在提交前核对作品内容。",
                "新建发布",
                AppIcon::SquarePen,
                Page::Compose,
            )
        };
        let next_style = ButtonCustomVariant::new(cx)
            .color(rgb(SURFACE).into())
            .foreground(rgb(0x27345d).into())
            .hover(rgb(0xe9edff).into())
            .active(rgb(0xdbe1ff).into());
        let next_step = v_flex()
            .min_w_0()
            .justify_center()
            .gap_3()
            .p_6()
            .rounded(px(14.))
            .bg(linear_gradient(
                112.,
                linear_color_stop(rgb(0x202c52), 0.),
                linear_color_stop(rgb(0x414897), 1.),
            ))
            .text_color(rgb(SURFACE))
            .child(icon_tile(icon, 36., NAV_ACCENT, 0x35436d))
            .child(
                div()
                    .text_size(px(25.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(heading),
            )
            .child(div().text_sm().text_color(rgb(0xe0e5ff)).child(detail))
            .child(
                h_flex().mt_1().child(
                    Button::new("overview-next")
                        .custom(next_style)
                        .bg(rgb(SURFACE))
                        .label(action)
                        .icon(AppIcon::ArrowRight)
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.page = destination;
                            cx.notify();
                        })),
                ),
            );
        let metric = |label: &'static str, value: String, icon, ink, background| {
            h_flex()
                .min_w_0()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(rgb(CANVAS))
                .when(!wide, |row| row.flex_1())
                .child(icon_tile(icon, 32., ink, background))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(label),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
        };
        let activity_header = h_flex()
            .flex_shrink_0()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("任务动态"),
                    )
                    .when(attention_count > 0, |heading| {
                        heading.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x8b4f10))
                                .child(format!("{attention_count} 项任务待处理")),
                        )
                    }),
            )
            .child(
                Button::new("overview-jobs")
                    .ghost()
                    .small()
                    .label("查看任务")
                    .icon(AppIcon::ArrowRight)
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.navigate(Page::Jobs);
                        cx.notify();
                    })),
            );
        let activity = surface().flex_1().min_h_0().gap_3().child(activity_header);
        let activity = if self.snapshot.jobs.is_empty() {
            activity.child(
                h_flex()
                    .gap_3()
                    .p_3()
                    .rounded(px(10.))
                    .bg(rgb(CANVAS))
                    .child(icon_tile(AppIcon::Clock3, 36., MUTED, SURFACE))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_weight(FontWeight::MEDIUM).child("暂无任务"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child("准备任务的进度和平台结果会显示在这里。"),
                            ),
                    ),
            )
        } else {
            let (mut attention, others): (Vec<_>, Vec<_>) = (0..self.snapshot.jobs.len())
                .partition(|&index| needs_attention(&self.snapshot.jobs[index]));
            attention.extend(others);
            activity.child(
                uniform_list(
                    "overview-activity",
                    attention.len(),
                    cx.processor(move |view, range: Range<usize>, _, _| {
                        range
                            .map(|index| {
                                let job = &view.snapshot.jobs[attention[index]];
                                h_flex()
                                    .w_full()
                                    .h(px(64.))
                                    .justify_between()
                                    .gap_4()
                                    .border_t_1()
                                    .border_color(rgb(BORDER))
                                    .child(
                                        h_flex()
                                            .min_w_0()
                                            .flex_1()
                                            .gap_3()
                                            .child(icon_tile(
                                                media_icon(job.content.asset.kind),
                                                36.,
                                                ACCENT,
                                                ACCENT_WASH,
                                            ))
                                            .child(
                                                v_flex()
                                                    .min_w_0()
                                                    .flex_1()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .child(job.content.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(rgb(MUTED))
                                                            .child(format!(
                                                                "{} · {}",
                                                                job.account.platform.name(),
                                                                job.account.alias
                                                            )),
                                                    ),
                                            ),
                                    )
                                    .child(state_pill(job.state))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .w_full()
                .flex_1()
                .min_h_0(),
            )
        };
        let metrics = surface()
            .justify_center()
            .gap_2()
            .p_3()
            .when(wide, |panel| panel.w(px(306.)).flex_shrink_0())
            .when(!wide, |panel| panel.flex_row().items_stretch())
            .child(metric(
                "已验证账号",
                format!("{verified_accounts} / {publishing_account_count}"),
                AppIcon::Users,
                ACCENT,
                ACCENT_WASH,
            ))
            .child(metric(
                "素材集",
                asset_count.to_string(),
                AppIcon::FolderOpen,
                0x16775c,
                0xe8f7ef,
            ))
            .child(metric(
                "进行中任务",
                active_count.to_string(),
                AppIcon::ListChecks,
                0x985812,
                0xfff1de,
            ));
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_4()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_stretch()
                    .gap_4()
                    .when(!wide, |row| row.flex_col())
                    .child(next_step.when(wide, |panel| panel.flex_1()))
                    .child(metrics),
            )
            .child(activity)
            .into_any_element()
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.browser_loaded && !self.snapshot.browser.is_empty() {
            self.browser.update(cx, |input, cx| {
                input.set_value(self.snapshot.browser.clone(), window, cx)
            });
            self.browser_loaded = true;
        }
        if matches!(self.page, Page::Accounts | Page::Jobs) {
            for (id, preview) in &self.snapshot.sign_ins {
                let platform = self
                    .snapshot
                    .accounts
                    .iter()
                    .find(|account| account.id == *id)
                    .map(|account| account.platform);
                if preview.publish_sms.is_some()
                    || preview.wechat_channels.is_some()
                    || platform == Some(Platform::Kuaishou)
                {
                    continue;
                }
                let Some(verification_id) = preview.verification_id else {
                    continue;
                };
                self.verification_inputs
                    .entry(id.clone())
                    .or_insert_with(|| {
                        let input = cx.new(|cx| {
                            InputState::new(window, cx).placeholder(
                                if platform == Some(Platform::Xiaohongshu) {
                                    "手机号或短信验证码"
                                } else {
                                    "验证码或网页要求的文字"
                                },
                            )
                        });
                        let account = id.clone();
                        let subscription = cx.subscribe(&input, move |s, input, event, cx| {
                            if matches!(event, InputEvent::PressEnter { .. }) {
                                s.fill_verification(&account, verification_id, &input, cx);
                            }
                        });
                        (input, subscription)
                    });
            }
        }
        match self.page {
            Page::Overview => self.overview(window, cx),
            Page::Accounts => self.accounts(window, cx),
            Page::Assets => self.material_library(cx).into_any_element(),
            Page::Compose => self.compose(window, cx),
            Page::Jobs => {
                let jobs: Vec<_> = self
                    .snapshot
                    .jobs
                    .iter()
                    .filter(|job| self.job_in_scope(job))
                    .cloned()
                    .collect();
                let filter = self.job_account_filter.as_ref();
                let scoped = filter.is_some();
                let account = filter
                    .and_then(|filter| self.snapshot.accounts.iter().find(|a| a.id == filter.id));
                let heading = filter.map_or_else(
                    || format!("全部任务 · {}", jobs.len()),
                    |filter| account.map_or(&filter.alias, |a| &a.alias).clone(),
                );
                let scope_detail = filter.map(|filter| {
                    let platform = filter.platform.name();
                    let count = jobs.len();
                    account
                        .and_then(|a| self.duplicate_alias_suffix(a))
                        .map_or_else(
                            || format!("{platform} · {count} 项任务"),
                            |id| format!("{platform} · {id} · {count} 项任务"),
                        )
                });
                let active = jobs.iter().filter(|job| job.state.active()).count();
                let waiting = jobs
                    .iter()
                    .filter(|job| job.state == JobState::Queued)
                    .count();
                let queue_count = |label, count, ink| {
                    h_flex()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(rgb(CANVAS))
                        .child(div().text_sm().text_color(rgb(MUTED)).child(label))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(ink))
                                .child(format!("{count}")),
                        )
                };
                let mut view = v_flex()
                    .gap_3()
                    .when(scoped, |view| {
                        view.child(
                            Button::new("account-jobs-back")
                                .small()
                                .ghost()
                                .self_start()
                                .w_auto()
                                .icon(AppIcon::ArrowLeft)
                                .label(if account.is_some() {
                                    "返回账号详情"
                                } else {
                                    "返回账号管理"
                                })
                                .on_click(cx.listener(|s, _, _, cx| {
                                    if let Some(filter) = s.job_account_filter.take() {
                                        s.navigate(Page::Accounts);
                                        if s.snapshot.accounts.iter().any(|a| a.id == filter.id) {
                                            s.open_account(filter.id);
                                        }
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .child(
                        surface().p_4().child(
                            h_flex()
                                .justify_between()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_3()
                                        .child(icon_tile(
                                            AppIcon::ListChecks,
                                            36.,
                                            ACCENT,
                                            ACCENT_WASH,
                                        ))
                                        .child(
                                            v_flex()
                                                .min_w_0()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(heading),
                                                )
                                                .when_some(scope_detail, |labels, detail| {
                                                    labels.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(MUTED))
                                                            .child(detail),
                                                    )
                                                }),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .flex_shrink_0()
                                        .gap_2()
                                        .child(queue_count("进行中", active, ACCENT))
                                        .child(queue_count("排队中", waiting, INK)),
                                ),
                        ),
                    );
                if jobs.is_empty() {
                    view = view.child(
                        surface()
                            .items_center()
                            .py_10()
                            .gap_3()
                            .child(icon_tile(AppIcon::ListChecks, 52., ACCENT, ACCENT_WASH))
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                                if scoped {
                                    "该账号暂无任务"
                                } else {
                                    "暂无任务"
                                },
                            ))
                            .when(!scoped, |empty| {
                                empty
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(MUTED))
                                            .child("选择账号和素材，创建准备任务。"),
                                    )
                                    .child(
                                        Button::new("jobs-compose")
                                            .primary()
                                            .label("新建发布")
                                            .icon(AppIcon::Plus)
                                            .on_click(cx.listener(|s, _, _, cx| {
                                                s.navigate(Page::Compose);
                                                cx.notify();
                                            })),
                                    )
                            }),
                    );
                } else if jobs.iter().any(|job| {
                    job.state == JobState::Unknown
                        || (job.state == JobState::Queued
                            && !self.snapshot.scheduled_jobs.contains(&job.id))
                }) {
                    view =
                        view.child(
                            div().px_2().text_sm().text_color(rgb(MUTED)).child(
                                "重启后可手动继续排队任务。结果待确认的任务需要先到平台核对。",
                            ),
                        );
                }
                for j in jobs {
                    let cancel = j.id.clone();
                    let scheduled = self.snapshot.scheduled_jobs.contains(&j.id);
                    let can_submit = self.snapshot.can_submit(&j);
                    let confirming_publish =
                        can_submit && self.confirming_publish.as_ref() == Some(&j.id);
                    let account_removed =
                        !self.snapshot.accounts.iter().any(|a| a.id == j.account.id);
                    let mut card = surface()
                        .child(
                            h_flex()
                                .justify_between()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .min_w_0()
                                        .gap_3()
                                        .child(icon_tile(
                                            media_icon(j.content.asset.kind),
                                            40.,
                                            ACCENT,
                                            ACCENT_WASH,
                                        ))
                                        .child(
                                            v_flex()
                                                .min_w_0()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(j.content.title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_xs()
                                                        .text_color(rgb(MUTED))
                                                        .child(format!(
                                                            "{} · {} · {}",
                                                            j.account.alias,
                                                            j.account.platform.name(),
                                                            if j.content.asset.kind
                                                                == MediaKind::Images
                                                            {
                                                                "图文"
                                                            } else {
                                                                "视频"
                                                            }
                                                        )),
                                                ),
                                        ),
                                )
                                .child(state_pill(j.state)),
                        )
                        .child(
                            div().text_sm().text_color(rgb(MUTED)).child(
                                if scheduled && j.state == JobState::Queued {
                                    "已排队，有空位后自动开始准备。".into()
                                } else if j.state == JobState::WaitingUser
                                    && j.ready_to_submit
                                    && !self
                                        .snapshot
                                        .submit_enabled_platforms
                                        .contains(&j.account.platform)
                                {
                                    format!(
                                        "准备完成，当前版本暂未开放{}提交。",
                                        j.account.platform.name()
                                    )
                                } else {
                                    j.message.clone()
                                },
                            ),
                        );
                    if account_removed {
                        card = card.child("账号已删除");
                    }
                    if j.state == JobState::Unknown
                        && let Some(management_url) = creator_content_management(j.account.platform)
                    {
                        let platform = j.account.platform.name();
                        let confirming_not_published =
                            self.confirming_not_published.as_ref() == Some(&j.id);
                        let job_id = j.id.clone();
                        card =
                            card.child(
                                h_flex()
                                    .items_center()
                                    .flex_wrap()
                                    .gap_3()
                                    .child(
                                        Button::new(eid("open-platform-content", &j.id))
                                            .label(creator_management_label(j.account.platform))
                                            .icon(AppIcon::ArrowUpRight)
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.open_url(management_url);
                                            })),
                                    )
                                    .when(!account_removed && !confirming_not_published, |row| {
                                        row.child(
                                            Button::new(eid("review-unpublished", &j.id))
                                                .label("确认未发布")
                                                .on_click(cx.listener(move |s, _, _, cx| {
                                                    s.confirming_not_published =
                                                        Some(job_id.clone());
                                                    cx.notify();
                                                })),
                                        )
                                    })
                                    .child(div().text_sm().text_color(rgb(MUTED)).child(format!(
                                        "请用任务卡片上的{platform}账号核对作品。"
                                    ))),
                            );
                        if !account_removed && confirming_not_published {
                            let job_id = j.id.clone();
                            card = card.child(
                                v_flex()
                                    .gap_3()
                                    .p_4()
                                    .rounded(px(10.))
                                    .bg(rgb(0xfff4e5))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("确认核对结果"),
                                    )
                                    .child(div().text_sm().child(format!(
                                        "我已在「{}」的{platform}内容管理中核对，确认《{}》没有发布或审核中的记录。",
                                        j.account.alias, j.content.title
                                    )))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(MUTED))
                                            .child("保存后会出现「重新准备」，可由你手动开始。"),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                Button::new(eid("cancel-review-unpublished", &j.id))
                                                    .label("返回")
                                                    .on_click(cx.listener(|s, _, _, cx| {
                                                        s.confirming_not_published = None;
                                                        cx.notify();
                                                    })),
                                            )
                                            .child(
                                                Button::new(eid("save-review-unpublished", &j.id))
                                                    .primary()
                                                    .label("保存未发布结果")
                                                    .on_click(cx.listener(move |s, _, _, cx| {
                                                        s.confirming_not_published = None;
                                                        s.backend.send(Command::ConfirmNotPublished(job_id.clone()));
                                                        cx.notify();
                                                    })),
                                            ),
                                    ),
                            );
                        }
                    }
                    if j.state == JobState::Uploading {
                        let progress = self
                            .snapshot
                            .upload_progress
                            .get(&j.id)
                            .copied()
                            .unwrap_or(UploadProgress::Uploading { percent: None });
                        card =
                            card.child(upload_progress_panel(&j.id, j.account.platform, progress));
                    }
                    if let Some(sms) = self.publish_sms_panel(&j, window, cx) {
                        card = card.child(sms);
                    } else if matches!(j.state, JobState::WaitingUser | JobState::Submitting)
                        && let Some(sign_in) = self.sign_in_panel(&j.account.id, &j.id, cx)
                    {
                        card = card.child(sign_in);
                    }
                    if let Some(events) = self.snapshot.events.get(&j.id)
                        && !events.is_empty()
                    {
                        let mut timeline = v_flex().gap_2().p_3().rounded(px(10.)).bg(rgb(CANVAS));
                        for event in events.iter().rev().take(4).rev() {
                            timeline = timeline.child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .child(
                                        div()
                                            .size(px(5.))
                                            .flex_shrink_0()
                                            .rounded_full()
                                            .bg(rgb(ACCENT)),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(event.state.label()),
                                    )
                                    .child(
                                        div().text_color(rgb(MUTED)).child(event.message.clone()),
                                    ),
                            );
                        }
                        card = card.child(timeline);
                    }
                    if let Some(receipt) = &j.result {
                        let mut receipt_view = v_flex()
                            .items_start()
                            .gap_2()
                            .p_3()
                            .rounded(px(10.))
                            .bg(rgb(0xeef8f3))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x147154))
                                    .child(format!("{}平台回执", j.account.platform.name())),
                            )
                            .child(div().text_sm().child(receipt.evidence.clone()));
                        if !receipt.platform_id.is_empty() {
                            receipt_view = receipt_view.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(format!("平台编号 {}", receipt.platform_id)),
                            );
                        }
                        let receipt_link = receipt
                            .url
                            .as_ref()
                            .filter(|url| match j.account.platform {
                                Platform::Douyin => {
                                    url.starts_with("https://www.douyin.com/video/")
                                        || url.starts_with("https://www.douyin.com/note/")
                                }
                                Platform::Kuaishou => {
                                    url.starts_with("https://www.kuaishou.com/short-video/")
                                }
                                Platform::Xiaohongshu => {
                                    url.starts_with("https://www.xiaohongshu.com/explore/")
                                }
                                Platform::WechatChannels | Platform::Simulation => false,
                            })
                            .map(|url| ("打开作品链接".to_owned(), url.clone()))
                            .or_else(|| {
                                creator_content_management(j.account.platform).map(|url| {
                                    (creator_management_label(j.account.platform), url.to_owned())
                                })
                            });
                        if let Some((label, url)) = receipt_link {
                            receipt_view = receipt_view.child(
                                Button::new(eid("receipt-link", &j.id))
                                    .label(label)
                                    .icon(AppIcon::ArrowUpRight)
                                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                            );
                        }
                        card = card.child(receipt_view);
                    }
                    if j.state.active() || j.state == JobState::Queued {
                        let mut actions = h_flex().gap_3().child(
                            Button::new(eid("cancel-job", &j.id))
                                .label("停止任务")
                                .on_click(cx.listener(move |s, _, _, _| {
                                    s.backend.send(Command::Cancel(cancel.clone()))
                                })),
                        );
                        if can_submit && !confirming_publish {
                            let id = j.id.clone();
                            actions = actions.child(
                                Button::new(eid("submit-job", &j.id))
                                    .primary()
                                    .label(format!("发布到{}", j.account.platform.name()))
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.confirming_publish = Some(id.clone());
                                        cx.notify();
                                    })),
                            );
                        }
                        card = card.child(actions);
                    }
                    if confirming_publish {
                        let id = j.id.clone();
                        let account = self
                            .snapshot
                            .accounts
                            .iter()
                            .find(|account| account.id == j.account.id)
                            .unwrap_or(&j.account);
                        let identity = account
                            .nickname
                            .as_ref()
                            .filter(|nickname| nickname.as_str() != account.alias.as_str())
                            .map_or_else(
                                || account.alias.clone(),
                                |nickname| format!("{}（平台昵称 {nickname}）", account.alias),
                            );
                        let action = if j.account.platform == Platform::Kuaishou {
                            "立即公开发布"
                        } else {
                            "发布"
                        };
                        card = card.child(
                            v_flex()
                                .gap_3()
                                .p_4()
                                .rounded(px(10.))
                                .bg(rgb(ACCENT_WASH))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("确认发布到{}", j.account.platform.name())),
                                )
                                .child(div().text_sm().child(format!(
                                    "用「{identity}」{action}《{}》",
                                    j.content.title
                                )))
                                .when(
                                    j.account.platform == Platform::Douyin
                                        && !j.content.douyin_video_covers.is_empty(),
                                    |panel| {
                                        let covers = &j.content.douyin_video_covers;
                                        panel.child(
                                            h_flex().gap_3().flex_wrap().children(
                                                [
                                                    ("竖版封面", covers.portrait.as_ref()),
                                                    ("横版封面", covers.landscape.as_ref()),
                                                ]
                                                .into_iter()
                                                .filter_map(|(label, image)| {
                                                    let image = image?;
                                                    Some(
                                                        v_flex()
                                                            .gap_1()
                                                            .child(
                                                                img(image.path.clone())
                                                                    .w(px(84.))
                                                                    .h(px(58.))
                                                                    .rounded_lg()
                                                                    .bg(rgb(SURFACE))
                                                                    .object_fit(ObjectFit::Contain),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(rgb(MUTED))
                                                                    .child(label),
                                                            ),
                                                    )
                                                }),
                                            ),
                                        )
                                    },
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new(eid("cancel-publish", &j.id))
                                                .label("返回")
                                                .on_click(cx.listener(|s, _, _, cx| {
                                                    s.confirming_publish = None;
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            Button::new(eid("confirm-publish", &j.id))
                                                .primary()
                                                .label("确认发布")
                                                .on_click(cx.listener(move |s, _, _, cx| {
                                                    s.confirming_publish = None;
                                                    s.backend.send(Command::Submit(id.clone()));
                                                    cx.notify();
                                                })),
                                        ),
                                ),
                        );
                    }
                    let can_resume = !scheduled
                        && !account_removed
                        && self
                            .snapshot
                            .can_prepare(j.account.platform, j.content.asset.kind)
                        && matches!(
                            j.state,
                            JobState::Queued | JobState::Failed | JobState::Cancelled
                        );
                    let can_delete = j.state.can_delete();
                    let confirming_delete = self.confirming_delete_job.as_ref() == Some(&j.id);
                    if can_delete && confirming_delete {
                        card = card.child(self.confirm_delete_job(&j, cx));
                    } else if can_resume || can_delete {
                        let id = j.id.clone();
                        let mut actions = h_flex().items_center().flex_wrap().gap_2();
                        if can_resume {
                            actions = actions.child(
                                Button::new(eid("resume", &id))
                                    .label(if j.state == JobState::Queued {
                                        "继续排队任务"
                                    } else {
                                        "重新准备（保留执行记录）"
                                    })
                                    .on_click(cx.listener(move |s, _, _, _| {
                                        s.backend.send(Command::Resume(id.clone()))
                                    })),
                            );
                        }
                        if j.state == JobState::Failed
                            && j.account.platform == Platform::Douyin
                            && !j.content.douyin_video_covers.is_empty()
                        {
                            actions = actions.child(
                                Button::new(eid("cover-open-creator", &j.id))
                                    .ghost()
                                    .icon(AppIcon::ArrowUpRight)
                                    .label("打开抖音创作者中心")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.open_url(DOUYIN_UPLOAD_URL);
                                    })),
                            );
                        }
                        if can_delete {
                            let id = j.id.clone();
                            actions = actions.child(
                                Button::new(eid("delete-job", &id))
                                    .ghost()
                                    .icon(AppIcon::Trash)
                                    .label("删除任务")
                                    .accessibility_label(format!("删除任务 {}", j.content.title))
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.confirming_delete_job = Some(id.clone());
                                        cx.notify();
                                    })),
                            );
                        }
                        card = card.child(actions);
                    }
                    view = view.child(card);
                }
                view.into_any_element()
            }
            Page::Settings => {
                let wide = workspace_content_width(window) >= 810.;
                let setting_title = |icon, title, detail: SharedString| {
                    h_flex()
                        .items_start()
                        .gap_3()
                        .child(icon_tile(icon, 40., ACCENT, ACCENT_WASH))
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(div().text_sm().text_color(rgb(MUTED)).child(detail)),
                        )
                };
                let browser_field = h_flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.browser)))
                    .child(
                        Button::new("save-browser")
                            .primary()
                            .label("保存路径")
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.backend.send(Command::SetBrowser(
                                    s.browser.read(cx).value().to_string(),
                                ));
                            })),
                    );
                let browser_panel = surface()
                    .when(wide, |panel| panel.flex_1())
                    .child(setting_title(
                        AppIcon::PanelTop,
                        "后台浏览器",
                        "使用 Google Chrome 在后台处理，登录入口在应用内。".into(),
                    ))
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Chrome 程序路径"),
                            )
                            .child(browser_field),
                    );
                let queue_panel = surface()
                    .when(wide, |panel| panel.w(px(270.)).flex_shrink_0())
                    .child(setting_title(
                        AppIcon::ListChecks,
                        "任务队列",
                        format!(
                            "不同账号最多同时执行 {} 个任务，同一账号按顺序执行。每项任务都需单独确认发布。",
                            publisher_core::MAX_CONCURRENT_JOBS
                        ).into(),
                    ));
                h_flex()
                    .items_stretch()
                    .gap_4()
                    .when(!wide, |row| row.flex_col())
                    .child(browser_panel)
                    .child(queue_panel)
                    .into_any_element()
            }
        }
    }

    fn confirm_delete_job(&self, job: &Job, cx: &mut Context<Self>) -> Div {
        let id = job.id.clone();
        v_flex()
            .gap_3()
            .p_4()
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(0xf3d9df))
            .bg(rgb(0xfff6f7))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("从任务列表删除《{}》？", job.content.title)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(eid("cancel-delete-job", &job.id))
                            .label("取消删除")
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.confirming_delete_job = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(eid("confirm-delete-job", &job.id))
                            .danger()
                            .label("确认删除")
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.confirming_delete_job = None;
                                s.backend.send(Command::DeleteJob(id.clone()));
                                cx.notify();
                            })),
                    ),
            )
    }
}
fn new_key() -> String {
    format!("{}-{:?}", std::process::id(), std::time::SystemTime::now())
}
impl Render for Publisher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = v_flex()
            .w(px(SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .px_3()
            .py_6()
            .gap_2()
            .bg(rgb(NAV_SURFACE))
            .child(
                h_flex()
                    .gap_3()
                    .px_2()
                    .mb_5()
                    .child(img(self.product_icon.clone()).size(px(36.)).flex_shrink_0())
                    .child(
                        div()
                            .text_size(px(20.))
                            .text_color(rgb(SURFACE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(PRODUCT_NAME),
                    ),
            );
        for (index, page) in [
            Page::Overview,
            Page::Accounts,
            Page::Assets,
            Page::Compose,
            Page::Jobs,
            Page::Settings,
        ]
        .into_iter()
        .enumerate()
        {
            let active = self.page == page;
            let ink = if active { SURFACE } else { NAV_INK };
            let icon_ink = if active { NAV_ACCENT } else { 0xa9b6d2 };
            let nav_style = ButtonCustomVariant::new(cx)
                .color(rgb(if active { NAV_SELECTED } else { NAV_SURFACE }).into())
                .foreground(rgb(ink).into())
                .hover(rgb(if active { NAV_SELECTED } else { NAV_HOVER }).into())
                .active(rgb(NAV_SELECTED).into());
            sidebar = sidebar
                .when(page == Page::Settings, |rail| rail.child(div().flex_1()))
                .child(
                    Button::new(("nav", index))
                        .accessibility_label(page.title())
                        .custom(nav_style)
                        .when(active, |button| button.bg(rgb(NAV_SELECTED)))
                        .w_full()
                        .h(px(40.))
                        .rounded(px(9.))
                        .child(
                            h_flex()
                                .w_full()
                                .gap_3()
                                .child(glyph(page.icon(), 17., icon_ink))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_sm()
                                        .font_weight(if active {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::MEDIUM
                                        })
                                        .child(page.title()),
                                ),
                        )
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.navigate(page);
                            cx.notify();
                        })),
                );
        }
        let scroll_id = match self.page {
            Page::Accounts => self
                .account_details
                .as_ref()
                .map(|details| eid("account-detail-scroll", &details.id)),
            Page::Assets => self
                .material_details
                .as_ref()
                .map(|id| eid("material-detail-scroll", id)),
            Page::Jobs => self
                .job_account_filter
                .as_ref()
                .map(|account| eid("account-jobs-scroll", &account.id)),
            _ => None,
        }
        .unwrap_or_else(|| ElementId::from(("page-scroll", self.page as usize)));
        let photo_gallery = self.photo_gallery(window, cx);
        let previewing_photos = photo_gallery.is_some();
        let body = photo_gallery.map_or_else(|| self.body(window, cx), Div::into_any_element);
        let fill_viewport = self.page == Page::Overview && !previewing_photos;
        let sms_notice = self.publish_sms_notice(cx);
        let has_notice = sms_notice.is_some() || !self.message.is_empty();
        let mut workspace = v_flex().flex_1().h_full().min_w_0();
        if let Some(sms_notice) = sms_notice {
            workspace = workspace.child(workspace_frame().pt_3().flex_shrink_0().child(sms_notice));
        }
        if !self.message.is_empty() {
            workspace = workspace.child(
                workspace_frame().pt_3().flex_shrink_0().child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(ACCENT_WASH))
                        .child(glyph(AppIcon::Info, 16., ACCENT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(rgb(INK))
                                .child(self.message.clone()),
                        )
                        .child(
                            Button::new("dismiss-message")
                                .ghost()
                                .xsmall()
                                .icon(AppIcon::X)
                                .accessibility_label("关闭提示")
                                .on_click(cx.listener(|s, _, _, cx| {
                                    s.message.clear();
                                    cx.notify();
                                })),
                        ),
                ),
            );
        }
        workspace = workspace.child(
            div()
                .id(scroll_id)
                .flex_1()
                .min_h_0()
                .when(!fill_viewport, |viewport| viewport.overflow_y_scroll())
                .child(
                    workspace_frame()
                        .when(fill_viewport, |frame| frame.h_full().min_h_0())
                        .pt(px(if has_notice { 12. } else { 24. }))
                        .pb_6()
                        .child(body),
                ),
        );
        if self.page == Page::Compose && !previewing_photos {
            workspace = workspace.child(self.compose_footer(cx));
        }

        h_flex()
            .size_full()
            .font_family("PingFang SC")
            .text_size(px(14.))
            .bg(rgb(CANVAS))
            .text_color(rgb(INK))
            .child(sidebar)
            .child(workspace)
    }
}
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--browser-smoke") {
        return browser_smoke();
    }
    let root = std::env::var_os("PUBLISHER_DATA_DIR")
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(Store::default_root)?;
    let store = Store::open(root)?;
    let _instance = store.lease("application")?;
    store.recover()?;
    let backend = Backend::new(store);
    let shutdown = backend.tx.clone();
    let exit = backend.tx.clone();
    let finished = backend.finished.clone();
    gpui_kit::application()
        .with_assets(AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            install_theme(cx);
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus(vec![Menu {
                name: PRODUCT_NAME.into(),
                items: vec![MenuItem::action("退出 Airs", Quit)],
                disabled: false,
            }]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.on_app_quit(move |_| {
                let _ = shutdown.send(Command::Shutdown);
                async {}
            })
            .detach();
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::centered(size(px(1160.), px(820.)), cx)),
                    window_min_size: Some(size(px(880.), px(600.))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_window_title(PRODUCT_NAME);
                    cx.new(|cx| Publisher::new(backend, window, cx))
                },
            )
            .expect("打开原生窗口失败");
            cx.activate(true);
        });
    let _ = exit.send(Command::Shutdown);
    let _ = finished.1.wait_timeout_while(
        finished.0.lock().unwrap(),
        Duration::from_secs(15),
        |done| !*done,
    );
    Ok(())
}
fn browser_smoke() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let path = std::env::temp_dir().join(format!("publisher-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        let mut session = publisher_platforms::browser::BrowserSession::launch(&path).await?;
        let page = session.browser.new_page("about:blank").await?;
        page.evaluate("document.title='Rust CDP 正常'; 1+1").await?;
        anyhow::ensure!(
            page.get_title().await? == Some("Rust CDP 正常".into()),
            "CDP 验证失败"
        );
        session.close().await?;
        std::fs::remove_dir_all(path)?;
        println!("Chrome 启动、CDP 事件处理和退出验证通过");
        Ok(())
    })
}
fn eid(prefix: &str, id: &str) -> ElementId {
    SharedString::from(format!("{prefix}-{id}")).into()
}
