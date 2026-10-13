use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Category, FORMATS, Format, Registry};

/// Per-user Explorer menu choices. Conversion routes remain unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowsMenu {
    pub enabled: bool,
    /// None uses Windows recommendations; an explicit list has no fallback.
    pub targets: Option<Vec<String>>,
}

impl Default for WindowsMenu {
    fn default() -> Self {
        Self {
            enabled: true,
            targets: None,
        }
    }
}

impl WindowsMenu {
    /// Reachable targets in the input's menu family; configured order is preserved.
    pub fn menu_targets(&self, registry: &Registry, from: &'static Format) -> Vec<&'static Format> {
        if !self.enabled {
            return Vec::new();
        }
        let Some(configured) = &self.targets else {
            return Self::default_targets(registry, from);
        };
        let reachable = registry.targets(from);
        let mut seen = HashSet::new();
        configured
            .iter()
            .filter_map(|id| reachable.iter().find(|format| format.id == id).copied())
            .filter(|format| same_menu_family(from, format))
            .filter(|format| seen.insert(format.id))
            .collect()
    }

    fn default_targets(registry: &Registry, from: &'static Format) -> Vec<&'static Format> {
        if from.category == Category::Video {
            let reachable = registry.targets(from);
            let mut picked: Vec<_> = ["mp4", "mov", "webm", "mkv"]
                .iter()
                .filter_map(|id| reachable.iter().find(|format| format.id == *id).copied())
                .take(3)
                .collect();
            if picked.is_empty() {
                picked = reachable
                    .iter()
                    .copied()
                    .filter(|format| format.category == Category::Video)
                    .take(3)
                    .collect();
            }
            if let Some(gif) = reachable.iter().find(|format| format.id == "gif") {
                picked.push(*gif);
            }
            return picked;
        }
        let picked: Vec<_> = registry
            .menu_targets(from)
            .into_iter()
            .filter(|format| same_menu_family(from, format))
            .collect();
        if picked.is_empty() {
            registry
                .targets(from)
                .into_iter()
                .filter(|format| same_menu_family(from, format))
                .take(4)
                .collect()
        } else {
            picked
        }
    }

    /// The default menus' union, in format-table and menu order, for settings UI.
    pub fn recommended_targets(registry: &Registry) -> Vec<String> {
        let mut seen = HashSet::new();
        let menu = Self::default();
        FORMATS
            .iter()
            .flat_map(|from| menu.menu_targets(registry, from))
            .filter(|format| seen.insert(format.id))
            .map(|format| format.id.to_owned())
            .collect()
    }

    /// Read only the menu section so other clients' setting schemas can evolve.
    /// The whole document must still be syntactically valid TOML.
    pub fn from_settings_toml(contents: &str) -> std::result::Result<Self, toml::de::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct MenuSettings {
            windows_menu: WindowsMenu,
        }

        toml::from_str::<MenuSettings>(contents).map(|settings| settings.windows_menu)
    }
}

fn same_menu_family(from: &Format, to: &Format) -> bool {
    match from.category {
        Category::Image | Category::Vector => {
            matches!(to.category, Category::Image | Category::Vector)
        }
        Category::Audio => to.category == Category::Audio,
        Category::Video => to.category == Category::Video || to.id == "gif",
        Category::Pdf | Category::Document | Category::Spreadsheet | Category::Presentation => {
            matches!(
                to.category,
                Category::Pdf | Category::Document | Category::Spreadsheet | Category::Presentation
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use crate::{Ctx, Engine, Result, Step, format_by_id};

    struct Routes(Vec<(&'static str, &'static str)>);

    impl Engine for Routes {
        fn id(&self) -> &'static str {
            "menu-contract"
        }

        fn steps(&self) -> Vec<Step> {
            self.0
                .iter()
                .map(|(from, to)| Step {
                    from: format_by_id(from).unwrap(),
                    to: format_by_id(to).unwrap(),
                })
                .collect()
        }

        fn convert(&self, _: &Ctx, _: &Path, _: &Path) -> Result<Vec<PathBuf>> {
            unreachable!("routing fixture is never executed")
        }
    }

    fn registry() -> Registry {
        registry_with_steps(&[
            ("png", "jpeg"),
            ("jpeg", "webp"),
            ("webp", "bmp"),
            ("gif", "mp4"),
            ("mp4", "mp3"),
        ])
    }

    fn registry_with_steps(steps: &[(&'static str, &'static str)]) -> Registry {
        let mut registry = Registry::new();
        registry.register(Arc::new(Routes(steps.to_vec())));
        registry
    }

    fn ids(formats: Vec<&'static Format>) -> Vec<&'static str> {
        formats.into_iter().map(|format| format.id).collect()
    }

    #[test]
    fn defaults_and_disabled_policy_preserve_registry_routes() {
        let registry = registry();
        let from = format_by_id("png").unwrap();
        let all = registry.targets(from);
        assert_eq!(
            WindowsMenu::default().menu_targets(&registry, from),
            registry.menu_targets(from)
        );
        let disabled = WindowsMenu {
            enabled: false,
            targets: Some(vec!["jpeg".into()]),
        };
        assert!(disabled.menu_targets(&registry, from).is_empty());
        assert_eq!(registry.targets(from), all);
        assert!(registry.plan(from, format_by_id("bmp").unwrap()).is_ok());
    }

    #[test]
    fn explicit_menu_uses_all_reachable_targets_without_fallback() {
        let registry = registry();
        let from = format_by_id("png").unwrap();
        assert!(!ids(registry.menu_targets(from)).contains(&"bmp"));
        let custom = WindowsMenu {
            enabled: true,
            targets: Some(
                ["bmp", "unknown", "webp", "bmp", "mp4", "jpeg", "png"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            ),
        };
        assert_eq!(
            ids(custom.menu_targets(&registry, from)),
            ["bmp", "webp", "jpeg"]
        );
        for targets in [vec![], vec!["unknown".into(), "mp4".into(), "png".into()]] {
            let custom = WindowsMenu {
                enabled: true,
                targets: Some(targets),
            };
            assert!(custom.menu_targets(&registry, from).is_empty());
        }
    }

    #[test]
    fn recommended_targets_are_stable_unique_default_menu_union() {
        let registry = registry();
        assert_eq!(
            WindowsMenu::recommended_targets(&registry),
            ["webp", "jpeg", "bmp"]
        );
        assert!(WindowsMenu::recommended_targets(&Registry::new()).is_empty());
    }

    #[test]
    fn default_video_menu_offers_three_preferred_videos_and_reachable_gif() {
        let registry = registry_with_steps(&[
            ("mp4", "mov"),
            ("mp4", "webm"),
            ("mp4", "mkv"),
            ("mp4", "avi"),
            ("mp4", "gif"),
            ("mp4", "mp3"),
            ("mp4", "png"),
            ("mov", "mp4"),
        ]);
        let menu = WindowsMenu::default();
        let from = format_by_id("mp4").unwrap();
        let full = registry.targets(from);
        assert_eq!(
            ids(menu.menu_targets(&registry, from)),
            ["mov", "webm", "mkv", "gif"]
        );
        assert_eq!(
            ids(menu.menu_targets(&registry, format_by_id("mov").unwrap())),
            ["mp4", "webm", "mkv", "gif"]
        );
        assert!(ids(registry.menu_targets(from)).contains(&"mp3"));
        assert!(ids(registry.targets(from)).contains(&"mp3"));
        assert!(registry.plan(from, format_by_id("mp3").unwrap()).is_ok());
        assert_eq!(registry.targets(from), full);
        let recommended = WindowsMenu::recommended_targets(&registry);
        assert!(recommended.iter().any(|id| id == "gif"));
        assert!(!recommended.iter().any(|id| id == "mp3"));
    }

    #[test]
    fn custom_menu_filters_reachable_cross_family_targets_but_keeps_saved_order() {
        let registry = registry_with_steps(&[
            ("mp4", "gif"),
            ("mp4", "mp3"),
            ("mp4", "wav"),
            ("mp4", "mov"),
            ("mp3", "mp4"),
            ("mp3", "wav"),
            ("png", "pdf"),
            ("png", "svg"),
            ("png", "jpeg"),
            ("pdf", "png"),
            ("pdf", "docx"),
            ("pdf", "csv"),
            ("csv", "pptx"),
            ("pptx", "xlsx"),
        ]);
        let menu = WindowsMenu {
            enabled: true,
            targets: Some(
                [
                    "mp4", "mp3", "gif", "mov", "gif", "wav", "pdf", "svg", "jpeg", "docx", "csv",
                    "pptx", "xlsx", "unknown",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            ),
        };
        assert_eq!(
            ids(menu.menu_targets(&registry, format_by_id("mp4").unwrap())),
            ["gif", "mov"]
        );
        assert_eq!(
            ids(menu.menu_targets(&registry, format_by_id("mp3").unwrap())),
            ["wav"]
        );
        assert_eq!(
            ids(menu.menu_targets(&registry, format_by_id("png").unwrap())),
            ["svg", "jpeg"]
        );
        assert_eq!(
            ids(menu.menu_targets(&registry, format_by_id("pdf").unwrap())),
            ["docx", "csv", "pptx", "xlsx"]
        );
    }

    #[test]
    fn defaults_fall_back_within_family_after_filtering_registry_recommendations() {
        let menu = WindowsMenu::default();
        for (from, steps, expected) in [
            ("svg", vec![("svg", "pdf"), ("svg", "bmp")], vec!["bmp"]),
            ("pdf", vec![("pdf", "png"), ("pdf", "csv")], vec!["csv"]),
            ("gif", vec![("gif", "mp4"), ("gif", "bmp")], vec!["bmp"]),
            (
                "mp4",
                vec![("mp4", "avi"), ("mp4", "gif"), ("mp4", "mp3")],
                vec!["avi", "gif"],
            ),
        ] {
            let registry = registry_with_steps(&steps);
            assert_eq!(
                ids(menu.menu_targets(&registry, format_by_id(from).unwrap())),
                expected,
                "{from}"
            );
        }
        assert!(
            menu.menu_targets(&Registry::new(), format_by_id("mp4").unwrap())
                .is_empty()
        );
    }

    #[test]
    fn serde_defaults_round_trip_preserving_explicit_empty_targets() {
        assert_eq!(
            toml::from_str::<WindowsMenu>("").unwrap(),
            WindowsMenu::default()
        );
        let disabled: WindowsMenu = toml::from_str("enabled = false\n").unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.targets, None);
        let empty: WindowsMenu = toml::from_str("targets = []\n").unwrap();
        assert!(empty.enabled);
        assert_eq!(empty.targets, Some(vec![]));
        assert_eq!(
            toml::from_str::<WindowsMenu>(&toml::to_string(&empty).unwrap()).unwrap(),
            empty
        );
    }

    #[test]
    fn settings_parser_interprets_only_the_menu_section_and_reports_bad_toml() {
        assert_eq!(
            WindowsMenu::from_settings_toml("concurrency = 'future schema'\n").unwrap(),
            WindowsMenu::default()
        );
        assert_eq!(
            WindowsMenu::from_settings_toml(
                "notifications = 'future schema'\n[windows_menu]\ntargets = ['webp']\n"
            )
            .unwrap(),
            WindowsMenu {
                enabled: true,
                targets: Some(vec!["webp".into()]),
            }
        );
        assert!(WindowsMenu::from_settings_toml("[windows_menu").is_err());
        assert!(WindowsMenu::from_settings_toml("[windows_menu]\nenabled = 'no'\n").is_err());
    }
}
