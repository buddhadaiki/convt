//! Preferences for the Explorer commands, kept in the shared settings file.

use std::collections::HashSet;

use convt_core::{Category, FORMATS, WindowsMenu, format_by_id};
use gpui_kit::*;

use super::SettingsView;
use super::theme::{self, Palette, text, text_button};

const GROUPS: [(Category, &str); 8] = [
    (Category::Image, "Images"),
    (Category::Vector, "Vectors"),
    (Category::Video, "Video"),
    (Category::Audio, "Audio"),
    (Category::Pdf, "PDF"),
    (Category::Document, "Documents"),
    (Category::Spreadsheet, "Spreadsheets"),
    (Category::Presentation, "Presentations"),
];

impl SettingsView {
    fn change_windows_menu(
        &mut self,
        change: impl FnOnce(&mut WindowsMenu),
        cx: &mut Context<Self>,
    ) {
        self.menu_error = self
            .app
            .update(cx, |state, cx| {
                state.save_settings_now(|settings| change(&mut settings.windows_menu), cx)
            })
            .err()
            .map(|error| format!("The menu preferences couldn't be saved: {error}"));
        cx.notify();
    }

    pub(super) fn windows_menu(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let state = self.app.read(cx);
        let registry = state.registry.clone();
        let menu = state.settings.windows_menu.clone();
        let errors = state.errors.clone();
        let custom = menu.targets.is_some();
        let selected: HashSet<_> = menu
            .targets
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let available: HashSet<_> = FORMATS
            .iter()
            .flat_map(|from| registry.targets(from))
            .map(|format| format.id)
            .collect();

        let enabled = theme::switch("menu-enabled", menu.enabled, false, p).on_click(cx.listener(
            move |this, _, _, cx| {
                this.change_windows_menu(|menu| menu.enabled = !menu.enabled, cx);
            },
        ));
        let weak = cx.entity().downgrade();
        let mode = theme::segmented(
            "menu",
            &[
                ("recommended", "Recommended".into()),
                ("custom", "Custom".into()),
            ],
            if custom { "custom" } else { "recommended" },
            p,
            move |mode, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.change_windows_menu(
                        |menu| {
                            if mode == "custom" {
                                menu.targets.get_or_insert_with(|| {
                                    WindowsMenu::recommended_targets(&registry)
                                });
                            } else {
                                menu.targets = None;
                            }
                        },
                        cx,
                    );
                });
            },
        );

        let controls = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(text(13., 18., p.text).child("Show Convert with convt"))
                    .child(enabled),
            )
            .child(mode)
            .child(text(12., 18., p.secondary).child(if custom {
                "Pick quick output formats. Each menu shows related formats supported for every selected file. Mais opens all compatible conversions in the app."
            } else {
                "Offer recommended formats for each kind of file, followed by Mais to open the app. Choose Custom to pick your quick formats."
            }));

        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(22.))
            .p(px(24.))
            .child(controls)
            .children(
                errors
                    .into_iter()
                    .map(|error| text(12., 18., p.error).child(error)),
            );
        if custom {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(text(12., 18., p.secondary).child(format!(
                        "{} formats selected",
                        selected.iter().filter(|id| format_by_id(id).is_some()).count()
                    )))
                    .child(
                        text_button("menu-clear", "Clear selection", p.secondary, 12.).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.change_windows_menu(|menu| menu.targets = Some(vec![]), cx);
                            }),
                        ),
                    ),
            );
            for (category, title) in GROUPS {
                let choices = FORMATS.iter().filter(|format| format.category == category);
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            text(12., 16., p.secondary)
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(div().flex().flex_wrap().gap(px(12.)).children(choices.map(
                            |format| {
                                let id = format.id;
                                let checked = selected.contains(id);
                                let supported = available.contains(id);
                                theme::checkbox(
                                    SharedString::from(format!("menu-format-{id}")),
                                    SharedString::from(if supported {
                                        format.name.to_owned()
                                    } else {
                                        format!("{} (unavailable)", format.name)
                                    }),
                                    checked,
                                    p,
                                )
                                .w(px(174.))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.change_windows_menu(
                                            |menu| {
                                                let targets =
                                                    menu.targets.get_or_insert_with(Vec::new);
                                                targets.retain(|target| target != id);
                                                if !checked {
                                                    targets.push(id.to_owned());
                                                }
                                            },
                                            cx,
                                        );
                                    },
                                ))
                            },
                        ))),
                );
            }
            body = body.child(text(12., 18., p.secondary).child(
                "Selections for unavailable formats stay saved. An empty selection leaves only Mais. Turn off the menu above to hide it.",
            ));
        }

        let registry = &self.app.read(cx).registry;
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    text(12., 16., p.secondary)
                        .font_weight(FontWeight::MEDIUM)
                        .child("Menu preview"),
                )
                .children(["png", "mp4", "pdf"].map(|id| {
                    let from = format_by_id(id).unwrap();
                    let targets = menu.menu_targets(registry, from);
                    let detail = if !menu.enabled {
                        "Menu disabled".to_owned()
                    } else {
                        targets
                            .iter()
                            .map(|format| format.name)
                            .chain(std::iter::once("Mais"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let line = format!("{} → {detail}", from.name);
                    text(12., 18., p.text)
                        .id(SharedString::from(format!("menu-preview-{id}")))
                        .test_support()
                        .aria_label(line.clone())
                        .child(line)
                })),
        );
        body = body.child(text(12., 18., p.secondary).child(
            "The Windows installer adds this menu. Changes apply when you open the menu again. Mais opens Quick convert with the selected files and all compatible conversions.",
        ));
        if let Some(error) = &self.menu_error {
            body = body.child(
                text(12., 18., p.error)
                    .id("menu-error")
                    .test_support()
                    .aria_label(error.clone())
                    .child(error.clone()),
            );
        }
        body
    }
}
