use super::*;

pub(super) struct AccountDetails {
    pub(super) id: String,
    action: Option<AccountAction>,
}

#[derive(Clone, Copy)]
enum AccountAction {
    Rename,
    Delete,
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
                .account_detail(account, details.action, cx)
                .into_any_element();
        }

        let mut filters = h_flex().gap_1().flex_wrap();
        for platform in std::iter::once(None).chain(ACCOUNT_PLATFORMS.into_iter().map(Some)) {
            let label = platform.map_or("全部账号", Platform::name);
            let selected = self.account_filter == platform;
            filters = filters.child(
                Button::new(eid("account-filter", label))
                    .small()
                    .h(px(32.))
                    .rounded(px(8.))
                    .ghost()
                    .label(label)
                    .toggled(selected)
                    .when(selected, |button| {
                        button.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.account_filter = platform;
                        cx.notify();
                    })),
            );
        }
        let view = v_flex().gap_4().child(
            h_flex()
                .items_center()
                .justify_between()
                .flex_wrap()
                .gap_3()
                .child(filters)
                .when_some(self.account_filter, |toolbar, platform| {
                    toolbar.child(
                        Button::new(eid("add-account", platform.name()))
                            .icon(AppIcon::Plus)
                            .label("添加账号")
                            .on_click(cx.listener(move |s, _, _, _| {
                                s.backend.send(Command::AddAccount(platform));
                            })),
                    )
                }),
        );
        let accounts: Vec<_> = self
            .snapshot
            .accounts
            .iter()
            .filter(|a| self.account_filter.is_none_or(|p| a.platform == p))
            .collect();
        if accounts.is_empty() {
            let heading = self.account_filter.map_or("还没有账号".into(), |p| {
                format!("还没有{}账号", p.name())
            });
            let hint = if self.account_filter.is_none() {
                "先选择一个平台标签，再添加账号。"
            } else {
                "添加账号后，可在应用内登录。"
            };
            return view
                .child(
                    surface()
                        .items_center()
                        .gap_2()
                        .py_8()
                        .child(icon_tile(AppIcon::Users, 44., ACCENT, ACCENT_WASH))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(heading))
                        .child(div().text_sm().text_color(rgb(MUTED)).child(hint)),
                )
                .into_any_element();
        }
        let content_width = workspace_content_width(window);
        let columns = ((content_width / 260.) as u16).clamp(1, 4);
        let style = ButtonCustomVariant::new(cx)
            .color(rgb(SURFACE).into())
            .hover(rgb(0xf3f1ff).into())
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

    fn account_card(
        &self,
        account: &Account,
        style: ButtonCustomVariant,
        cx: &mut Context<Self>,
    ) -> Button {
        let (icon, ink, wash) = platform_icon(account.platform);
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
            .rounded(px(14.))
            .w_full()
            .min_w_0()
            .h(px(108.))
            .px_4()
            .accessibility_label(label)
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(icon_tile(icon, 36., ink, wash))
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
                            .child(glyph(AppIcon::ChevronRight, 16., MUTED)),
                    )
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
            return match preview.status {
                SignInStatus::Loading if account.platform == Platform::Xiaohongshu => {
                    ("正在打开登录页", ACCENT, ACCENT_WASH)
                }
                SignInStatus::Loading => ("正在获取二维码", ACCENT, ACCENT_WASH),
                SignInStatus::WaitingForScan => ("等待扫码", ACCENT, ACCENT_WASH),
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
        action: Option<AccountAction>,
        cx: &mut Context<Self>,
    ) -> Div {
        let (icon, ink, wash) = platform_icon(account.platform);
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
                .label("取消登录")
                .on_click(cx.listener(move |s, _, _, _| {
                    s.backend.send(Command::CancelLogin(id.clone()));
                }))
        } else {
            let label = if account.session == SessionStatus::Ready {
                "检查会话"
            } else if account.platform == Platform::Xiaohongshu {
                "登录小红书"
            } else {
                "扫码登录"
            };
            Button::new(eid("login", &id))
                .primary()
                .label(label)
                .disabled(self.snapshot.sign_ins.contains_key(&id))
                .on_click(cx.listener(move |s, _, _, _| {
                    s.backend.send(Command::Login(id.clone()));
                }))
        };
        let mut view = v_flex()
            .gap_4()
            .child(
                div().child(
                    Button::new("back-to-accounts")
                        .ghost()
                        .icon(AppIcon::ArrowLeft)
                        .label("返回账号列表")
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.account_details = None;
                            cx.notify();
                        })),
                ),
            )
            .child(
                surface().child(
                    h_flex()
                        .gap_3()
                        .child(icon_tile(icon, 48., ink, wash))
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(20.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(account.alias.clone()),
                                )
                                .child(
                                    div().text_sm().text_color(rgb(MUTED)).child(format!(
                                        "{} · {capability}",
                                        account.platform.name()
                                    )),
                                ),
                        ),
                ),
            )
            .child(
                surface()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .flex_wrap()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        div().font_weight(FontWeight::SEMIBOLD).child("登录状态"),
                                    )
                                    .child(account_status_pill(status, status_ink, status_wash)),
                            )
                            .child(login),
                    )
                    .when_some(
                        self.sign_in_panel(&account.id, &account.id, cx),
                        |panel, sign_in| panel.child(sign_in),
                    ),
            );

        let old_alias = account.alias.clone();
        let mut profile =
            surface()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("账号资料"))
                .child(
                    v_flex()
                        .gap_2()
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
                        .child(Button::new("rename-account").label("修改名称").on_click(
                            cx.listener(move |s, _, window, cx| {
                                if let Some(details) = &mut s.account_details {
                                    details.action = Some(AccountAction::Rename);
                                }
                                s.alias.update(cx, |input, cx| {
                                    input.set_value(old_alias.clone(), window, cx);
                                    input.focus(window, cx);
                                });
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("delete-account")
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
            None => {}
        }
        view = view.child(profile);
        view
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
        Platform::Simulation => (AppIcon::Users, MUTED, CANVAS),
    }
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
