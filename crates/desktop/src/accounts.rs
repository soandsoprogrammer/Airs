use super::*;
use gpui_kit::component::{input::Textarea, tooltip::Tooltip};

pub(super) struct AccountDetails {
    pub(super) id: String,
    action: Option<AccountAction>,
}

enum AccountAction {
    Rename,
    Delete,
    EditNote {
        input: Entity<TextareaState>,
        saving: bool,
    },
}

impl Publisher {
    pub(super) fn open_account(&mut self, id: String) {
        if let Some(account) = self
            .snapshot
            .accounts
            .iter()
            .find(|account| account.id == id)
            && self.account_filter.is_some()
        {
            self.account_filter = Some(account.platform);
        }
        self.account_details = Some(AccountDetails { id, action: None });
    }

    pub(super) fn accounts(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(details) = &self.account_details
            && let Some(account) = self.snapshot.accounts.iter().find(|a| a.id == details.id)
        {
            return self
                .account_detail(account, details.action.as_ref(), cx)
                .into_any_element();
        }

        let accounts: Vec<_> = self
            .snapshot
            .accounts
            .iter()
            .filter(|account| self.account_filter.is_none_or(|p| account.platform == p))
            .collect();
        let content_width = workspace_content_width(window);
        let platforms = v_flex()
            .gap_3()
            .child(
                div()
                    .grid()
                    .grid_cols(ACCOUNT_PLATFORMS.len() as u16 + 1)
                    .gap_2p5()
                    .children(
                        std::iter::once(None)
                            .chain(ACCOUNT_PLATFORMS.into_iter().map(Some))
                            .map(|platform| self.account_platform_card(platform, cx)),
                    ),
            )
            .when_some(self.account_filter, |platforms, platform| {
                platforms.child(
                    h_flex().justify_end().child(
                        Button::new(eid("add-account", platform.name()))
                            .small()
                            .primary()
                            .w_auto()
                            .h(px(34.))
                            .icon(AppIcon::Plus)
                            .label("添加账号")
                            .on_click(cx.listener(move |s, _, _, _| {
                                s.backend.send(Command::AddAccount(platform));
                            })),
                    ),
                )
            });
        let view = v_flex().gap_4().child(platforms);
        if accounts.is_empty() {
            let heading = self.account_filter.map_or("还没有账号".into(), |p| {
                format!("还没有{}账号", p.name())
            });
            let hint = if self.account_filter.is_none() {
                "选择一个平台后即可添加账号。"
            } else {
                "添加账号后，可在应用内登录。"
            };
            return view
                .child(
                    surface()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .p_4()
                        .child(icon_tile(AppIcon::Users, 40., ACCENT, ACCENT_WASH))
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_1()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(heading))
                                .child(div().text_sm().text_color(rgb(MUTED)).child(hint)),
                        ),
                )
                .into_any_element();
        }
        let columns = ((content_width / 235.) as u16).clamp(1, 4);
        let style = ButtonCustomVariant::new(cx)
            .color(rgb(BORDER).into())
            .hover(rgb(CANVAS).into())
            .active(rgb(ACCENT_WASH).into());
        view.child(
            div().grid().grid_cols(columns).gap_3().children(
                accounts
                    .into_iter()
                    .map(|account| self.account_card(account, style, cx)),
            ),
        )
        .into_any_element()
    }

    fn account_platform_card(&self, platform: Option<Platform>, cx: &mut Context<Self>) -> Button {
        let selected = self.account_filter == platform;
        let label = platform.map_or("全部", Platform::name);
        let accessible_label = platform.map_or("全部账号", Platform::name);
        let count = self
            .snapshot
            .accounts
            .iter()
            .filter(|account| platform.is_none_or(|p| account.platform == p))
            .count();
        let ink = if selected { SURFACE } else { INK };
        let background = if selected { NAV_SELECTED } else { 0xe1e8f4 };
        let icon = platform.map_or_else(
            || {
                icon_tile(
                    AppIcon::Users,
                    26.,
                    if selected { NAV_INK } else { ACCENT },
                    if selected { NAV_HOVER } else { 0xd6def0 },
                )
            },
            |platform| platform_brand_icon(platform, 26.),
        );
        let style = ButtonCustomVariant::new(cx)
            .color(rgb(background).into())
            .foreground(rgb(ink).into())
            .hover(rgb(if selected { NAV_SELECTED } else { 0xd3dced }).into())
            .active(rgb(background).into());

        Button::new(eid("account-filter", label))
            .custom(style)
            .toggled(selected)
            .bg(rgb(background))
            .text_color(rgb(ink))
            .w_full()
            .min_w_0()
            .h(px(74.))
            .px_2p5()
            .rounded(px(10.))
            .accessibility_label(format!("{accessible_label}，{count} 个账号"))
            .child(
                h_flex().w_full().min_w_0().gap_2().child(icon).child(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(label),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(if selected { NAV_INK } else { MUTED }))
                                .child(format!("{count} 个账号")),
                        ),
                ),
            )
            .on_click(cx.listener(move |s, _, _, cx| {
                s.account_filter = platform;
                cx.notify();
            }))
    }

    fn account_card(
        &self,
        account: &Account,
        style: ButtonCustomVariant,
        cx: &mut Context<Self>,
    ) -> Button {
        let (status, status_ink, status_wash) = self.account_status(account);
        let short_id = self.duplicate_alias_suffix(account);
        let mut label = format!("{} · {} · {status}", account.alias, account.platform.name());
        if let Some(short_id) = &short_id {
            label.push_str(&format!(" · {short_id}"));
        }
        let id = account.id.clone();
        Button::new(eid("account-card", &id))
            .custom(style)
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(12.))
            .w_full()
            .min_w_0()
            .h(px(112.))
            .px_3()
            .py_2()
            .accessibility_label(label)
            .child(
                v_flex()
                    .w_full()
                    .h_full()
                    .min_w_0()
                    .justify_between()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(platform_brand_icon(account.platform, 34.))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(account.alias.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(account.platform.name()),
                                    ),
                            )
                            .child(glyph(AppIcon::ChevronRight, 14., MUTED)),
                    )
                    .when_some(account_note_preview(account), |card, note| card.child(note))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(account_status_pill(status, status_ink, status_wash))
                            .when_some(short_id, |row, id| {
                                row.child(
                                    div().truncate().text_xs().text_color(rgb(MUTED)).child(id),
                                )
                            }),
                    ),
            )
            .on_click(cx.listener(move |s, _, _, cx| {
                s.open_account(id.clone());
                cx.notify();
            }))
    }

    pub(super) fn duplicate_alias_suffix(&self, account: &Account) -> Option<String> {
        self.snapshot
            .accounts
            .iter()
            .any(|other| {
                other.id != account.id
                    && other.alias == account.alias
                    && other.platform == account.platform
            })
            .then(|| account.id.chars().take(8).collect())
    }

    pub(super) fn account_status(&self, account: &Account) -> (&'static str, u32, u32) {
        if let Some(preview) = self.snapshot.sign_ins.get(&account.id) {
            if preview.publish_sms.is_some() {
                return ("等待发布验证", 0x895315, 0xfff3df);
            }
            if account.platform == Platform::WechatChannels
                && preview.verification_id.is_some()
                && let Some(mode) = &preview.wechat_channels
            {
                let status = match mode {
                    WechatChannelsPreview::Choice { .. } => "请选择登录方式",
                    WechatChannelsPreview::WaitingForDesktop => "等待桌面微信确认",
                    WechatChannelsPreview::SelectChannel { .. } => "请选择视频号",
                };
                return (status, ACCENT, ACCENT_WASH);
            }
            return match preview.status {
                SignInStatus::Loading if account.platform == Platform::Xiaohongshu => {
                    ("正在打开登录页", ACCENT, ACCENT_WASH)
                }
                SignInStatus::Loading => ("正在获取二维码", ACCENT, ACCENT_WASH),
                SignInStatus::WaitingForScan => ("等待扫码", ACCENT, ACCENT_WASH),
                SignInStatus::ConfirmOnPhone if account.platform == Platform::WechatChannels => {
                    ("等待微信确认", 0x895315, 0xfff3df)
                }
                SignInStatus::ConfirmOnPhone => ("等待手机确认", 0x895315, 0xfff3df),
                SignInStatus::Expired => ("二维码已过期", 0x895315, 0xfff3df),
                SignInStatus::VerifyInApp if account.platform == Platform::Kuaishou => {
                    ("请选择登录角色", 0x895315, 0xfff3df)
                }
                SignInStatus::VerifyInApp if account.platform == Platform::Xiaohongshu => {
                    ("等待登录", 0x895315, 0xfff3df)
                }
                SignInStatus::VerifyInApp => ("需要安全验证", 0x895315, 0xfff3df),
                SignInStatus::CheckingAccount => ("正在确认登录", ACCENT, ACCENT_WASH),
            };
        }
        if self.snapshot.logins.contains(&account.id) {
            return ("正在检查登录状态", ACCENT, ACCENT_WASH);
        }
        match account.session {
            SessionStatus::Ready => ("已验证", 0x176c47, 0xe9f7ef),
            SessionStatus::NeedsLogin => ("需要登录", 0x895315, 0xfff3df),
            SessionStatus::Unchecked => ("尚未检查", MUTED, 0xf0f2f6),
        }
    }

    fn account_detail(
        &self,
        account: &Account,
        action: Option<&AccountAction>,
        cx: &mut Context<Self>,
    ) -> Div {
        let (status, status_ink, status_wash) = self.account_status(account);
        let capability = if self
            .snapshot
            .submit_enabled_platforms
            .contains(&account.platform)
        {
            "支持发布"
        } else if self
            .snapshot
            .can_prepare(account.platform, MediaKind::Video)
        {
            "支持作品准备"
        } else {
            "支持账号登录"
        };
        let busy = self.snapshot.logins.contains(&account.id);
        let pending_job = self.snapshot.jobs.iter().any(|job| {
            job.account.id == account.id && (job.state.active() || job.state == JobState::Queued)
        });
        let delete_blocked = busy || pending_job;
        let id = account.id.clone();
        let login = if busy {
            Button::new(eid("cancel-login", &id))
                .small()
                .label("取消登录")
                .on_click(cx.listener(move |s, _, _, _| {
                    s.backend.send(Command::CancelLogin(id.clone()));
                }))
        } else {
            let label = if account.session == SessionStatus::Ready {
                "检查会话"
            } else if account.platform == Platform::Xiaohongshu {
                "登录小红书"
            } else if account.platform == Platform::WechatChannels {
                "登录视频号"
            } else {
                "扫码登录"
            };
            Button::new(eid("login", &id))
                .small()
                .primary()
                .label(label)
                .disabled(self.snapshot.sign_ins.contains_key(&id))
                .on_click(cx.listener(move |s, _, _, _| {
                    s.backend.send(Command::Login(id.clone()));
                }))
        };
        let job_account = account.clone();
        let mut view = v_flex()
            .gap_3()
            .child(
                Button::new("back-to-accounts")
                    .small()
                    .ghost()
                    .self_start()
                    .w_auto()
                    .icon(AppIcon::ArrowLeft)
                    .label("返回账号列表")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.account_details = None;
                        cx.notify();
                    })),
            )
            .child(
                surface()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .flex_wrap()
                            .gap_3()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_3()
                                    .child(platform_brand_icon(account.platform, 44.))
                                    .child(
                                        v_flex()
                                            .min_w_0()
                                            .flex_1()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_lg()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(account.alias.clone()),
                                            )
                                            .child(
                                                h_flex()
                                                    .flex_wrap()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(MUTED))
                                                            .child(format!(
                                                                "{} · {capability}",
                                                                account.platform.name()
                                                            )),
                                                    )
                                                    .child(account_status_pill(
                                                        status,
                                                        status_ink,
                                                        status_wash,
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        Button::new("view-account-jobs")
                                            .small()
                                            .icon(AppIcon::ListChecks)
                                            .label("查看发布任务")
                                            .on_click(cx.listener(move |s, _, _, cx| {
                                                s.open_account_jobs(&job_account);
                                                cx.notify();
                                            })),
                                    )
                                    .child(login),
                            ),
                    )
                    .when_some(
                        self.sign_in_panel(&account.id, &account.id, cx),
                        |panel, sign_in| panel.child(sign_in),
                    ),
            )
            .child(self.account_note(account, action, cx));

        let old_alias = account.alias.clone();
        let mut profile = surface()
            .gap_3()
            .p_4()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("账号资料"))
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(px(8.))
                    .bg(rgb(CANVAS))
                    .child(account_field(
                        "平台昵称",
                        account.nickname.as_deref().unwrap_or("登录后获取"),
                    ))
                    .child(account_field("账号编号", &account.id)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("rename-account")
                            .small()
                            .label("修改名称")
                            .on_click(cx.listener(move |s, _, window, cx| {
                                if let Some(details) = &mut s.account_details {
                                    details.action = Some(AccountAction::Rename);
                                }
                                s.alias.update(cx, |input, cx| {
                                    input.set_value(old_alias.clone(), window, cx);
                                    input.focus(window, cx);
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("delete-account")
                            .small()
                            .danger()
                            .outline()
                            .label("删除账号")
                            .disabled(delete_blocked)
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(details) = &mut s.account_details {
                                    details.action = Some(AccountAction::Delete);
                                    cx.notify();
                                }
                            })),
                    ),
            );
        if delete_blocked {
            profile = profile.child(div().text_xs().text_color(rgb(MUTED)).child(if busy {
                "结束登录检查后可删除账号。"
            } else {
                "结束该账号的排队或执行任务后可删除账号。"
            }));
        }
        match action {
            Some(AccountAction::Rename) => {
                profile = profile.child(self.rename_account(account, cx))
            }
            Some(AccountAction::Delete) => {
                profile = profile.child(self.delete_account(account, delete_blocked, cx))
            }
            Some(AccountAction::EditNote { .. }) | None => {}
        }
        view = view.child(profile);
        view
    }

    fn account_note(
        &self,
        account: &Account,
        action: Option<&AccountAction>,
        cx: &mut Context<Self>,
    ) -> Div {
        let heading = div().font_weight(FontWeight::SEMIBOLD).child("备注");
        let card = surface().gap_3().p_4();
        if let Some(AccountAction::EditNote { input, saving }) = action {
            let account_id = account.id.clone();
            return card
                .child(heading)
                .child(
                    Textarea::new(input)
                        .h(px(112.))
                        .disabled(*saving)
                        .aria_label("账号备注"),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(format!("最多 {ACCOUNT_NOTE_LIMIT} 字，留空保存可清除备注")),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("cancel-account-note")
                                        .small()
                                        .label("取消编辑")
                                        .disabled(*saving)
                                        .on_click(cx.listener(|s, _, _, cx| {
                                            s.clear_account_action();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("save-account-note")
                                        .small()
                                        .primary()
                                        .label(if *saving {
                                            "保存中…"
                                        } else {
                                            "保存备注"
                                        })
                                        .disabled(*saving)
                                        .on_click(cx.listener(move |s, _, _, cx| {
                                            s.save_account_note(&account_id, cx);
                                        })),
                                ),
                        ),
                );
        }

        let note = account.note.clone();
        let empty = note.is_empty();
        card.child(
            h_flex().justify_between().gap_2().child(heading).child(
                Button::new("edit-account-note")
                    .small()
                    .label(if empty {
                        "添加备注"
                    } else {
                        "编辑备注"
                    })
                    .on_click(cx.listener(move |s, _, window, cx| {
                        let Some(details) = &mut s.account_details else {
                            return;
                        };
                        let input =
                            cx.new(|cx| TextareaState::new(window, cx).placeholder("输入账号备注"));
                        input.update(cx, |input, cx| {
                            input.set_value(note.clone(), window, cx);
                            input.focus(window, cx);
                        });
                        details.action = Some(AccountAction::EditNote {
                            input,
                            saving: false,
                        });
                        cx.notify();
                    })),
            ),
        )
        .child(
            div()
                .text_sm()
                .whitespace_normal()
                .text_color(rgb(if empty { MUTED } else { INK }))
                .child(if empty {
                    "暂无备注".into()
                } else {
                    account.note.clone()
                }),
        )
    }

    fn save_account_note(&mut self, account_id: &str, cx: &mut Context<Self>) {
        let Some(details) = self.account_details.as_mut().filter(|d| d.id == account_id) else {
            return;
        };
        let Some(AccountAction::EditNote { input, saving }) = &mut details.action else {
            return;
        };
        if *saving {
            return;
        }
        let note = input.read(cx).value().to_string();
        let clearing = note.trim().is_empty();
        if !clearing && note.chars().count() > ACCOUNT_NOTE_LIMIT {
            self.message = format!("备注最多 {ACCOUNT_NOTE_LIMIT} 字");
            cx.notify();
            return;
        }
        let input_id = input.entity_id();
        *saving = true;
        let (result, response) = tokio::sync::oneshot::channel();
        self.backend.send(Command::SaveAccountNote {
            id: account_id.to_owned(),
            note: if clearing { String::new() } else { note },
            result,
        });
        cx.spawn(async move |this, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err("无法保存备注，请重试".into()));
            let _ = this.update(cx, |view, cx| {
                if let Some(details) = &mut view.account_details
                    && let Some(AccountAction::EditNote { input, saving }) = &mut details.action
                    && input.entity_id() == input_id
                {
                    if result.is_ok() {
                        details.action = None;
                    } else {
                        *saving = false;
                    }
                }
                view.message = match result {
                    Ok(()) if clearing => "备注已清除".into(),
                    Ok(()) => "备注已保存".into(),
                    Err(message) => message,
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn rename_account(&self, account: &Account, cx: &mut Context<Self>) -> Div {
        let id = account.id.clone();
        v_flex()
            .gap_2()
            .p_4()
            .rounded(px(10.))
            .bg(rgb(CANVAS))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("显示名称"),
            )
            .child(Input::new(&self.alias))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-name")
                            .small()
                            .primary()
                            .label("保存名称")
                            .on_click(cx.listener(move |s, _, _, cx| {
                                let name = s.alias.read(cx).value().to_string();
                                if name.trim().is_empty() {
                                    s.message = "显示名称不能为空".into();
                                } else {
                                    s.backend.send(Command::Rename(id.clone(), name));
                                    s.clear_account_action();
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("cancel-name")
                            .small()
                            .label("取消修改")
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.clear_account_action();
                                cx.notify();
                            })),
                    ),
            )
    }

    fn delete_account(&self, account: &Account, blocked: bool, cx: &mut Context<Self>) -> Div {
        let id = account.id.clone();
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
                    .child(format!("删除「{}」及本机登录资料？", account.alias)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x76535e))
                    .child("历史任务仍可在任务记录中查看。"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cancel-delete-account")
                            .small()
                            .label("取消删除")
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.clear_account_action();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("confirm-delete-account")
                            .small()
                            .label("确认删除")
                            .danger()
                            .disabled(blocked)
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.clear_account_action();
                                s.backend.send(Command::DeleteAccount(id.clone()));
                                cx.notify();
                            })),
                    ),
            )
    }

    fn clear_account_action(&mut self) {
        if let Some(details) = &mut self.account_details {
            details.action = None;
        }
    }
}

pub(super) fn platform_icon(platform: Platform) -> (AppIcon, u32, u32) {
    match platform {
        Platform::Douyin => (AppIcon::Music2, ACCENT, ACCENT_WASH),
        Platform::Kuaishou => (AppIcon::Video, 0xaf480c, 0xffefdf),
        Platform::Xiaohongshu => (AppIcon::BookOpen, 0xb52c4b, 0xffe9ef),
        Platform::WechatChannels => (AppIcon::Video, 0xa85408, 0xfff0d9),
        Platform::Simulation => (AppIcon::Users, MUTED, CANVAS),
    }
}

fn account_note_preview(account: &Account) -> Option<Div> {
    let preview = account
        .note
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if preview.is_empty() {
        return None;
    }
    let full_note: SharedString = account.note.clone().into();
    let text = StyledText::new(preview.clone());
    let layout = text.layout().clone();
    Some(
        div()
            .w_full()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_color(rgb(MUTED))
            .child(
                InteractiveText::new(eid("account-note-preview", &account.id), text).tooltip(
                    move |_, window, cx| {
                        (layout.text() != preview).then(|| {
                            let note = full_note.clone();
                            Tooltip::element(move |_, _| {
                                div()
                                    .min_w_0()
                                    .max_w(px(440.))
                                    .whitespace_normal()
                                    .child(note.clone())
                            })
                            .p_2()
                            .build(window, cx)
                        })
                    },
                ),
            ),
    )
}

fn account_status_pill(label: &'static str, ink: u32, wash: u32) -> Div {
    h_flex()
        .flex_shrink_0()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded_full()
        .bg(rgb(wash))
        .text_color(rgb(ink))
        .text_xs()
        .child(div().size(px(5.)).rounded_full().bg(rgb(ink)))
        .child(label)
}

fn account_field(label: &'static str, value: &str) -> Div {
    h_flex()
        .gap_3()
        .text_sm()
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(div().min_w_0().child(value.to_owned()))
}
