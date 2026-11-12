mod accounts;
mod appearance;
mod backend;
mod compose;
mod video_preview;
extern crate gpui_kit as gpui;
use accounts::AccountDetails;
use appearance::surface;
use appearance::*;
use backend::*;
use compose::{asset_format, asset_name, media_icon};
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
use publisher_platforms::sign_in::{SignInPreview, SignInStatus, VerificationAction};
use publisher_platforms::upload_progress::UploadProgress;
use publisher_platforms::xiaohongshu::CREATOR_MANAGE_URL as XIAOHONGSHU_MANAGE_URL;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use video_preview::VideoPreview;
gpui_kit::actions!(publisher, [Quit]);

const PRODUCT_NAME: &str = "Airs";
const SIDEBAR_WIDTH: f32 = 206.;
const WORKSPACE_MAX_WIDTH: f32 = 1240.;
const WORKSPACE_PADDING: f32 = 28.;

fn workspace_content_width(window: &Window) -> f32 {
    (window.viewport_size().width.as_f32() - SIDEBAR_WIDTH).min(WORKSPACE_MAX_WIDTH)
        - WORKSPACE_PADDING * 2.
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
    publish_sms_input: Option<PublishSmsInput>,
    page: Page,
    message: String,
    alias: Entity<InputState>,
    title: Entity<InputState>,
    description: Entity<TextareaState>,
    tags: Entity<InputState>,
    browser: Entity<InputState>,
    selected_accounts: HashSet<String>,
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
    confirming_publish: Option<String>,
    confirming_not_published: Option<String>,
    confirming_delete_job: Option<String>,
    _subscriptions: Vec<Subscription>,
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
    verification_id: u64,
    input: Entity<InputState>,
    valid: bool,
    _subscription: Subscription,
}

fn valid_sms_code(value: &str) -> bool {
    let value = value.trim();
    (4..=8).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn creator_content_management(platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::Douyin => Some(CREATOR_MANAGE_URL),
        Platform::Kuaishou => Some(KUAISHOU_MANAGE_URL),
        Platform::Xiaohongshu => Some(XIAOHONGSHU_MANAGE_URL),
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
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if this
                    .update(cx, |s, cx| {
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
                                Update::ComposeAssetImported(id, kind)
                                    if s.page == Page::Compose && s.compose_media_kind == kind =>
                                {
                                    s.select_compose_asset(id);
                                    cx.notify();
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
            publish_sms_input: None,
            page: Page::Overview,
            message: String::new(),
            alias: input("修改此账号的显示名称", cx),
            title: input("作品标题", cx),
            description,
            tags: input("用空格分隔，最多 5 个标签", cx),
            browser: input("Chrome 可执行程序的完整路径", cx),
            selected_accounts: HashSet::new(),
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
        let name = platform.name();
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
                format!("打开{name} App 扫码，按手机上的提示完成登录。").into(),
                Some("刷新二维码"),
            ),
            SignInStatus::ConfirmOnPhone => (
                "已扫码，等待手机确认",
                format!("请在{name} App 确认这次登录。").into(),
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
        let mut panel = h_flex()
            .items_center()
            .flex_wrap()
            .gap_5()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(rgb(0xe2defb))
            .bg(rgb(0xf7f6ff));
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

    fn publish_sms_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let challenge = self.snapshot.jobs.iter().find_map(|job| {
            if job.state != JobState::Submitting || job.account.platform != Platform::Douyin {
                return None;
            }
            let preview = self.snapshot.sign_ins.get(&job.account.id)?;
            Some((
                job,
                preview.publish_sms.as_ref()?,
                preview.verification_id?,
                preview.input_feedback,
            ))
        });
        let Some((job, sms, verification_id, feedback)) = challenge else {
            self.publish_sms_input = None;
            return None;
        };

        if self
            .publish_sms_input
            .as_ref()
            .is_none_or(|field| field.verification_id != verification_id)
        {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("短信验证码")
                    .masked(true)
            });
            let subscription = cx.subscribe(&input, |view, input, event, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(field) = view.publish_sms_input.as_mut()
                    && field.input == input
                {
                    let valid = valid_sms_code(&input.read(cx).value());
                    if field.valid != valid {
                        field.valid = valid;
                        cx.notify();
                    }
                }
            });
            self.publish_sms_input = Some(PublishSmsInput {
                verification_id,
                input,
                valid: false,
                _subscription: subscription,
            });
        }
        let field = self.publish_sms_input.as_ref()?;
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
                .mx_7()
                .mt_4()
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
                                    Button::new("publish-sms-request")
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
                                            .role(Role::PasswordInput),
                                    ),
                                )
                                .child(
                                    Button::new("publish-sms-verify")
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
                                                if let Some(field) = s.publish_sms_input.as_mut()
                                                    && field.verification_id == verification_id
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

    fn pick_media(&mut self, kind: MediaKind, cx: &mut Context<Self>) {
        let select_for_compose = self.page == Page::Compose;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: kind == MediaKind::Images,
            prompt: Some(
                match kind {
                    MediaKind::Video => "选择本地视频",
                    MediaKind::Images => "选择本地图文图片",
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && !paths.is_empty()
            {
                let _ = this.update(cx, |s, _| {
                    s.backend.send(Command::Import {
                        paths,
                        kind,
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

    fn overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let publishing_accounts = self.snapshot.accounts.iter().filter(|account| {
            self.snapshot
                .can_prepare(account.platform, MediaKind::Video)
        });
        let publishing_account_count = publishing_accounts.clone().count();
        let verified_accounts = publishing_accounts
            .filter(|account| account.session == SessionStatus::Ready)
            .count();
        let asset_count = self.snapshot.assets.len();
        let needs_attention = |job: &&Job| {
            matches!(
                job.state,
                JobState::WaitingUser | JobState::Unknown | JobState::Failed
            )
        };
        let attention_count = self.snapshot.jobs.iter().filter(needs_attention).count();
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
                "从本机选择视频或图片，再填写作品内容。",
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
        let next_step = surface()
            .p_6()
            .bg(rgb(0xf0eeff))
            .border_color(rgb(0xe1dcfa))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .child(icon_tile(icon, 44., ACCENT, SURFACE))
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(heading),
                            )
                            .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
                            .child(
                                h_flex().gap_2().mt_1().flex_wrap().child(
                                    Button::new("overview-next")
                                        .primary()
                                        .label(action)
                                        .icon(AppIcon::ArrowRight)
                                        .on_click(cx.listener(move |s, _, _, cx| {
                                            s.page = destination;
                                            cx.notify();
                                        })),
                                ),
                            ),
                    ),
            );
        let metric = |label: &'static str, value: String, icon, ink, background| {
            surface().flex_1().gap_3().child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .child(icon_tile(icon, 34., ink, background))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().text_color(rgb(MUTED)).child(label))
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(value),
                            ),
                    ),
            )
        };
        let mut activity = surface().gap_3().child(
            h_flex()
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
                            s.page = Page::Jobs;
                            cx.notify();
                        })),
                ),
        );
        if self.snapshot.jobs.is_empty() {
            activity = activity.child(
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
            );
        } else {
            for job in self
                .snapshot
                .jobs
                .iter()
                .filter(needs_attention)
                .chain(
                    self.snapshot
                        .jobs
                        .iter()
                        .filter(|job| !needs_attention(job)),
                )
                .take(4)
            {
                activity = activity.child(
                    h_flex()
                        .justify_between()
                        .gap_4()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .pt_3()
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
                        .child(state_pill(job.state)),
                );
            }
        }
        v_flex()
            .gap_4()
            .child(next_step)
            .child(
                h_flex()
                    .items_stretch()
                    .gap_3()
                    .child(metric(
                        "已验证 / 发布账号",
                        format!("{verified_accounts} / {publishing_account_count}"),
                        AppIcon::Users,
                        ACCENT,
                        ACCENT_WASH,
                    ))
                    .child(metric(
                        "本地素材",
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
                    )),
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
                if preview.publish_sms.is_some() || platform == Some(Platform::Kuaishou) {
                    continue;
                }
                let Some(verification_id) = preview.verification_id else {
                    continue;
                };
                self.verification_inputs
                    .entry(id.clone())
                    .or_insert_with(|| {
                        let input = cx.new(|cx| {
                            InputState::new(window, cx)
                                .placeholder(if platform == Some(Platform::Xiaohongshu) {
                                    "手机号或短信验证码"
                                } else {
                                    "验证码或网页要求的文字"
                                })
                                .masked(true)
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
        let panel = surface;
        match self.page {
            Page::Overview => self.overview(cx),
            Page::Accounts => self.accounts(window, cx),
            Page::Assets => {
                let mut library =
                    panel().gap_0().child(
                        h_flex()
                            .flex_wrap()
                            .gap_4()
                            .justify_between()
                            .pb_4()
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                                        "本地素材 · {} 组",
                                        self.snapshot.assets.len()
                                    )))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(0x65718a))
                                            .child("文件保留在原位置，执行前会检查变化。"),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("import-video")
                                            .icon(AppIcon::Plus)
                                            .label("导入视频")
                                            .on_click(cx.listener(|s, _, _, cx| {
                                                s.pick_media(MediaKind::Video, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("import-photos")
                                            .primary()
                                            .icon(AppIcon::Plus)
                                            .label("导入图片")
                                            .on_click(cx.listener(|s, _, _, cx| {
                                                s.pick_media(MediaKind::Images, cx)
                                            })),
                                    ),
                            ),
                    );
                if self.snapshot.assets.is_empty() {
                    library = library.child(
                        v_flex()
                            .items_center()
                            .gap_2()
                            .py_10()
                            .px_5()
                            .rounded_lg()
                            .bg(rgb(0xf7f8fc))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(44.))
                                    .mb_1()
                                    .rounded_lg()
                                    .bg(rgb(0xeeecff))
                                    .text_color(rgb(0x5146d9))
                                    .child(Icon::new(AppIcon::Clapperboard)),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("还没有导入素材"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x65718a))
                                    .child("支持常见视频格式与 JPG、PNG、WebP 图片。"),
                            ),
                    );
                }
                for a in self.snapshot.assets.clone() {
                    let id = a.id.clone();
                    let name = asset_name(&a);
                    let path = a.path.to_string_lossy().into_owned();
                    let format = asset_format(&a);
                    library = library.child(
                        h_flex()
                            .gap_3()
                            .py_3()
                            .border_t_1()
                            .border_color(rgb(0xecedf3))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(38.))
                                    .flex_shrink_0()
                                    .rounded_lg()
                                    .bg(rgb(0xeeecff))
                                    .text_color(rgb(0x5146d9))
                                    .child(Icon::new(media_icon(a.kind))),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .text_color(rgb(0x65718a))
                                            .child(path.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(0xf2f3f8))
                                    .text_xs()
                                    .text_color(rgb(0x65718a))
                                    .flex_shrink_0()
                                    .child(format!(
                                        "{format} · {:.1} MB",
                                        a.size as f64 / 1_000_000.
                                    )),
                            )
                            .child(self.preview_button(&a, cx))
                            .child(
                                Button::new(eid("use", &a.id))
                                    .label("用于新建发布")
                                    .tooltip(path)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.select_compose_asset(id.clone());
                                        s.page = Page::Compose;
                                        cx.notify();
                                    })),
                            ),
                    );
                }
                library.into_any_element()
            }
            Page::Compose => self.compose(window, cx),
            Page::Jobs => {
                let active = self
                    .snapshot
                    .jobs
                    .iter()
                    .filter(|job| job.state.active())
                    .count();
                let waiting = self
                    .snapshot
                    .jobs
                    .iter()
                    .filter(|job| job.state == JobState::Queued)
                    .count();
                let mut view = v_flex().gap_4().child(
                    h_flex()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("全部任务 · {}", self.snapshot.jobs.len())),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child(format!("{active} 个进行中  ·  {waiting} 个排队中")),
                        ),
                );
                if self.snapshot.jobs.is_empty() {
                    view = view.child(
                        panel()
                            .items_center()
                            .py_10()
                            .gap_3()
                            .child(icon_tile(AppIcon::ListChecks, 52., ACCENT, ACCENT_WASH))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("暂无任务"),
                            )
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
                                        s.page = Page::Compose;
                                        cx.notify();
                                    })),
                            ),
                    );
                } else {
                    view = view.child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("重启后可手动继续排队任务。结果待确认的任务需要先到平台核对。"),
                    );
                }
                for j in self.snapshot.jobs.clone() {
                    let cancel = j.id.clone();
                    let can_submit = self.snapshot.can_submit(&j);
                    let confirming_publish =
                        can_submit && self.confirming_publish.as_ref() == Some(&j.id);
                    let account_removed =
                        !self.snapshot.accounts.iter().any(|a| a.id == j.account.id);
                    let mut card = panel()
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
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child(j.message.clone()),
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
                    if matches!(j.state, JobState::WaitingUser | JobState::Submitting)
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
                                Platform::Simulation => false,
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
                    let can_resume = !account_removed
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
                let setting_title = |icon, title, detail| {
                    h_flex()
                        .gap_3()
                        .child(icon_tile(icon, 40., ACCENT, ACCENT_WASH))
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_1()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
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
                let browser_panel = panel()
                    .child(setting_title(
                        AppIcon::PanelTop,
                        "后台浏览器",
                        "使用 Google Chrome 在后台处理，登录入口在应用内。",
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
                v_flex()
                    .gap_4()
                    .child(browser_panel)
                    .child(panel().child(setting_title(
                        AppIcon::ListChecks,
                        "任务队列",
                        "同一时间执行一个任务。准备完成后可在任务卡片查看提交操作。",
                    )))
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
            .py_5()
            .gap_1()
            .border_r_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .mb_5()
                    .child(img(self.product_icon.clone()).size(px(42.)).flex_shrink_0())
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(PRODUCT_NAME),
                            )
                            .child(div().text_xs().text_color(rgb(MUTED)).child("多平台发布")),
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
            let ink = if active { ACCENT } else { 0x414e66 };
            let nav_style = ButtonCustomVariant::new(cx)
                .color(rgb(if active { ACCENT_WASH } else { SURFACE }).into())
                .foreground(rgb(ink).into())
                .hover(rgb(if active { 0xe7e3ff } else { CANVAS }).into())
                .active(rgb(0xe1ddff).into());
            sidebar = sidebar.child(
                Button::new(("nav", index))
                    .accessibility_label(page.title())
                    .custom(nav_style)
                    .w_full()
                    .h(px(40.))
                    .rounded(px(9.))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .child(glyph(page.icon(), 17., ink))
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
                            )
                            .when(active, |row| {
                                row.child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))
                            }),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.page = page;
                        s.photo_preview = None;
                        s.account_details = None;
                        if page != Page::Jobs {
                            s.confirming_publish = None;
                            s.confirming_not_published = None;
                            s.confirming_delete_job = None;
                        }
                        cx.notify();
                    })),
            );
        }
        let account_details = self
            .account_details
            .as_ref()
            .filter(|_| self.page == Page::Accounts);
        let scroll_id = account_details.map_or_else(
            || ElementId::from(("page-scroll", self.page as usize)),
            |details| eid("account-detail-scroll", &details.id),
        );
        let photo_gallery = self.photo_gallery(window, cx);
        let previewing_photos = photo_gallery.is_some();
        let body = photo_gallery.map_or_else(|| self.body(window, cx), Div::into_any_element);
        let publish_sms = self.publish_sms_panel(window, cx);
        let mut workspace = v_flex().flex_1().h_full().min_w_0();
        if let Some(publish_sms) = publish_sms {
            workspace = workspace.child(publish_sms);
        }
        if !self.message.is_empty() {
            workspace = workspace.child(
                h_flex()
                    .mx_7()
                    .mt_4()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(0xe1def9))
                    .bg(rgb(0xf1effe))
                    .child(glyph(AppIcon::Info, 16., ACCENT))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(0x3e367e))
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
            );
        }
        workspace = workspace.child(
            div()
                .id(scroll_id)
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .w_full()
                        .max_w(px(WORKSPACE_MAX_WIDTH))
                        .mx_auto()
                        .px(px(WORKSPACE_PADDING))
                        .py_5()
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
