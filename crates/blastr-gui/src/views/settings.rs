//! The settings dialog: gpui-kit's settings pages over the workspace's
//! settings. A change applies at once; one the analysis depends on offers to
//! run the comparison again instead of doing it silently.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsView,
};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Icon, IconName, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Entity, ParentElement, SharedString, Styled, Window, div, px};

use crate::settings::{DEPTHS, Settings, ignore_problem, parse_list};
use crate::theme::{self, TEXT_SMALL, TEXT_UI};
use crate::workspace::{Workspace, WorkspaceEvent};

/// Opens the settings dialog.
pub(crate) fn open(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) {
    // What was typed into the list fields, kept as typed while it is parsed
    // into the settings, so that a trailing comma is not taken away.
    let drafts: Rc<RefCell<HashMap<&'static str, SharedString>>> = Rc::default();
    window.open_dialog(cx, move |dialog, _, cx| {
        let rerun = workspace.read(cx).rerun_needed;
        let root = workspace.read(cx).root().map(PathBuf::from);
        let mut pages = vec![
            editor_page(&workspace),
            analysis_page(&workspace),
            agents_page(&workspace),
        ];
        if let Some(root) = root {
            pages.push(repository_page(&workspace, root, drafts.clone()));
        }
        let again = workspace.clone();
        // The categories are drawn as the window's side panels are, the page
        // beside them on the same surface, set in the window's sizes.
        let mut sidebar = gpui_kit::StyleRefinement::default()
            .text_size(TEXT_UI)
            .border_r_1()
            .border_color(theme::LINE);
        sidebar.background = Some(theme::BG_PANEL.hsla().into());
        let header = gpui_kit::StyleRefinement::default().text_size(TEXT_UI);
        dialog
            .w(px(820.))
            .bg(theme::BG_PANEL)
            .border_color(theme::LINE_STRONG)
            .title(div().text_size(px(15.)).child("Settings"))
            .child(
                div()
                    .h(px(460.))
                    .text_size(TEXT_UI)
                    .border_1()
                    .border_color(theme::LINE)
                    .rounded(theme::RADIUS_LG)
                    .overflow_hidden()
                    .child(
                        SettingsView::new("settings")
                            .sidebar_width(px(190.))
                            .sidebar_style(&sidebar)
                            .header_style(&header)
                            .pages(pages),
                    ),
            )
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .when(rerun, |this| {
                        this.child(Icon::new(IconName::Info).text_color(theme::EDIT.fg))
                            .child(
                                div()
                                    .text_size(TEXT_SMALL)
                                    .text_color(theme::EDIT.fg)
                                    .child("The comparison ran with other settings."),
                            )
                            .child(
                                Button::new("settings-rerun")
                                    .small()
                                    .primary()
                                    .label("Run it again")
                                    .on_click(move |_, _, cx| {
                                        again.update(cx, |workspace, cx| workspace.retry(cx))
                                    }),
                            )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_4)
                            .child("Saved to your configuration directory."),
                    ),
            )
    });
}

/// A field that reads and writes one part of the settings.
fn getter<T: 'static>(
    workspace: &Entity<Workspace>,
    read: impl Fn(&Settings) -> T + 'static,
) -> impl Fn(&App) -> T + 'static {
    let workspace = workspace.clone();
    move |cx| read(&workspace.read(cx).settings)
}

fn setter<T: 'static>(
    workspace: &Entity<Workspace>,
    write: impl Fn(&mut Settings, T) + 'static,
) -> impl Fn(T, &mut App) + 'static {
    let workspace = workspace.clone();
    let write = Rc::new(write);
    move |value, cx| {
        let write = write.clone();
        workspace.update(cx, |workspace, cx| {
            workspace.update_settings(cx, |settings| write(settings, value))
        });
    }
}

fn editor_page(workspace: &Entity<Workspace>) -> SettingPage {
    let problem = getter(workspace, |settings| {
        crate::editor::problem(&settings.editor)
    });
    SettingPage::new("Editor").group(
        SettingGroup::new()
            .title("Open in editor")
            .description(
                "The command that opens a file. {file}, {line} and {column} are filled in; quote a path \
                 with spaces. Empty opens the file with the program the system associates with it.",
            )
            .item(SettingItem::new(
                "Command",
                SettingField::input(
                    getter(workspace, |settings| SharedString::from(settings.editor.clone())),
                    setter(workspace, |settings, value: SharedString| settings.editor = value.to_string()),
                ),
            ).layout(gpui_kit::Axis::Vertical))
            .item(SettingItem::render(move |_, _, cx| {
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::EDIT.fg)
                    .children(problem(cx))
            })),
    )
}

fn analysis_page(workspace: &Entity<Workspace>) -> SettingPage {
    SettingPage::new("Analysis").group(
        SettingGroup::new()
            .title("Analysis")
            .description("A change here asks to run the comparison again.")
            .item(
                SettingItem::new(
                    "Depth",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: f64::from(*DEPTHS.start()),
                            max: f64::from(*DEPTHS.end()),
                            step: 1.0,
                        },
                        getter(workspace, |settings| f64::from(shown_depth(settings))),
                        setter(workspace, |settings, value: f64| {
                            settings.analysis.depth = (value.max(0.0).round() as u32)
                                .clamp(*DEPTHS.start(), *DEPTHS.end());
                        }),
                    ),
                )
                .description(
                    "How many steps the impact of a change is followed. 0 = unlimited: every \
                     item it reaches is listed.",
                ),
            )
            .item(
                SettingItem::new(
                    "Refresh on save",
                    toggle(
                        "refresh-on-save",
                        getter(workspace, shown_refresh_on_save),
                        setter(workspace, |settings, value: bool| {
                            settings.analysis.refresh_on_save = value
                        }),
                    ),
                )
                .description("Run a comparison of the working tree again when a file is saved."),
            ),
    )
}

fn agents_page(workspace: &Entity<Workspace>) -> SettingPage {
    let copy = workspace.clone();
    SettingPage::new("AI agents").group(
        SettingGroup::new()
            .title("AI agents")
            .description(
                "Agents such as Claude Code use Blastr through its MCP server, blastr-mcp, while \
                 this window is open: what they open, compare and select is shown here.",
            )
            .item(
                SettingItem::new(
                    "Allow AI agents (MCP)",
                    toggle(
                        "agents",
                        getter(workspace, |settings| settings.agents),
                        setter(workspace, |settings, value: bool| settings.agents = value),
                    ),
                )
                .description("When off, every request of an agent is refused."),
            )
            .item(
                SettingItem::new(
                    "MCP config",
                    SettingField::render(move |_, _, _| {
                        let copy = copy.clone();
                        Button::new("copy-mcp-config")
                            .small()
                            .outline()
                            .label("Copy MCP config")
                            .on_click(move |_, _, cx| {
                                let config = mcp_config(&mcp_program());
                                copy.update(cx, |_, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(config));
                                    cx.emit(WorkspaceEvent::Notice(
                                        "Copied the MCP config".to_owned(),
                                    ));
                                });
                            })
                    }),
                )
                .description("The entry that adds Blastr to an agent's MCP servers."),
            ),
    )
}

/// The MCP server, which is installed next to the window.
fn mcp_program() -> PathBuf {
    let name = format!("blastr-mcp{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .map(|exe| exe.with_file_name(&name))
        .unwrap_or_else(|_| PathBuf::from(name))
}

/// The MCP config that starts `program` as the server `blastr`.
fn mcp_config(program: &Path) -> String {
    let command = program.to_string_lossy();
    serde_json::json!({ "mcpServers": { "blastr": { "command": command } } }).to_string()
}

/// A switch for one setting, drawn in the brand colour when it is on.
///
/// The library's own switch field draws *on* as a track in the primary
/// button's near-white with a near-black knob, and *off* as a dark grey track
/// with the same knob: in this theme the two read alike, and an enabled
/// setting looked disabled. This one fills the track when it is on and keeps
/// a light knob in both states.
fn toggle(
    id: &'static str,
    value: impl Fn(&App) -> bool + 'static,
    set: impl Fn(bool, &mut App) + 'static,
) -> SettingField<SharedString> {
    let set = Rc::new(set);
    SettingField::render(move |_, _, cx| {
        let set = set.clone();
        Switch::new(id)
            .checked(value(cx))
            .color(theme::CHANGED.solid)
            .on_click(move |checked: &bool, _, cx| set(*checked, cx))
    })
}

/// The depth the dialog shows: 0 for unlimited.
fn shown_depth(settings: &Settings) -> u32 {
    settings.analysis.depth
}

/// Whether the dialog shows refresh on save as on.
fn shown_refresh_on_save(settings: &Settings) -> bool {
    settings.analysis.refresh_on_save
}

fn repository_page(
    workspace: &Entity<Workspace>,
    root: PathBuf,
    drafts: Rc<RefCell<HashMap<&'static str, SharedString>>>,
) -> SettingPage {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let list = |key: &'static str,
                separators: &'static [char],
                joiner: &'static str,
                read: fn(&crate::settings::RepositorySettings) -> Vec<String>,
                write: fn(&mut crate::settings::RepositorySettings, Vec<String>)| {
        let (root_get, root_set) = (root.clone(), root.clone());
        let (drafts_get, drafts_set) = (drafts.clone(), drafts.clone());
        SettingField::input(
            getter(workspace, move |settings| {
                drafts_get.borrow().get(key).cloned().unwrap_or_else(|| {
                    settings
                        .repository(&root_get)
                        .map(|repository| read(repository).join(joiner))
                        .unwrap_or_default()
                        .into()
                })
            }),
            setter(workspace, move |settings, value: SharedString| {
                write(
                    settings.repository_mut(&root_set),
                    parse_list(&value, separators),
                );
                drafts_set.borrow_mut().insert(key, value);
            }),
        )
    };
    let flag = |id: &'static str,
                read: fn(&crate::settings::RepositorySettings) -> bool,
                write: fn(&mut crate::settings::RepositorySettings, bool)| {
        let (root_get, root_set) = (root.clone(), root.clone());
        toggle(
            id,
            getter(workspace, move |settings| {
                read(&settings.repository_or_default(&root_get))
            }),
            setter(workspace, move |settings, value: bool| {
                write(settings.repository_mut(&root_set), value)
            }),
        )
    };
    let ignore_root = root.clone();
    let problem = getter(workspace, move |settings| {
        settings
            .repository(&ignore_root)
            .and_then(|repository| ignore_problem(&repository.ignore))
    });
    SettingPage::new("This repository").group(
        SettingGroup::new()
            .title(name)
            .description("Kept for this repository only. A change here asks to run the comparison again.")
            .item(
                SettingItem::new(
                    "Features",
                    list("features", &[',', ' ', '\t'], ", ", |repository| repository.features.clone(), |repository, value| {
                        repository.features = value
                    }),
                )
                .description("Separated by commas or spaces. Empty means the default features."),
            )
            .item(SettingItem::new(
                "All features",
                flag("all-features", |repository| repository.all_features, |repository, value| repository.all_features = value),
            ))
            .item(
                SettingItem::new(
                    "Target triple",
                    list("target", &[',', ' '], "", |repository| repository.target.iter().cloned().collect(), |repository, value| {
                        repository.target = value.into_iter().next()
                    }),
                )
                .description("Empty means the host."),
            )
            .item(
                SettingItem::new(
                    "Ignore",
                    list("ignore", &[',', '\n'], ", ", |repository| repository.ignore.clone(), |repository, value| {
                        repository.ignore = value
                    }),
                )
                .description("Files whose items are not listed, such as vendor/**. Separated by commas."),
            )
            .item(SettingItem::render(move |_, _, cx| {
                div().text_size(TEXT_SMALL).text_color(theme::EDIT.fg).children(problem(cx))
            }))
            .item(
                SettingItem::new(
                    "Restricted mode",
                    flag(
                        "restricted",
                        |repository| repository.restricted == Some(true),
                        |repository, value| repository.restricted = Some(value),
                    ),
                )
                .description("Skip build scripts and procedural macros. Code they generate is then invisible."),
            ),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_fresh_settings_file_shows_refresh_on_save_on_and_unlimited_depth() {
        let directory = tempfile::tempdir().expect("a directory");
        let settings = Settings::load(&directory.path().join("settings.json"));
        assert!(shown_refresh_on_save(&settings));
        assert_eq!(shown_depth(&settings), 0);
        // A repository with no entry yet shows every feature on.
        let repository = settings.repository_or_default(Path::new("/code/a"));
        assert!(repository.all_features);
        assert_eq!(repository.restricted, None);
    }

    #[test]
    fn the_mcp_config_names_the_server_beside_the_window() {
        let config = mcp_config(Path::new(r"C:\Program Files\Blastr\blastr-mcp.exe"));
        assert_eq!(
            config,
            r#"{"mcpServers":{"blastr":{"command":"C:\\Program Files\\Blastr\\blastr-mcp.exe"}}}"#
        );
        let program = mcp_program();
        let name = program.file_name().expect("a name").to_string_lossy();
        assert_eq!(name, format!("blastr-mcp{}", std::env::consts::EXE_SUFFIX));
    }
}
