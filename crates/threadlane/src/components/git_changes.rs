use makepad_widgets::*;
use std::collections::HashSet;

use crate::git::GitFile;

#[derive(Clone, Copy, Debug)]
enum GitChangesRow {
    File { index: usize },
}

#[derive(Clone, Debug, Default)]
pub enum GitChangesAction {
    Open(String),
    SelectionChanged,
    #[default]
    None,
}

script_mod! {
    use mod.prelude.widgets.*

    mod.components.GitSvgIcon = View {
        width: 14
        height: 14
        align: Align{x: 0.5 y: 0.5}
        icon := Icon {
            width: Fill
            height: Fill
            icon_walk: Walk{width: 14 height: 14}
            draw_icon +: { color: theme.color_muted_foreground }
        }
    }

    mod.components.GitChangesBase = #(GitChanges::register_widget(vm))
    mod.components.GitChanges = set_type_default() do mod.components.GitChangesBase {
        width: Fill
        height: Fill
        flow: Down
        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true
            scroll_bar: mod.widgets.ScrollBar {}

            Empty := View {
                width: Fill
                height: 42
                align: Align{x: 0.5 y: 0.5}
                empty_lbl := Label {
                    text: "Working tree clean"
                    draw_text +: {
                        color: theme.color_muted_foreground
                        text_style: theme.font_regular { font_size: 9.0 }
                    }
                }
            }

            File := View {
                width: Fill
                height: 26
                flow: Right
                spacing: 4
                align: Align{y: 0.5}
                padding: Inset{left: 2 right: 2}

                file_code_icon := mod.components.GitSvgIcon {
                    icon +: {
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/file-code.svg")
                            color: theme.color_muted_foreground
                        }
                    }
                }
                file_image_icon := mod.components.GitSvgIcon {
                    visible: false
                    icon +: {
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/image.svg")
                            color: theme.color_muted_foreground
                        }
                    }
                }

                status_modified_icon := mod.components.GitSvgIcon {
                    icon +: {
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/status-modified.svg")
                            color: theme.color_warning
                        }
                    }
                }
                status_untracked_icon := mod.components.GitSvgIcon {
                    visible: false
                    icon +: {
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/status-untracked.svg")
                            color: theme.color_warning
                        }
                    }
                }
                status_added_icon := mod.components.GitSvgIcon {
                    visible: false
                    icon +: {
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/status-added.svg")
                            color: theme.color_success
                        }
                    }
                }
                status_deleted_icon := mod.components.GitSvgIcon {
                    visible: false
                    icon +: {
                        draw_icon +: {
                            svg: crate_resource("self:resources/icons/status-deleted.svg")
                            color: theme.color_destructive
                        }
                    }
                }

                path_btn := Button {
                    width: Fill
                    height: 24
                    padding: Inset{left: 2 right: 4}
                    spacing: 0
                    align: Align{x: 0.0 y: 0.5}
                    text: ""
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_accent
                        color_down: theme.color_secondary
                        border_color: theme.color_transparent
                        border_size: 0.0
                        border_radius: theme.radius_xs
                    }
                    draw_text +: {
                        color: theme.color_foreground
                        color_hover: theme.color_foreground
                        text_style: theme.font_code { font_size: 8.5 }
                    }
                }

                additions_lbl := Label {
                    align: Align{y: 0.5}
                    padding: Inset{left: 2 right: 1}
                    draw_text +: {
                        color: theme.color_success
                        text_style: theme.font_code { font_size: 8.5 }
                    }
                }

                deletions_lbl := Label {
                    align: Align{y: 0.5}
                    padding: Inset{left: 1 right: 4}
                    draw_text +: {
                        color: theme.color_destructive
                        text_style: theme.font_code { font_size: 8.5 }
                    }
                }

                select_checked_btn := mod.components.IconButton {
                    width: 20
                    height: 20
                    visible: false
                    icon_walk: Walk{width: 14 height: 14 margin: 0}
                    draw_icon +: {
                        svg: crate_resource("self:resources/icons/checkbox-checked.svg")
                        color: theme.color_primary
                        color_hover: theme.color_foreground
                        color_down: theme.color_primary_foreground
                    }
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_accent
                        color_down: theme.color_secondary
                    }
                }

                select_unchecked_btn := mod.components.IconButton {
                    width: 20
                    height: 20
                    icon_walk: Walk{width: 14 height: 14 margin: 0}
                    draw_icon +: {
                        svg: crate_resource("self:resources/icons/checkbox-unchecked.svg")
                        color: theme.color_muted_foreground
                        color_hover: theme.color_foreground
                        color_down: theme.color_primary_foreground
                    }
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_accent
                        color_down: theme.color_secondary
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct GitChanges {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    files: Vec<GitFile>,
    #[rust]
    rows: Vec<GitChangesRow>,
    #[rust]
    selected: HashSet<String>,
}

impl GitChanges {
    pub fn set_files(&mut self, cx: &mut Cx, files: Vec<GitFile>) {
        if self.files == files {
            return;
        }
        for file in &files {
            if !self.files.iter().any(|f| f.path == file.path) {
                self.selected.insert(file.path.clone());
            }
        }
        self.files = files;
        self.selected
            .retain(|path| self.files.iter().any(|file| &file.path == path));
        self.rebuild_rows();
        self.view.redraw(cx);
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for index in 0..self.files.len() {
            self.rows.push(GitChangesRow::File { index });
        }
    }

    pub fn selected_files(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| self.selected.contains(&file.path))
            .map(|file| file.path.clone())
            .collect()
    }

    pub fn all_files(&self) -> Vec<String> {
        self.files.iter().map(|file| file.path.clone()).collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn toggle_all(&mut self, cx: &mut Cx) {
        if self.selected.len() == self.files.len() {
            self.selected.clear();
        } else {
            self.selected = self.files.iter().map(|file| file.path.clone()).collect();
        }
        self.view.redraw(cx);
    }

    pub fn clear_selection(&mut self, cx: &mut Cx) {
        if !self.selected.is_empty() {
            self.selected.clear();
            self.view.redraw(cx);
        }
    }
}

impl Widget for GitChanges {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = step.as_portal_list().borrow_mut() {
                if self.files.is_empty() {
                    list.set_item_range(cx, 0, 0);
                    let item = list.item(cx, 0, id!(Empty));
                    item.draw_all_unscoped(cx);
                    continue;
                }
                list.set_item_range(cx, 0, self.rows.len());
                while let Some(row_index) = list.next_visible_item(cx) {
                    match self.rows.get(row_index).copied() {
                        Some(GitChangesRow::File { index: file_index }) => {
                            let Some(file) = self.files.get(file_index) else {
                                continue;
                            };
                            let row = list.item(cx, row_index, id!(File));
                            let is_selected = self.selected.contains(&file.path);
                            row.button(cx, ids!(select_checked_btn))
                                .set_visible(cx, is_selected);
                            row.button(cx, ids!(select_unchecked_btn))
                                .set_visible(cx, !is_selected);

                            let is_image = file.path.ends_with(".svg")
                                || file.path.ends_with(".png")
                                || file.path.ends_with(".jpg")
                                || file.path.ends_with(".jpeg")
                                || file.path.ends_with(".ico")
                                || file.path.ends_with(".webp");

                            row.view(cx, ids!(file_code_icon))
                                .set_visible(cx, !is_image);
                            row.view(cx, ids!(file_image_icon))
                                .set_visible(cx, is_image);

                            let status = file.status_char();
                            let is_added = status == 'A' || status == '?';
                            let is_deleted = status == 'D';
                            let is_modified = !is_added && !is_deleted;

                            row.view(cx, ids!(status_modified_icon))
                                .set_visible(cx, is_modified);
                            row.view(cx, ids!(status_untracked_icon))
                                .set_visible(cx, false);
                            row.view(cx, ids!(status_added_icon))
                                .set_visible(cx, is_added);
                            row.view(cx, ids!(status_deleted_icon))
                                .set_visible(cx, is_deleted);

                            let formatted_path = format_file_path(&file.path);
                            let path_btn = row.button(cx, ids!(path_btn));
                            if path_btn.text() != formatted_path {
                                path_btn.set_text(cx, &formatted_path);
                            }

                            let add_str = if file.additions > 0 {
                                format!("+{}", file.additions)
                            } else {
                                String::new()
                            };
                            let del_str = if file.deletions > 0 {
                                format!("-{}", file.deletions)
                            } else {
                                String::new()
                            };

                            let additions_lbl = row.label(cx, ids!(additions_lbl));
                            if additions_lbl.text() != add_str {
                                additions_lbl.set_text(cx, &add_str);
                            }

                            let deletions_lbl = row.label(cx, ids!(deletions_lbl));
                            if deletions_lbl.text() != del_str {
                                deletions_lbl.set_text(cx, &del_str);
                            }
                            row.draw_all_unscoped(cx);
                        }
                        None => {}
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let uid = self.widget_uid();
            let list = self.view.portal_list(cx, ids!(list));
            for (index, row) in list.items_with_actions(actions) {
                let Some(GitChangesRow::File { index: file_index }) = self.rows.get(index).copied()
                else {
                    continue;
                };
                let Some(file) = self.files.get(file_index) else {
                    continue;
                };
                if row.button(cx, ids!(select_checked_btn)).clicked(actions)
                    || row.button(cx, ids!(select_unchecked_btn)).clicked(actions)
                {
                    if !self.selected.insert(file.path.clone()) {
                        self.selected.remove(&file.path);
                    }
                    cx.widget_action(uid, GitChangesAction::SelectionChanged);
                    self.view.redraw(cx);
                } else if row.button(cx, ids!(path_btn)).clicked(actions) {
                    cx.widget_action(uid, GitChangesAction::Open(file.path.clone()));
                }
            }
        }
    }
}

fn format_file_path(path: &str) -> String {
    if let Some((dir, name)) = path.rsplit_once('/') {
        let truncated_dir = if dir.len() > 30 {
            format!("...{}", &dir[dir.len() - 27..])
        } else {
            dir.to_string()
        };
        format!("{name}   {truncated_dir}")
    } else {
        path.to_string()
    }
}
