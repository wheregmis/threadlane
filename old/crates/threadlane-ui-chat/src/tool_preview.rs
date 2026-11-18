//! Native file-opening capabilities for the shared tool preview renderer.
use super::markdown::{classify_chat_link, ChatLinkTarget};
use super::tool_detail::{args_json, args_path};
use gpui::*;
use gpui_component::button::Button;
use threadlane_ui_state::{actions::AppAction, controller, AppState, ToolActivityInfo};
fn open_button(
    id: String,
    path: String,
    line: Option<usize>,
    folder: bool,
    model: &Entity<AppState>,
) -> Button {
    let target = match classify_chat_link(&path) {
        ChatLinkTarget::ProjectFile(path) if !path.contains(":/") => Some(path),
        _ => None,
    };
    let model = model.clone();
    threadlane_ui_kit::tool_preview::open_button(
        id,
        &path,
        line,
        folder,
        target.is_some(),
        move |_, _, cx| {
            let Some(path) = target.clone() else {
                return;
            };
            if folder {
                let Some(root) = model.read(cx).active_git_work_dir() else {
                    return;
                };
                if let Ok(path) = threadlane_tools::validate_path_in_workspace(&path, &root) {
                    cx.reveal_path(&path);
                }
            } else {
                model.update(cx, |state, cx| {
                    let action = match line {
                        Some(line) => AppAction::OpenFileInEditorAtLine { path, line },
                        None => AppAction::OpenFileInEditor(path),
                    };
                    controller::dispatch(state, action);
                    cx.notify();
                });
            }
        },
    )
}
pub(crate) fn render(
    activity: &ToolActivityInfo,
    model: &Entity<AppState>,
    cx: &mut App,
) -> Option<AnyElement> {
    let args = args_json(&activity.arguments).unwrap_or_default();
    let path = threadlane_tools::read_file_snapshot_path(&activity.detail)
        .or_else(|| args_path(&args))
        .unwrap_or_else(|| ".".into());
    let entry_base = if std::path::Path::new(&path).is_absolute() {
        model
            .read(cx)
            .active_git_work_dir()
            .and_then(|root| {
                let validated = threadlane_tools::validate_path_in_workspace(&path, &root).ok()?;
                let canonical_root = root.canonicalize().ok()?;
                validated
                    .strip_prefix(canonical_root)
                    .ok()
                    .map(std::path::PathBuf::from)
            })
            .unwrap_or_else(|| std::path::PathBuf::from(&path))
    } else {
        std::path::PathBuf::from(&path)
    };
    threadlane_ui_kit::tool_preview::render(
        activity,
        path,
        entry_base,
        |id, path, line, folder| open_button(id, path, line, folder, model),
        cx,
    )
}
#[cfg(test)]
mod tests {
    #[gpui::test]
    fn previews_show_results_trap_scroll_and_open_files(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use gpui_component::Root;
        use threadlane_ui_state::{AppState, RequestedEditorTarget, ToolActivityInfo};
        struct Harness {
            model: gpui::Entity<AppState>,
            activity: ToolActivityInfo,
            outer_scrolls: std::rc::Rc<std::cell::Cell<usize>>,
        }
        impl gpui::Render for Harness {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
                let outer_scrolls = self.outer_scrolls.clone();
                gpui::div()
                    .id("outer-scroll")
                    .size_full()
                    .on_scroll_wheel(move |_, _, _| {
                        outer_scrolls.set(outer_scrolls.get() + 1);
                    })
                    .child(super::render(&self.activity, &self.model, cx).unwrap())
            }
        }
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.rs"), "fn sample() {}\n".repeat(80)).unwrap();
        let model = cx.new(|_| {
            let mut state = AppState::default();
            state.active_work_dir = Some(dir.path().to_path_buf());
            state.active_session_id = None;
            state.is_new_task = false;
            state
        });
        let retained_model = model.clone();
        let outer_scrolls = std::rc::Rc::new(std::cell::Cell::new(0));
        let retained_scrolls = outer_scrolls.clone();
        let holder = std::rc::Rc::new(std::cell::RefCell::new(None));
        let holder_clone = holder.clone();
        let activity = ToolActivityInfo {
            id: "preview".into(),
            title: "read_file".into(),
            category: "Completed".into(),
            display_summary: "Read sample.rs".into(),
            arguments: r#"{"path":"sample.rs"}"#.into(),
            detail: (10..80)
                .map(|no| format!("{no}:a3f|fn sample() {{}}\n"))
                .collect(),
            is_expanded: false,
        };
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let harness = cx.new(|_| Harness {
                model,
                activity,
                outer_scrolls,
            });
            holder_clone.borrow_mut().replace(harness.clone());
            Root::new(harness, window, cx)
        });
        let harness = holder.borrow().as_ref().unwrap().clone();
        for (tool, selector, expected_line, absolute_listing) in [
            ("read_file", "tool-preview-open", Some(10), false),
            (
                "grep_search",
                "tool-preview-search-match-sample.rs-10",
                Some(10),
                false,
            ),
            (
                "list_dir",
                "tool-preview-directory-entry-sample.rs",
                None,
                false,
            ),
            (
                "list_dir",
                "tool-preview-directory-entry-sample.rs",
                None,
                true,
            ),
        ] {
            retained_model.update(cx, |model, _| model.requested_editor_target = None);
            if tool != "read_file" {
                harness.update(cx, |harness, cx| {
                    harness.activity.title = tool.into();
                    harness.activity.arguments = r#"{"path":".","pattern":"sample"}"#.into();
                    if absolute_listing {
                        harness.activity.arguments =
                            serde_json::json!({"path": dir.path()}).to_string();
                    }
                    harness.activity.detail = if tool == "grep_search" {
                        (10..80)
                            .map(|no| format!("sample.rs:{no}:fn sample() {{}}\n"))
                            .collect()
                    } else {
                        "[FILE] sample.rs\n".to_string()
                            + &(0..70)
                                .map(|ix| format!("[FILE] another-{ix}.rs\n"))
                                .collect::<String>()
                    };
                    cx.notify();
                });
            }
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let viewport = cx
                .debug_bounds("tool-preview-viewport")
                .expect("preview is visible without disclosure");
            assert!(viewport.size.height <= gpui::px(96.));
            let button = cx.debug_bounds(selector).expect("file action is visible");
            retained_model.read_with(cx, |model, _| {
                assert!(
                    model.active_git_work_dir().is_some(),
                    "active checkout missing"
                )
            });
            cx.simulate_click(button.center(), gpui::Modifiers::default());
            cx.run_until_parked();
            retained_model.read_with(cx, |model, _| {
                assert_eq!(
                    model.requested_editor_target,
                    Some(RequestedEditorTarget::File {
                        project: dir.path().to_path_buf(),
                        path: "sample.rs".into(),
                        line: expected_line,
                    }),
                    "tool {tool}, button {button:?}, status {:?}",
                    model.session_status
                )
            });
            let before = cx.debug_bounds("tool-preview-content").unwrap();
            for delta in [-40., -10000., -40., 10000., 40.] {
                cx.simulate_event(gpui::ScrollWheelEvent {
                    position: viewport.center(),
                    delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(delta))),
                    ..Default::default()
                });
                cx.run_until_parked();
                cx.update(|window, cx| window.draw(cx).clear(cx));
                if delta == -40. {
                    assert!(
                        cx.debug_bounds("tool-preview-content").unwrap().origin.y < before.origin.y,
                        "tool {tool} must scroll its content: before {before:?}, after {:?}",
                        cx.debug_bounds("tool-preview-content")
                    );
                }
            }
            assert_eq!(
                retained_scrolls.get(),
                0,
                "wheel events stay inside every preview, including at edges"
            );
        }
        retained_model.update(cx, |model, _| model.requested_editor_target = None);
        harness.update(cx, |harness, cx| {
            harness.activity.arguments =
                serde_json::json!({"path": dir.path().parent().unwrap()}).to_string();
            harness.activity.detail = "[FILE] sample.rs\n".into();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let button = cx
            .debug_bounds("tool-preview-directory-entry-sample.rs")
            .unwrap();
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        retained_model.read_with(cx, |model, _| {
            assert!(
                model.requested_editor_target.is_none(),
                "outside-workspace targets stay disabled"
            );
        });
    }
}
