//! Local sample tree. Opens the existing preview editor; no filesystem service runs here.
use gpui::{prelude::*, *};
use gpui_component::notification::Notification;
use gpui_component::tree::{TreeItem, TreeState};
use gpui_component::{ActiveTheme, WindowExt};
use std::rc::Rc;
use threadlane_ui_kit as kit;

pub struct FilesPreview {
    tree: Entity<TreeState>,
    open: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl FilesPreview {
    pub fn new(on_open: impl Fn(&mut Window, &mut App) + 'static, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| {
            let mut tree = TreeState::new(cx);
            tree.set_items(
                vec![TreeItem::new(".", "Sample files")
                    .expanded(true)
                    .children(vec![TreeItem::new(
                        crate::editor::FILE,
                        crate::editor::FILE,
                    )])],
                cx,
            );
            tree
        });
        Self {
            tree,
            open: Rc::new(on_open),
        }
    }
}

impl Render for FilesPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open.clone();
        let menu_open = self.open.clone();
        kit::project_files_surface()
            .child(kit::project_files_find_button().on_click(|_, window, cx| {
                window.push_notification(Notification::info("Project search runs in the desktop host. This tree contains a local sample buffer."), cx);
            }))
            .child(kit::project_file_tree(&self.tree,
                move |_, window, cx| open(window, cx),
                move |path, folder, menu, _, _| {
                    let open = menu_open.clone();
                    kit::project_file_menu(menu, path, folder, None, move |action, window, cx| match action {
                        kit::ProjectFileAction::Open(_) => open(window, cx),
                        kit::ProjectFileAction::OpenInPanel(_) => open(window, cx),
                        kit::ProjectFileAction::CopyRelative(path) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                            window.push_notification(Notification::info("Copied relative path"), cx);
                        }
                        kit::ProjectFileAction::CopyAbsolute(_) => {}
                    })
                }))
            .child(div().flex_none().px_3().py_2().text_xs().text_color(cx.theme().muted_foreground)
                .child("Sample files · Opens the local editor · No project files changed"))
    }
}
