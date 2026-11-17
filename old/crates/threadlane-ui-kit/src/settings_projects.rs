//! Attached-project presentation; the host owns persistence and removal guards.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::{ActiveTheme, Disableable, Sizable};
use std::{path::PathBuf, rc::Rc};

pub struct SettingsProject {
    pub path: PathBuf,
    pub name: String,
    pub active: bool,
    pub disabled_reason: Option<String>,
}

pub fn settings_projects(
    projects: Vec<SettingsProject>,
    error: Option<String>,
    on_remove: impl Fn(PathBuf, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let on_remove = Rc::new(on_remove);
    let muted = cx.theme().muted_foreground;
    super::settings_group(cx)
        .flex_col()
        .gap_3()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Attached projects"))
        .child(div().text_xs().text_color(muted).child("Removing a project keeps its files and saved conversations. Attach the folder again to restore access. Automations are managed separately."))
        .when(projects.is_empty(), |group| group.child(div().text_sm().child("No attached projects. Use Attach project in the sidebar to add a folder.")))
        .children(projects.into_iter().map(|project| {
            let on_remove = on_remove.clone();
            let path = project.path.clone();
            let id: SharedString = format!("remove-project-{:?}", project.path.as_os_str().as_encoded_bytes()).into();
            div().flex().items_center().gap_3()
                .child(div().flex_1().min_w_0()
                    .child(div().text_sm().child(format!("{}{}", project.name, if project.active { " · Active" } else { "" })))
                    .child(div().text_xs().text_color(muted).child(crate::display_path(&project.path)))
                    .when_some(project.disabled_reason.clone(), |row, reason| row.child(div().text_xs().text_color(muted).child(reason))))
                .child(Button::new(id.clone()).debug_selector(move || id.to_string()).label("Remove…").outline().small()
                    .accessibility_label(format!("Remove project {}", project.name))
                    .disabled(project.disabled_reason.is_some())
                    .on_click(move |_, window, cx| on_remove(path.clone(), window, cx)))
        }))
        .when_some(error, |group, error| group.child(div().text_sm().text_color(cx.theme().danger).child(error)))
        .into_any_element()
}

pub fn project_removal_dialog(alert: AlertDialog, name: &str, path: &str) -> AlertDialog {
    alert.title(format!("Remove “{name}”?"))
        .description(format!("{path}\n\nRemove this project from Threadlane? Files and saved conversations will not be deleted. You can attach the folder again. Automations will not be removed or paused."))
        .button_props(DialogButtonProps::default().ok_text("Remove project").show_cancel(true))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{settings_projects, SettingsProject};
    use gpui::{Context, IntoElement, Render, TestAppContext, Window};
    use std::{cell::RefCell, ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf, rc::Rc};

    struct Host {
        paths: Vec<PathBuf>,
        removed: Rc<RefCell<Vec<PathBuf>>>,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let removed = self.removed.clone();
            settings_projects(
                self.paths
                    .iter()
                    .map(|path| SettingsProject {
                        path: path.clone(),
                        name: "Project".into(),
                        active: false,
                        disabled_reason: None,
                    })
                    .collect(),
                None,
                move |path, _, _| removed.borrow_mut().push(path),
                cx,
            )
        }
    }

    #[gpui::test]
    fn project_removal_callbacks_preserve_non_utf8_paths(cx: &mut TestAppContext) {
        use gpui::{AppContext, Modifiers};
        cx.update(gpui_component::init);
        let paths: Vec<PathBuf> = [0xfe, 0xff]
            .into_iter()
            .map(|byte| PathBuf::from(OsString::from_vec(vec![b'/', b'p', byte])))
            .collect();
        assert_eq!(paths[0].to_string_lossy(), paths[1].to_string_lossy());
        let removed = Rc::new(RefCell::new(Vec::new()));
        let expected = paths.clone();
        let output = removed.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            gpui_component::Root::new(cx.new(|_| Host { paths, removed }), window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        for id in [
            "remove-project-[47, 112, 255]",
            "remove-project-[47, 112, 254]",
        ] {
            let bounds = cx.debug_bounds(id).expect("project removal button");
            cx.simulate_click(bounds.center(), Modifiers::default());
            cx.run_until_parked();
        }
        assert_eq!(
            *output.borrow(),
            expected.into_iter().rev().collect::<Vec<_>>()
        );
    }
}
