//! Preview adapter over the imported, immutable workspace file inventory.
use super::SessionPreview;
use gpui::{prelude::*, *};
use threadlane_ui_kit::file_completion::{
    self as files, CompleteFileCompletion, DismissFileCompletion, FileCompletionMenuStatus,
    FileCompletionRequest, FileQueryTrigger, SelectNextFileCompletion,
    SelectPreviousFileCompletion,
};

impl SessionPreview {
    fn current_file_trigger(&self, cx: &App) -> Option<FileQueryTrigger> {
        let input = self.input.read(cx);
        let selection = input.selected_range();
        if self.editor_open || selection.start != selection.end || input.cursor() != selection.end {
            return None;
        }
        files::active_file_query(&input.value(), input.cursor())
    }

    pub(super) fn file_menu_open(&self, cx: &App) -> bool {
        !self.dismiss_file_menu && self.current_file_trigger(cx).is_some()
    }

    fn file_matches(&self, query: &str) -> (Vec<String>, bool) {
        let Some(Ok(inventory)) = &self.fixture.file_inventory else {
            return (Vec::new(), false);
        };
        let (matches, more) = files::filter_file_matches(
            query,
            &inventory.paths,
            files::FILE_COMPLETION_RESULT_LIMIT,
        );
        (matches.into_iter().cloned().collect(), more)
    }

    fn insert_file(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.file_menu_open(cx) || !files::is_safe_relative_path(path) {
            return;
        }
        let Some(trigger) = self.current_file_trigger(cx) else {
            return;
        };
        if !self
            .file_matches(&trigger.query)
            .0
            .iter()
            .any(|candidate| candidate == path)
        {
            return;
        }
        self.input.update(cx, |input, cx| {
            input.set_selected_range(trigger.range, cx);
            input.replace(files::format_path_insertion(path), window, cx);
            input.focus(window, cx);
        });
        self.selected_file_index = 0;
        cx.notify();
    }

    pub(super) fn insert_selected_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(trigger) = self.current_file_trigger(cx) else {
            return;
        };
        let matches = self.file_matches(&trigger.query).0;
        if let Some(path) = matches.get(
            self.selected_file_index
                .min(matches.len().saturating_sub(1)),
        ) {
            self.insert_file(path, window, cx);
        }
    }

    pub(super) fn complete_file_action(
        &mut self,
        _: &CompleteFileCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_menu_open(cx) {
            self.insert_selected_file(window, cx);
            cx.stop_propagation();
        }
    }
    pub(super) fn previous_file_action(
        &mut self,
        _: &SelectPreviousFileCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.file_menu_open(cx) {
            return;
        }
        self.selected_file_index = self.selected_file_index.saturating_sub(1);
        self.file_scroll_handle
            .scroll_to_item(self.selected_file_index);
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn next_file_action(
        &mut self,
        _: &SelectNextFileCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(trigger) = self
            .current_file_trigger(cx)
            .filter(|_| !self.dismiss_file_menu)
        else {
            return;
        };
        let count = self.file_matches(&trigger.query).0.len();
        self.selected_file_index = (self.selected_file_index + 1).min(count.saturating_sub(1));
        self.file_scroll_handle
            .scroll_to_item(self.selected_file_index);
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn dismiss_file_action(
        &mut self,
        _: &DismissFileCompletion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.file_menu_open(cx) {
            return;
        }
        self.dismiss_file_menu = true;
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_file_completion(&self, cx: &mut Context<Self>) -> Option<Div> {
        let trigger = self
            .current_file_trigger(cx)
            .filter(|_| !self.dismiss_file_menu)?;
        let (matches, has_more) = self.file_matches(&trigger.query);
        let scope = self
            .session
            .runtime_work_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into());
        let status = match &self.fixture.file_inventory {
            Some(Ok(inventory)) => FileCompletionMenuStatus::Ready {
                matches: &matches,
                selected_index: self.selected_file_index,
                has_more,
                non_utf8_skipped: inventory.non_utf8_skipped,
            },
            Some(Err(error))
                if error.contains(threadlane_protocol::repo::FILE_INVENTORY_NOT_A_REPOSITORY) =>
            {
                FileCompletionMenuStatus::Unavailable("File completion requires a Git workspace")
            }
            Some(Err(error)) => FileCompletionMenuStatus::Failed(error),
            None => FileCompletionMenuStatus::Unavailable(
                "No file inventory captured. Import the session again to preview workspace files.",
            ),
        };
        let owner = cx.entity().downgrade();
        Some(files::file_completion_popup(files::file_completion_menu(
            &scope,
            status,
            &self.file_scroll_handle,
            move |request, window, cx| {
                let _ = owner.update(cx, |host, cx| match request {
                    FileCompletionRequest::Insert(path) => host.insert_file(path, window, cx),
                    FileCompletionRequest::Retry => {
                        use gpui_component::WindowExt;
                        window.push_notification(
                            gpui_component::notification::Notification::info(
                                "Import the session again to refresh captured workspace files",
                            ),
                            cx,
                        );
                    }
                });
            },
            cx,
        )))
    }
}
