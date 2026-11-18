//! Interactive presentation states; all paths here are explicitly local samples.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Selectable, Sizable};
use threadlane_ui_kit::file_completion::{
    file_completion_menu, FileCompletionMenuStatus, FileCompletionRequest,
};

pub struct FilePickerSamples {
    mode: usize,
    paths: Vec<String>,
    selected: usize,
    picked: Option<String>,
    scroll: ScrollHandle,
}
impl FilePickerSamples {
    pub fn new() -> Self {
        Self { mode: 0, selected: 0, picked: None, scroll: ScrollHandle::new(), paths: vec![
            "crates/threadlane-ui-kit/src/file_completion.rs".into(),
            "crates/a-very-long-workspace-directory/src/a-very-long-file-name-that-must-fit-in-a-narrow-composer.rs".into(),
            "docs/日本 語/keyboard shortcuts.md".into(),
        ] }
    }
}
impl Render for FilePickerSamples {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = div().min_w_0().flex().flex_wrap().gap_1().children(
            [
                "Matches",
                "Loading",
                "Empty",
                "Failed",
                "Unavailable",
                "Capped",
            ]
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                Button::new(SharedString::from(format!("file-picker-sample-{index}")))
                    .label(label)
                    .small()
                    .ghost()
                    .selected(self.mode == index)
                    .on_click(cx.listener(move |host, _, _, cx| {
                        host.mode = index;
                        cx.notify();
                    }))
            }),
        );
        let status = match self.mode {
            1 => FileCompletionMenuStatus::Loading,
            2 => FileCompletionMenuStatus::Ready {
                matches: &[],
                selected_index: 0,
                has_more: false,
                non_utf8_skipped: 0,
            },
            3 => FileCompletionMenuStatus::Failed("Workspace connection lost. Try again."),
            4 => FileCompletionMenuStatus::Unavailable("File completion requires a Git workspace"),
            _ => FileCompletionMenuStatus::Ready {
                matches: &self.paths,
                selected_index: self.selected,
                has_more: self.mode == 5,
                non_utf8_skipped: 2,
            },
        };
        let owner = cx.entity().downgrade();
        let menu = file_completion_menu(
            "a-very-long-workspace-name",
            status,
            &self.scroll,
            move |request, _, cx| {
                let _ = owner.update(cx, |host, cx| {
                    match request {
                        FileCompletionRequest::Insert(path) => {
                            host.selected = host
                                .paths
                                .iter()
                                .position(|candidate| candidate == path)
                                .unwrap_or(0);
                            host.picked = Some(format!("Selected sample: {path}"));
                        }
                        FileCompletionRequest::Retry => {
                            host.mode = 0;
                            host.picked = Some("Sample retry restored the file list.".into());
                        }
                    }
                    cx.notify();
                });
            },
            cx,
        );
        div().min_w_0().flex().flex_col().gap_2().child(controls).child(menu)
            .child(div().min_w_0().text_sm().text_color(cx.theme().muted_foreground)
                .child(self.picked.clone().unwrap_or_else(|| "Select a sample row; real @ queries are available in the saved-session composer.".into())))
    }
}
