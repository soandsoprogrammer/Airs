use super::*;
use crate::compose::{asset_format, asset_name};
use publisher_core::Asset;

impl Publisher {
    pub(super) fn material_library(&self, cx: &mut Context<Self>) -> Div {
        let sets = &self.snapshot.material_sets;
        if let Some(set) = self
            .material_details
            .as_ref()
            .and_then(|id| sets.iter().find(|set| &set.id == id))
        {
            return self.material_detail(set, cx);
        }
        let toolbar = h_flex()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(format!("{} 个素材集", sets.len())),
            )
            .child(
                Button::new("import-materials")
                    .primary()
                    .icon(AppIcon::Plus)
                    .label("导入素材")
                    .on_click(cx.listener(|s, _, _, cx| s.pick_materials(cx))),
            );
        let view = v_flex().gap_3().child(toolbar);
        if sets.is_empty() {
            return view.child(
                surface()
                    .flex_row()
                    .items_center()
                    .p_5()
                    .child(icon_tile(AppIcon::FolderOpen, 40., ACCENT, ACCENT_WASH))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("还没有素材集"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child("一次选择 TXT、Markdown、视频和图片，即可导入为一组。"),
                            ),
                    ),
            );
        }
        view.children(sets.iter().map(|set| self.material_card(set, cx)))
    }

    fn material_card(&self, set: &MaterialSet, cx: &mut Context<Self>) -> Div {
        let id = set.id.clone();
        surface()
            .p_4()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_3()
                            .child(icon_tile(AppIcon::FolderOpen, 38., ACCENT, ACCENT_WASH))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(set.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(material_summary(set)),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(eid("inspect-material-set", &id))
                                    .small()
                                    .label("查看素材")
                                    .accessibility_label(format!("查看素材集 {}", set.title))
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.material_details = Some(id.clone());
                                        cx.notify();
                                    })),
                            )
                            .child(self.use_material_button(set, None, "用于新建发布", cx)),
                    ),
            )
            .when(!set.description.is_empty(), |card| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(set.description.clone()),
                )
            })
    }

    fn material_detail(&self, set: &MaterialSet, cx: &mut Context<Self>) -> Div {
        let mut view = v_flex()
            .gap_3()
            .child(
                Button::new("back-to-material-library")
                    .small()
                    .ghost()
                    .self_start()
                    .w_auto()
                    .icon(AppIcon::ArrowLeft)
                    .label("返回素材库")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.material_details = None;
                        cx.notify();
                    })),
            )
            .child(
                surface()
                    .p_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .flex_wrap()
                            .gap_3()
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(set.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(MUTED))
                                            .child(material_summary(set)),
                                    ),
                            )
                            .child(self.use_material_button(set, None, "用于新建发布", cx)),
                    )
                    .when(!set.description.is_empty(), |card| {
                        card.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("简介"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .whitespace_normal()
                                        .child(set.description.clone()),
                                ),
                        )
                    })
                    .when(!set.text_files.is_empty(), |card| {
                        card.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                    "文本来源 · {}",
                                    set.text_files
                                        .iter()
                                        .filter_map(|path| path.file_name())
                                        .map(|name| name.to_string_lossy())
                                        .collect::<Vec<_>>()
                                        .join("、")
                                )))
                    }),
            );
        if set.assets().next().is_some() {
            view = view.child(
                surface()
                    .p_0()
                    .gap_0()
                    .children(set.assets().enumerate().map(|(index, asset)| {
                        let (role, action) = match asset.kind {
                            MediaKind::Video if index == 0 => ("视频主体", "使用视频"),
                            MediaKind::Video => ("备选视频", "使用视频"),
                            MediaKind::Images => ("图文图片", "使用图文"),
                        };
                        h_flex()
                            .gap_3()
                            .px_4()
                            .py_3()
                            .when(index > 0, |row| row.border_t_1().border_color(rgb(BORDER)))
                            .child(icon_tile(media_icon(asset.kind), 38., ACCENT, ACCENT_WASH))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(asset_name(asset)),
                                    )
                                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                                        "{role} · {} · {}",
                                        asset_format(asset),
                                        material_file_size(asset.size)
                                    ))),
                            )
                            .child(self.preview_button(asset, cx))
                            .child(self.use_material_button(set, Some(asset), action, cx))
                    })),
            );
        }
        view
    }

    fn use_material_button(
        &self,
        set: &MaterialSet,
        asset: Option<&Asset>,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Button {
        let id = set.id.clone();
        let asset_id = asset.map(|asset| asset.id.clone());
        Button::new(match &asset_id {
            Some(asset_id) => eid("use-material-asset", asset_id),
            None => eid("use-material-set", &id),
        })
        .small()
        .when(asset.is_none(), |button| button.primary())
        .label(label)
        .accessibility_label(format!(
            "{label} {}",
            asset.map_or_else(|| set.title.clone(), asset_name)
        ))
        .on_click(cx.listener(move |s, _, window, cx| {
            s.navigate(Page::Compose);
            s.use_material_set(&id, asset_id.as_deref(), window, cx);
        }))
    }
}

pub(super) fn material_summary(set: &MaterialSet) -> String {
    let counts = [
        (set.videos.len(), "段视频"),
        (
            set.images.as_ref().map_or(0, |asset| asset.images.len()),
            "张图片",
        ),
        (set.text_files.len(), "个文本"),
    ];
    counts
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn material_file_size(bytes: u64) -> String {
    if bytes < 100_000 {
        format!("{} KB", bytes.div_ceil(1_000))
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.)
    }
}
