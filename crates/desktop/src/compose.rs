use super::*;
use crate::accounts::platform_icon;
use gpui_kit::component::input::Textarea;
use publisher_core::Asset;

#[derive(Clone, Copy)]
enum CoverOrientation {
    Portrait,
    Landscape,
}

impl CoverOrientation {
    fn label(self) -> &'static str {
        match self {
            Self::Portrait => "竖版封面",
            Self::Landscape => "横版封面",
        }
    }

    fn image(self, covers: &DouyinVideoCovers) -> Option<&ImageFile> {
        match self {
            Self::Portrait => covers.portrait.as_ref(),
            Self::Landscape => covers.landscape.as_ref(),
        }
    }
}

impl Publisher {
    pub(super) fn compose(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let wide = workspace_content_width(window) >= 760.;
        v_flex()
            .w_full()
            .gap_4()
            .child(self.compose_toolbar(cx))
            .child(
                div()
                    .flex()
                    .w_full()
                    .gap_4()
                    .when(wide, |layout| layout.flex_row().items_start())
                    .when(!wide, |layout| layout.flex_col())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_4()
                            .child(self.compose_media(cx))
                            .when(!wide, |layout| layout.child(self.compose_accounts(cx)))
                            .child(self.compose_fields(cx))
                            .when(self.has_selected_douyin_account(), |layout| {
                                layout.child(self.compose_douyin_settings(cx))
                            }),
                    )
                    .when(wide, |layout| {
                        layout.child(self.compose_accounts(cx).w(px(304.)).flex_shrink_0())
                    }),
            )
            .into_any_element()
    }

    fn compose_toolbar(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .justify_between()
            .flex_wrap()
            .gap_3()
            .child(
                h_flex()
                    .gap_1()
                    .p_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .children(
                        [(MediaKind::Video, "视频"), (MediaKind::Images, "图文")].map(
                            |(choice, label)| {
                                let selected = choice == self.compose_media_kind;
                                Button::new(eid("compose-kind", label))
                                    .small()
                                    .ghost()
                                    .h(px(30.))
                                    .w(px(88.))
                                    .icon(media_icon(choice))
                                    .label(label)
                                    .toggled(selected)
                                    .when(selected, |button| {
                                        button.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
                                    })
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        if s.compose_media_kind != choice {
                                            s.compose_media_kind = choice;
                                            s.selected_asset = None;
                                            s.compose_image_order.clear();
                                            s.compose_asset_picker_open = false;
                                            s.reset_compose_batch();
                                            cx.notify();
                                        }
                                    }))
                            },
                        ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("restore-draft")
                            .small()
                            .ghost()
                            .label("恢复草稿")
                            .disabled(self.snapshot.draft.is_none())
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.restore_compose_draft(window, cx);
                            })),
                    )
                    .child(
                        Button::new("save-draft")
                            .small()
                            .label("保存草稿")
                            .disabled(self.compose_selected_asset().is_none())
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(content) = s.content(cx) {
                                    s.backend.send(Command::SaveDraft(content));
                                }
                            })),
                    ),
            )
    }

    fn has_selected_douyin_account(&self) -> bool {
        self.snapshot.accounts.iter().any(|account| {
            account.platform == Platform::Douyin && self.selected_accounts.contains(&account.id)
        })
    }

    fn compose_douyin_settings(&self, cx: &mut Context<Self>) -> Div {
        let panel = surface().gap_3().child(section_title("抖音封面"));
        if self.compose_media_kind == MediaKind::Images {
            return panel.child(muted_text("图文以第一张图片作为封面。"));
        }
        panel.child(
            h_flex()
                .gap_3()
                .items_stretch()
                .child(self.compose_cover_slot(CoverOrientation::Portrait, cx))
                .child(self.compose_cover_slot(CoverOrientation::Landscape, cx)),
        )
    }

    fn compose_cover_slot(&self, orientation: CoverOrientation, cx: &mut Context<Self>) -> Div {
        let label = orientation.label();
        let image = orientation.image(&self.douyin_video_covers);
        let preview = if let Some(image) = image {
            let path = image.path.clone();
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            Button::new(eid("preview-douyin-cover", label))
                .ghost()
                .w_full()
                .h(px(88.))
                .p_0()
                .rounded_lg()
                .bg(rgb(SURFACE))
                .accessibility_label(format!("预览{label}，{filename}"))
                .tooltip(filename)
                .child(img(path.clone()).size_full().object_fit(ObjectFit::Contain))
                .on_click(cx.listener(move |s, _, _, cx| {
                    s.photo_preview = Some(PhotoPreview {
                        page: Page::Compose,
                        paths: vec![path.clone()],
                        index: 0,
                    });
                    cx.notify();
                }))
                .into_any_element()
        } else {
            v_flex()
                .items_center()
                .justify_center()
                .gap_1()
                .w_full()
                .h(px(88.))
                .rounded_lg()
                .bg(rgb(SURFACE))
                .child(glyph(AppIcon::Image, 18., MUTED))
                .child(muted_text("抖音默认"))
                .into_any_element()
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(CANVAS))
            .child(field_title(label))
            .child(preview)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        Button::new(eid("select-douyin-cover", label))
                            .small()
                            .label(if image.is_some() {
                                "更换图片"
                            } else {
                                "选择图片"
                            })
                            .accessibility_label(format!("选择{label}图片"))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.pick_douyin_cover(orientation, cx);
                            })),
                    )
                    .when(image.is_some(), |row| {
                        row.child(
                            Button::new(eid("clear-douyin-cover", label))
                                .small()
                                .ghost()
                                .label("恢复默认")
                                .accessibility_label(format!("恢复{label}默认设置"))
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.set_douyin_cover(orientation, None);
                                    cx.notify();
                                })),
                        )
                    }),
            )
    }

    fn pick_douyin_cover(&mut self, orientation: CoverOrientation, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(format!("选择{}", orientation.label()).into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let image = cx
                .background_executor()
                .spawn(async move { ImageFile::inspect(&path) })
                .await;
            let _ = this.update(cx, |s, cx| {
                if s.page == Page::Compose && s.compose_media_kind == MediaKind::Video {
                    match image {
                        Ok(image) => s.set_douyin_cover(orientation, Some(image)),
                        Err(error) => s.message = format!("{}：{error}", orientation.label()),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_douyin_cover(&mut self, orientation: CoverOrientation, image: Option<ImageFile>) {
        let slot = match orientation {
            CoverOrientation::Portrait => &mut self.douyin_video_covers.portrait,
            CoverOrientation::Landscape => &mut self.douyin_video_covers.landscape,
        };
        if *slot != image {
            *slot = image;
            self.reset_compose_batch();
        }
    }

    fn compose_media(&self, cx: &mut Context<Self>) -> Div {
        let kind = self.compose_media_kind;
        let picked = self.compose_ordered_asset();
        let available = self
            .snapshot
            .assets
            .iter()
            .filter(|asset| asset.kind == kind);
        let has_assets = available.clone().next().is_some();
        let (heading, import_label, empty_label, empty_hint) = match kind {
            MediaKind::Video => (
                "视频素材",
                "导入视频",
                "未选择视频",
                "支持 MP4、MOV、M4V 和 WebM。",
            ),
            MediaKind::Images => (
                "图文素材",
                "导入图片",
                "未选择图片",
                "可一次选择多张 JPG、PNG 或 WebP 图片。",
            ),
        };
        let browse_label = if self.compose_asset_picker_open {
            "收起素材库"
        } else if picked.is_some() {
            "更换素材"
        } else {
            "选择素材"
        };
        let mut panel = surface().gap_3().child(
            h_flex()
                .flex_wrap()
                .justify_between()
                .gap_2()
                .child(section_title(heading))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("compose-import-media")
                                .small()
                                .when(has_assets, |button| button.ghost())
                                .icon(AppIcon::Plus)
                                .label(import_label)
                                .on_click(cx.listener(move |s, _, _, cx| s.pick_media(kind, cx))),
                        )
                        .when(has_assets, |actions| {
                            actions.child(
                                Button::new("compose-toggle-library")
                                    .small()
                                    .icon(AppIcon::FolderOpen)
                                    .label(browse_label)
                                    .on_click(cx.listener(|s, _, _, cx| {
                                        s.compose_asset_picker_open = !s.compose_asset_picker_open;
                                        cx.notify();
                                    })),
                            )
                        }),
                ),
        );
        panel = panel.child(if let Some(asset) = picked.as_ref() {
            h_flex()
                .justify_between()
                .gap_3()
                .p_3()
                .rounded(px(10.))
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(CANVAS))
                .child(asset_label(asset).flex_1())
                .child(self.preview_button(asset, cx))
        } else {
            h_flex()
                .gap_3()
                .p_3()
                .rounded(px(10.))
                .bg(rgb(CANVAS))
                .child(icon_tile(media_icon(kind), 36., ACCENT, ACCENT_WASH))
                .child(
                    v_flex()
                        .gap_1()
                        .child(field_title(empty_label))
                        .child(muted_text(empty_hint)),
                )
        });
        if self.compose_asset_picker_open && has_assets {
            panel = panel.child(
                v_flex()
                    .id("compose-media-options")
                    .max_h(px(188.))
                    .overflow_y_scroll()
                    .gap_2()
                    .children(available.map(|asset| {
                        let id = asset.id.clone();
                        let selected = self.selected_asset.as_ref() == Some(&id);
                        compose_choice(eid("compose-asset", &id), selected)
                            .h(px(52.))
                            .accessibility_label(format!("选择素材 {}", asset_name(asset)))
                            .tooltip(asset.path.to_string_lossy().into_owned())
                            .child(asset_label(asset).flex_1())
                            .when(selected, |button| button.icon(AppIcon::Check))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.select_compose_asset(id.clone());
                                cx.notify();
                            }))
                    })),
            );
        }
        if let Some(asset) = picked.filter(|asset| asset.kind == MediaKind::Images) {
            panel = panel.child(
                v_flex()
                    .gap_1()
                    .child(field_title("图片顺序"))
                    .child(self.compose_image_sequence(&asset, cx)),
            );
        }
        panel
    }

    fn compose_image_sequence(&self, asset: &Asset, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("compose-image-sequence")
            .max_h(px(202.))
            .overflow_y_scroll()
            .gap_1()
            .children(asset.images.iter().enumerate().map(|(index, image)| {
                let name = image
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let count = asset.images.len();
                h_flex()
                    .gap_2()
                    .pr_2()
                    .child(
                        div()
                            .w(px(22.))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child((index + 1).to_string()),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .children(
                        [
                            (index.checked_sub(1), "上移", AppIcon::ArrowUp),
                            (
                                (index + 1 < count).then_some(index + 1),
                                "下移",
                                AppIcon::ArrowDown,
                            ),
                        ]
                        .map(|(destination, label, icon)| {
                            Button::new(eid(label, &index.to_string()))
                                .ghost()
                                .xsmall()
                                .icon(icon)
                                .tooltip(label)
                                .accessibility_label(format!("{label}第 {} 张图片", index + 1))
                                .disabled(destination.is_none())
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    if let Some(destination) = destination
                                        && index < s.compose_image_order.len()
                                        && destination < s.compose_image_order.len()
                                    {
                                        s.compose_image_order.swap(index, destination);
                                        s.reset_compose_batch();
                                        cx.notify();
                                    }
                                }))
                        }),
                    )
            }))
    }

    fn compose_fields(&self, cx: &mut Context<Self>) -> Div {
        let title = self.title.read(cx).value();
        let title = title.trim();
        let selected_platforms: HashSet<_> = self
            .snapshot
            .accounts
            .iter()
            .filter(|account| self.selected_accounts.contains(&account.id))
            .map(|account| account.platform)
            .collect();
        let kind = self.compose_media_kind;
        let counters: Vec<_> = ACCOUNT_PLATFORMS
            .into_iter()
            .filter(|platform| selected_platforms.contains(platform))
            .filter(|&platform| {
                !(kind == MediaKind::Video
                    && platform == Platform::Kuaishou
                    && selected_platforms.contains(&Platform::Douyin))
            })
            .collect();
        let counters = if counters.is_empty() {
            vec![Platform::Douyin]
        } else {
            counters
        };
        let counter = |platform: Platform| {
            let prefix = if counters.len() > 1 {
                format!("{} ", platform.name())
            } else {
                String::new()
            };
            let length = Content::title_length(title, platform, kind);
            let limit = Content::title_limit(platform, kind);
            div()
                .text_xs()
                .text_color(rgb(if length > limit { 0xa83646 } else { MUTED }))
                .child(format!("{prefix}{length} / {limit}"))
        };
        surface()
            .gap_4()
            .child(section_title("作品内容"))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex().justify_between().child(field_title("标题")).child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .children(counters.iter().copied().map(counter)),
                        ),
                    )
                    .child(Input::new(&self.title).aria_label("作品标题")),
            )
            .child(
                v_flex().gap_2().child(field_title("描述")).child(
                    Textarea::new(&self.description)
                        .h(px(112.))
                        .aria_label("作品描述"),
                ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(field_title("标签"))
                    .child(Input::new(&self.tags).aria_label("作品标签"))
                    .child(muted_text(
                        if selected_platforms.contains(&Platform::Kuaishou) {
                            "快手标题、描述与标签合计最多 500 字。标签无需加 #。"
                        } else {
                            "描述与标签合计最多 1000 字。标签无需加 #。"
                        },
                    )),
            )
    }

    fn compose_accounts(&self, cx: &mut Context<Self>) -> Div {
        let mut panel = surface().gap_3().child(
            h_flex()
                .justify_between()
                .gap_2()
                .child(section_title("发布账号"))
                .child(
                    Button::new("compose-manage-accounts")
                        .small()
                        .ghost()
                        .label("管理账号")
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.page = Page::Accounts;
                            cx.notify();
                        })),
                ),
        );
        if self.snapshot.accounts.is_empty() {
            return panel.child(muted_text("请先添加一个发布账号。"));
        }
        let mut filters = h_flex()
            .flex_wrap()
            .gap_1()
            .border_b_1()
            .border_color(rgb(BORDER))
            .pb_3();
        for platform in std::iter::once(None).chain(ACCOUNT_PLATFORMS.into_iter().map(Some)) {
            let label = platform.map_or("全部", Platform::name);
            let selected = self.compose_account_filter == platform;
            filters = filters.child(
                Button::new(eid("compose-account-filter", label))
                    .small()
                    .ghost()
                    .h(px(29.))
                    .toggled(selected)
                    .label(label)
                    .when(selected, |button| {
                        button.bg(rgb(ACCENT_WASH)).text_color(rgb(ACCENT))
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.compose_account_filter = platform;
                        cx.notify();
                    })),
            );
        }
        let filter_key = self.compose_account_filter.map_or("all", Platform::name);
        let mut choices = self
            .snapshot
            .accounts
            .iter()
            .filter(|account| {
                self.compose_account_filter
                    .is_none_or(|platform| platform == account.platform)
            })
            .peekable();
        panel = panel.child(filters);
        if choices.peek().is_none() {
            return panel.child(muted_text("这个平台还没有账号。"));
        }
        panel.child(
            v_flex()
                .id(eid("compose-account-options", filter_key))
                .max_h(px(320.))
                .overflow_y_scroll()
                .gap_2()
                .children(choices.map(|account| self.compose_account_choice(account, cx))),
        )
    }

    fn compose_account_choice(&self, account: &Account, cx: &mut Context<Self>) -> Button {
        let id = account.id.clone();
        let selected = self.selected_accounts.contains(&id);
        let (icon, ink, wash) = platform_icon(account.platform);
        let can_prepare = self
            .snapshot
            .can_prepare(account.platform, self.compose_media_kind);
        let (status, status_ink, _) = self.account_status(account);
        let (status, status_ink) = if can_prepare {
            (status, status_ink)
        } else if self.compose_media_kind == MediaKind::Images {
            ("暂不支持图文", MUTED)
        } else {
            ("发布准备待接入", MUTED)
        };
        let short_id = self.duplicate_alias_suffix(account);
        let platform = account.platform.name();
        let details = if let Some(short_id) = short_id {
            format!("{platform} · {status} · {short_id}")
        } else {
            format!("{platform} · {status}")
        };
        compose_choice(eid("compose-account", &id), selected)
            .h(px(51.))
            .disabled(!can_prepare && !selected)
            .accessibility_label(format!("{} · {details}", account.alias))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(icon_tile(icon, 28., ink, wash))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(account.alias.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(status_ink))
                                    .child(details),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_center()
                            .size(px(16.))
                            .flex_shrink_0()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(rgb(if selected { ACCENT } else { 0xcbd3e1 }))
                            .when(selected, |check| {
                                check
                                    .bg(rgb(ACCENT))
                                    .child(glyph(AppIcon::Check, 11., SURFACE))
                            }),
                    ),
            )
            .on_click(cx.listener(move |s, _, _, cx| {
                if !s.selected_accounts.remove(&id) {
                    s.selected_accounts.insert(id.clone());
                }
                s.reset_compose_batch();
                cx.notify();
            }))
    }

    pub(super) fn compose_footer(&self, cx: &mut Context<Self>) -> Div {
        let picked = self.compose_selected_asset();
        let account_count = self.selected_accounts.len();
        let title_empty = self.title.read(cx).value().trim().is_empty();
        let missing = if picked.is_none() {
            Some(match self.compose_media_kind {
                MediaKind::Video => "请选择视频",
                MediaKind::Images => "请选择图文图片",
            })
        } else if account_count == 0 {
            Some("请选择至少一个发布账号")
        } else if self.snapshot.accounts.iter().any(|account| {
            self.selected_accounts.contains(&account.id)
                && !self
                    .snapshot
                    .can_prepare(account.platform, self.compose_media_kind)
        }) {
            Some("所选账号中有平台暂不支持此类作品")
        } else if title_empty {
            Some("请填写作品标题")
        } else {
            None
        };
        let (label, caption) = if self.pending_batch.is_some() {
            ("正在创建任务", "正在保存本次发布任务")
        } else if self.queued {
            ("查看任务", "本批次已创建")
        } else {
            (
                "开始准备",
                missing.unwrap_or("上传素材后，等待你确认发布。"),
            )
        };
        let media = picked.map_or("未选素材".into(), asset_name);
        div()
            .flex_shrink_0()
            .border_t_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                workspace_frame()
                    .flex_row()
                    .items_center()
                    .py_3()
                    .gap_3()
                    .child(icon_tile(
                        media_icon(self.compose_media_kind),
                        36.,
                        ACCENT,
                        ACCENT_WASH,
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(format!("已选 {account_count} 个账号")),
                                    )
                                    .child(div().text_color(rgb(MUTED)).child("·"))
                                    .child(div().min_w_0().truncate().child(media)),
                            )
                            .child(muted_text(caption)),
                    )
                    .child(
                        Button::new("enqueue")
                            .primary()
                            .h(px(38.))
                            .w(px(124.))
                            .label(label)
                            .disabled(
                                self.pending_batch.is_some() || (missing.is_some() && !self.queued),
                            )
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.start_compose_batch(cx);
                                cx.notify();
                            })),
                    ),
            )
    }

    pub(super) fn compose_selected_asset(&self) -> Option<&Asset> {
        let id = self.selected_asset.as_ref()?;
        self.snapshot.assets.iter().find(|asset| &asset.id == id)
    }

    pub(super) fn select_compose_asset(&mut self, id: String) {
        self.compose_asset_picker_open = false;
        if self.selected_asset.as_ref() != Some(&id)
            && let Some(asset) = self.snapshot.assets.iter().find(|asset| asset.id == id)
        {
            self.compose_media_kind = asset.kind;
            self.compose_image_order = (0..asset.images.len()).collect();
            self.selected_asset = Some(id);
            self.reset_compose_batch();
        }
    }

    pub(super) fn compose_ordered_asset(&self) -> Option<Asset> {
        let mut asset = self.compose_selected_asset()?.clone();
        if asset.kind == MediaKind::Images {
            asset.images = self
                .compose_image_order
                .iter()
                .map(|&index| asset.images[index].clone())
                .collect();
            let first = asset.images.first()?;
            asset.path = first.path.clone();
            asset.modified_ms = first.modified_ms;
        }
        Some(asset)
    }

    pub(super) fn reset_compose_batch(&mut self) {
        self.queued = false;
        self.batch_key = new_key();
    }

    pub(super) fn compose_input_changed(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        if matches!(event, InputEvent::Change) {
            if self.queued || self.pending_batch.as_ref() == Some(&self.batch_key) {
                self.reset_compose_batch();
            }
            cx.notify();
        }
    }

    fn restore_compose_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = self.snapshot.draft.clone() {
            let content_changed = self.content(cx).as_ref() != Some(&draft);
            self.select_compose_asset(draft.asset.id.clone());
            self.douyin_video_covers = draft.douyin_video_covers;
            if draft.asset.kind == MediaKind::Images
                && let Some(asset) = self.compose_selected_asset()
            {
                let order: Option<Vec<_>> = draft
                    .asset
                    .images
                    .iter()
                    .map(|draft_image| asset.images.iter().position(|image| image == draft_image))
                    .collect();
                if let Some(order) = order.filter(|order| order.len() == asset.images.len()) {
                    self.compose_image_order = order;
                }
            }
            self.title
                .update(cx, |input, cx| input.set_value(draft.title, window, cx));
            self.description.update(cx, |input, cx| {
                input.set_value(draft.description, window, cx)
            });
            self.tags.update(cx, |input, cx| {
                input.set_value(draft.tags.join(" "), window, cx)
            });
            if content_changed {
                self.reset_compose_batch();
            }
            cx.notify();
        }
    }

    fn start_compose_batch(&mut self, cx: &App) {
        if self.queued {
            self.page = Page::Jobs;
            return;
        }
        if self.pending_batch.is_some() || self.selected_accounts.is_empty() {
            return;
        }
        let Some(content) = self.content(cx) else {
            return;
        };
        if let Err(error) = content.validate() {
            self.message = error.to_string();
            return;
        }
        let key = self.batch_key.clone();
        self.message.clear();
        self.backend.send(Command::Enqueue {
            key: key.clone(),
            accounts: self.selected_accounts.iter().cloned().collect(),
            content,
        });
        self.pending_batch = Some(key);
    }

    pub(super) fn finish_compose_batch(&mut self, key: String, result: Result<(), String>) {
        if self.pending_batch.as_ref() != Some(&key) {
            return;
        }
        self.pending_batch = None;
        match result {
            Ok(()) if self.batch_key == key => {
                self.queued = true;
                if self.page == Page::Compose {
                    self.page = Page::Jobs;
                }
            }
            Err(message) => self.message = message,
            _ => {}
        }
    }
}

fn section_title(label: &'static str) -> Div {
    div()
        .text_size(px(17.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
}

fn field_title(label: &'static str) -> Div {
    div().text_sm().font_weight(FontWeight::MEDIUM).child(label)
}

fn muted_text(text: &'static str) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(text)
}

fn compose_choice(id: ElementId, selected: bool) -> Button {
    Button::new(id)
        .w_full()
        .min_w_0()
        .outline()
        .toggled(selected)
        .bg(rgb(if selected { ACCENT_WASH } else { SURFACE }))
        .border_color(rgb(if selected { 0xc8c3fa } else { BORDER }))
        .text_color(rgb(if selected { ACCENT } else { INK }))
}

pub(super) fn asset_name(asset: &Asset) -> String {
    if asset.kind == MediaKind::Images {
        return format!("图文 · {} 张图片", asset.images.len());
    }
    asset
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub(super) fn asset_format(asset: &Asset) -> String {
    if asset.kind == MediaKind::Images {
        return "图文".into();
    }
    asset
        .path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| "视频".into())
}

pub(super) fn media_icon(kind: MediaKind) -> AppIcon {
    match kind {
        MediaKind::Video => AppIcon::Video,
        MediaKind::Images => AppIcon::Image,
    }
}

fn asset_label(asset: &Asset) -> Div {
    let format = asset_format(asset);
    let size = if asset.size < 1_000_000 {
        format!("{:.1} KB", asset.size as f64 / 1_000.)
    } else {
        format!("{:.1} MB", asset.size as f64 / 1_000_000.)
    };
    h_flex()
        .min_w_0()
        .gap_2()
        .child(icon_tile(media_icon(asset.kind), 30., ACCENT, ACCENT_WASH))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(asset_name(asset)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!("{format} · {size}")),
                ),
        )
}
