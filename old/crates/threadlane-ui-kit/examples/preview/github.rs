use gpui::{prelude::*, *};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::resizable::ResizableState;
use gpui_component::scroll::Scrollbar;
use gpui_component::text::TextViewState;
use gpui_component::WindowExt;
use std::collections::{HashMap, HashSet};
use threadlane_protocol::repo::{
    GitHubIssueComment, GitHubIssueDetail, GitHubIssueSummary, GitHubLabel, GitHubPrCommit,
    GitHubPrFile, GitHubPrInfo, PrCheckStatus, PrConversationComment, PrReview, PrReviewComment,
};
use threadlane_ui_kit::github::{
    self as kit, GitHubDetailAction, GitHubListAction as Action, GitHubStateFilter as State,
    PrDetailTab,
};
#[path = "github_issue_dialog.rs"]
mod issue_dialog;

actions!(
    github_preview,
    [
        PreviousItem,
        NextItem,
        OpenItem,
        PreviousDetailTab,
        NextDetailTab,
        PreviousFile,
        NextFile,
        OpenFile
    ]
);
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", PreviousItem, Some("GitHubPreviewList")),
        KeyBinding::new("down", NextItem, Some("GitHubPreviewList")),
        KeyBinding::new("enter", OpenItem, Some("GitHubPreviewList")),
        KeyBinding::new("left", PreviousDetailTab, Some("GitHubPreviewTabs")),
        KeyBinding::new("right", NextDetailTab, Some("GitHubPreviewTabs")),
        KeyBinding::new("up", PreviousFile, Some("GitHubPreviewFiles")),
        KeyBinding::new("down", NextFile, Some("GitHubPreviewFiles")),
        KeyBinding::new("enter", OpenFile, Some("GitHubPreviewFiles")),
    ]);
}
pub enum GitHubPreviewEvent {
    Close,
    Settings,
}
impl EventEmitter<GitHubPreviewEvent> for GitHubPreview {}

#[derive(Clone)]
struct SampleItem {
    id: String,
    project: String,
    title: String,
    number: usize,
    pull_request: bool,
    state: State,
    draft: bool,
    failed: bool,
}
enum SampleDetail {
    Issue(GitHubIssueDetail),
    PullRequest(GitHubPrInfo),
}
#[derive(Default)]
struct SamplePrDraft {
    body: String,
    reply: Option<(String, String)>,
}
pub struct GitHubPreview {
    project: String,
    pull_requests: bool,
    state: State,
    scope: String,
    items: Vec<SampleItem>,
    visible: Vec<usize>,
    selected: Option<String>,
    limit: usize,
    scenario: String,
    notice: Option<String>,
    query: Entity<InputState>,
    list: ListState,
    focus: FocusHandle,
    split: Entity<ResizableState>,
    detail: Option<SampleDetail>,
    detail_body: Entity<TextViewState>,
    comments: ListState,
    pr_tabs: HashMap<String, PrDetailTab>,
    tabs_focus: FocusHandle,
    commits: ListState,
    conversation_scroll: ScrollHandle,
    files: ListState,
    files_focus: FocusHandle,
    file_selections: HashMap<String, String>,
    viewed_files: HashMap<String, HashSet<String>>,
    file_diff: Entity<TextViewState>,
    diff_key: Option<(String, String)>,
    drafts: HashMap<String, SamplePrDraft>,
    comment_input: Entity<TextareaState>,
    reply_input: Entity<TextareaState>,
    draft_input_key: Option<String>,
    _draft_subscriptions: Vec<Subscription>,
    _query_subscription: Subscription,
}
impl GitHubPreview {
    pub fn new(project: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search sample issues and pull requests…")
        });
        let subscription = cx.subscribe(&query, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.reconcile(cx);
            }
        });
        let comment_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Add a comment…")
                .auto_grow(2, 6)
                .soft_wrap(true)
        });
        let reply_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Write a reply…")
                .auto_grow(2, 6)
                .soft_wrap(true)
        });
        let draft_subscriptions = [(comment_input.clone(), false), (reply_input.clone(), true)]
            .into_iter()
            .map(|(input, reply)| {
                cx.subscribe(&input, move |view, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change)
                        && view.pull_requests
                        && view.draft_input_key == view.selected
                    {
                        if let Some(id) = view.selected.clone() {
                            let draft = view.drafts.entry(id).or_default();
                            if reply {
                                if let Some((_, body)) = &mut draft.reply {
                                    *body = input.read(cx).value().to_string();
                                }
                            } else {
                                draft.body = input.read(cx).value().to_string();
                            }
                            cx.notify();
                        }
                    }
                })
            })
            .collect();
        let mut view = Self {
            project,
            pull_requests: false,
            state: State::Open,
            scope: String::new(),
            items: samples(),
            visible: Vec::new(),
            selected: None,
            limit: 10,
            scenario: "loaded".into(),
            notice: None,
            query,
            list: ListState::new(0, ListAlignment::Top, window.rem_size() * 5.5),
            focus: cx.focus_handle(),
            split: cx.new(|_| ResizableState::default()),
            detail: None,
            detail_body: cx.new(|cx| TextViewState::markdown("", cx)),
            comments: ListState::new(0, ListAlignment::Top, window.rem_size() * 6.),
            pr_tabs: HashMap::new(),
            tabs_focus: cx.focus_handle(),
            commits: ListState::new(0, ListAlignment::Top, window.rem_size() * 5.),
            conversation_scroll: ScrollHandle::new(),
            files: ListState::new(0, ListAlignment::Top, window.rem_size() * 3.25),
            files_focus: cx.focus_handle(),
            file_selections: HashMap::new(),
            viewed_files: HashMap::new(),
            file_diff: cx.new(|cx| TextViewState::markdown("", cx)),
            diff_key: None,
            drafts: HashMap::new(),
            comment_input,
            reply_input,
            draft_input_key: None,
            _draft_subscriptions: draft_subscriptions,
            _query_subscription: subscription,
        };
        view.reconcile(cx);
        view
    }
    pub fn select_kind(&mut self, pull_requests: bool, cx: &mut Context<Self>) {
        self.pull_requests = pull_requests;
        if !pull_requests
            && ["draft-", "diff-", "viewed-"]
                .iter()
                .any(|prefix| self.scenario.starts_with(prefix))
        {
            self.scenario = "loaded".into();
        }
        if !pull_requests && self.state == State::Merged {
            self.state = State::Open;
        }
        self.limit = 10;
        self.reconcile(cx);
    }
    fn title(&self) -> &'static str {
        if self.pull_requests {
            "Pull requests"
        } else {
            "Issues"
        }
    }
    fn scope_label(&self) -> String {
        self.project_label(&self.scope)
    }
    fn selected_project_label(&self) -> String {
        self.items.iter().find(|item| Some(item.id.as_str()) == self.selected.as_deref())
            .map(|item| self.project_label(&item.project)).unwrap_or_else(|| self.scope_label())
    }
    fn project_label(&self, project: &str) -> String {
        match project {
            "sample-local" => self.project.clone(),
            "sample-other" => "Other project · sample".into(),
            _ => "All projects".into(),
        }
    }
    fn matching(&self, cx: &App) -> Vec<usize> {
        let query = self.query.read(cx).value().to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.pull_request == self.pull_requests
                    && item.state == self.state
                    && (self.scope.is_empty() || self.scope == item.project)
                    && format!("{} {} {}", item.title, item.number, item.project)
                        .to_lowercase()
                        .contains(query.as_str())
            })
            .map(|(ix, _)| ix)
            .collect()
    }
    fn has_more(&self, cx: &App) -> bool {
        self.matching(cx).len() > self.limit
    }
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        self.visible = self.matching(cx).into_iter().take(self.limit).collect();
        let previous = self.selected.clone();
        self.selected = self
            .selected
            .take()
            .filter(|id| self.visible.iter().any(|ix| self.items[*ix].id == *id))
            .or_else(|| self.visible.first().map(|ix| self.items[*ix].id.clone()));
        self.list
            .reset(self.visible.len() + usize::from(self.has_more(cx)));
        if previous != self.selected {
            self.notice = None;
        }
        self.sync_detail(cx);
        cx.notify();
    }
    fn request(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::State(state) => {
                self.state = state;
                self.limit = 10;
                self.reconcile(cx);
            }
            Action::Refresh => {
                self.scenario = "loaded".into();
                self.notice = None;
                self.reconcile(cx);
            }
            Action::LoadMore => {
                self.limit += 10;
                self.reconcile(cx);
            }
            Action::Settings => cx.emit(GitHubPreviewEvent::Settings),
            Action::NewIssue => {
                let owner = cx.entity().downgrade();
                issue_dialog::open(
                    format!("{} · local preview", self.scope_label()),
                    self.scenario == "issue-create-error",
                    move |title, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.notice = Some(format!("Created local sample: {title}. No GitHub issue was created."));
                            cx.notify();
                        });
                    }, window, cx,
                );
            }
            Action::AttachProject => {
                self.notice =
                    Some("Project attachment is a preview sample. No folders were opened.".into())
            }
        }
        cx.notify();
    }
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let current = self
            .visible
            .iter()
            .position(|ix| Some(self.items[*ix].id.as_str()) == self.selected.as_deref())
            .unwrap_or(0);
        let ix = current
            .saturating_add_signed(delta)
            .min(self.visible.len() - 1);
        self.select_item(self.items[self.visible[ix]].id.clone(), cx);
        self.list.scroll_to_reveal_item(ix);
    }
    fn select_item(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&id) {
            self.notice = None;
        }
        self.selected = Some(id);
        self.sync_detail(cx);
        cx.notify();
    }
    fn sync_detail(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .items
            .iter()
            .find(|item| Some(item.id.as_str()) == self.selected.as_deref());
        self.detail = selected.map(sample_detail);
        let (body, count) = match &self.detail {
            Some(SampleDetail::Issue(issue)) => (issue.body.as_str(), issue.comments.len()),
            Some(SampleDetail::PullRequest(pr)) => (pr.body.as_str(), 0),
            None => ("", 0),
        };
        self.detail_body
            .update(cx, |text, cx| text.set_text(body, cx));
        self.comments.reset(count);
        self.commits.reset(match &self.detail {
            Some(SampleDetail::PullRequest(pr)) => pr.commits.len(),
            _ => 0,
        });
        let files = match &self.detail {
            Some(SampleDetail::PullRequest(pr)) => pr.files.as_slice(),
            _ => &[],
        };
        self.files.reset(files.len());
        if let Some(id) = &self.selected {
            if !self
                .file_selections
                .get(id)
                .is_some_and(|path| files.iter().any(|file| &file.path == path))
            {
                if let Some(file) = files.first() {
                    self.file_selections.insert(id.clone(), file.path.clone());
                } else {
                    self.file_selections.remove(id);
                }
            }
        }
        self.sync_file_diff(cx);
    }
    fn selected_file(&self) -> Option<&str> {
        self.selected
            .as_ref()
            .and_then(|id| self.file_selections.get(id))
            .map(String::as_str)
    }
    fn sync_file_diff(&mut self, cx: &mut Context<Self>) {
        let key = self
            .selected
            .clone()
            .zip(self.selected_file().map(str::to_owned));
        if self.diff_key == key {
            return;
        }
        let text = key.as_ref().map(|(_, path)| {
            // Fixed local fixture; production keeps its canonical diff preparation and file guards.
            let diff = if path.ends_with(".png") {
                format!("diff --git a/{path} b/{path}\nBinary files a/{path} and b/{path} differ")
            } else if path.ends_with("obsolete.rs") {
                format!("diff --git a/{path} b/{path}\ndeleted file mode 100644\n--- a/{path}\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-fn obsolete_review() {{\n-}}")
            } else {
                let (old_path, rename) = if path == "src/café file.rs" {
                    ("src/legacy file.rs", "similarity index 50%\nrename from src/legacy file.rs\nrename to src/café file.rs\n")
                } else { (path.as_str(), "") };
                format!("diff --git a/{old_path} b/{path}\n{rename}--- a/{old_path}\n+++ b/{path}\n@@ -1,3 +1,6 @@\n fn render_review() {{\n-    render_desktop_only();\n+    render_shared_files();\n+    retain_selection();\n+    preserve_viewed_guards();\n+    support_keyboard_navigation();\n }}")
            };
            format!("```diff\n{diff}\n```")
        }).unwrap_or_default();
        self.file_diff = cx.new(|cx| TextViewState::markdown(&text, cx));
        self.diff_key = key;
    }
    fn select_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(SampleDetail::PullRequest(pr)) = &self.detail else {
            return;
        };
        let Some(ix) = pr.files.iter().position(|file| file.path == path) else {
            return;
        };
        let Some(id) = &self.selected else {
            return;
        };
        self.file_selections.insert(id.clone(), path);
        self.files.scroll_to_reveal_item(ix);
        self.sync_file_diff(cx);
        cx.notify();
    }
    fn apply_file_action(&mut self, action: kit::PrFileAction, cx: &mut Context<Self>) {
        let Some(SampleDetail::PullRequest(pr)) = &self.detail else {
            return;
        };
        let current = pr
            .files
            .iter()
            .position(|file| Some(file.path.as_str()) == self.selected_file());
        if let Some(ix) = kit::pr_file_action_ix(current, pr.files.len(), action) {
            self.select_file(pr.files[ix].path.clone(), cx);
        }
    }
    fn next_unviewed(&self, pr: &GitHubPrInfo) -> Option<String> {
        let viewed = self
            .selected
            .as_ref()
            .and_then(|id| self.viewed_files.get(id));
        let start = pr
            .files
            .iter()
            .position(|file| Some(file.path.as_str()) == self.selected_file())
            .unwrap_or_default();
        pr.files
            .iter()
            .cycle()
            .skip(start + 1)
            .take(pr.files.len().saturating_sub(1))
            .find(|file| !viewed.is_some_and(|paths| paths.contains(&file.path)))
            .map(|file| file.path.clone())
    }
    fn viewed_enabled(&self) -> bool {
        !self.scenario.starts_with("viewed-") && !self.scenario.starts_with("diff-")
    }
    fn request_file_review(&mut self, action: kit::GitHubFileReviewAction, cx: &mut Context<Self>) {
        match action {
            kit::GitHubFileReviewAction::SetViewed(checked) => {
                if !self.viewed_enabled() {
                    return;
                }
                let Some(id) = self.selected.clone() else {
                    return;
                };
                let Some(path) = self.selected_file().map(str::to_owned) else {
                    return;
                };
                let viewed = self.viewed_files.entry(id).or_default();
                if checked {
                    viewed.insert(path);
                } else {
                    viewed.remove(&path);
                }
                self.notice =
                    Some("Viewed marker changed locally. No GitHub request was made.".into());
            }
            kit::GitHubFileReviewAction::NextUnviewed => {
                let Some(SampleDetail::PullRequest(pr)) = &self.detail else {
                    return;
                };
                if let Some(path) = self.next_unviewed(pr) {
                    self.select_file(path, cx);
                }
            }
        }
        cx.notify();
    }
    fn render_file(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(SampleDetail::PullRequest(pr)) = &self.detail else {
            return Empty.into_any_element();
        };
        let Some(file) = pr.files.get(ix) else {
            return Empty.into_any_element();
        };
        let viewed = self
            .selected
            .as_ref()
            .and_then(|id| self.viewed_files.get(id))
            .is_some_and(|paths| paths.contains(&file.path));
        let selected = self.selected_file() == Some(file.path.as_str());
        let marker = if selected && self.scenario == "viewed-pending" {
            Some("Saving…")
        } else if selected && self.scenario == "viewed-unknown" {
            Some("Couldn't confirm — refresh to settle")
        } else {
            viewed.then_some("Viewed")
        };
        let owner = cx.entity().downgrade();
        let path = file.path.clone();
        kit::github_pr_file_row(
            file,
            selected,
            marker,
            move |_, window, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.files_focus.focus(window, cx);
                    view.select_file(path.clone(), cx);
                });
            },
            cx,
        )
        .into_any_element()
    }
    fn render_files(&self, pr: &GitHubPrInfo, cx: &mut Context<Self>) -> AnyElement {
        if pr.files.is_empty() {
            return kit::github_empty("No changed files reported.".into(), false, cx)
                .into_any_element();
        }
        let viewed = self
            .selected
            .as_ref()
            .and_then(|id| self.viewed_files.get(id));
        let count = pr
            .files
            .iter()
            .filter(|file| viewed.is_some_and(|paths| paths.contains(&file.path)))
            .count();
        let progress = if self.scenario == "viewed-loading" {
            "Loading viewed status…".into()
        } else if count == pr.files.len() {
            "All listed files viewed".into()
        } else {
            format!("{count} of {} listed files viewed", pr.files.len())
        };
        let mut controls = kit::GitHubPrFilesControls::new(
            progress,
            self.next_unviewed(pr)
                .map(|_| "Select the next file without a viewed marker".into()),
        );
        if let Some(path) = self.selected_file() {
            let help = if self.scenario == "viewed-pending" {
                format!("{path}: saving to GitHub…")
            } else if self.scenario == "viewed-unknown" {
                format!("{path}: couldn't confirm — use Refresh to settle")
            } else if self.scenario == "viewed-loading" {
                format!("{path}: loading viewed status…")
            } else if self.scenario.starts_with("diff-") {
                format!("{path}: load the diff before marking viewed")
            } else if self.scenario == "viewed-error" {
                format!("{path}: viewed status unavailable — refresh to retry")
            } else {
                format!("{path}: saved to GitHub for your signed-in account")
            };
            controls = controls.viewed(
                viewed.is_some_and(|paths| paths.contains(path)),
                self.viewed_enabled(),
                help.into(),
            );
        }
        controls = controls.error(
            (self.scenario == "viewed-error")
                .then(|| "Couldn't load viewed status. Refresh to retry.".into()),
        );
        let owner = cx.entity().downgrade();
        let toolbar = controls.render(
            move |action, _, cx| {
                let _ = owner.update(cx, |view, cx| view.request_file_review(action, cx));
            },
            cx,
        );
        let files = kit::github_pr_file_list(&self.files_focus, cx)
            .key_context("GitHubPreviewFiles")
            .on_action(cx.listener(|view, _: &PreviousFile, _, cx| {
                view.apply_file_action(kit::PrFileAction::Previous, cx)
            }))
            .on_action(cx.listener(|view, _: &NextFile, _, cx| {
                view.apply_file_action(kit::PrFileAction::Next, cx)
            }))
            .on_action(cx.listener(|view, _: &OpenFile, _, cx| {
                view.apply_file_action(kit::PrFileAction::Open, cx)
            }))
            .child(
                list(
                    self.files.clone(),
                    cx.processor(|view, ix, _, cx| view.render_file(ix, cx)),
                )
                .size_full()
                .with_sizing_behavior(ListSizingBehavior::Infer),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&self.files)),
            );
        let owner = cx.entity().downgrade();
        let diff = kit::github_pr_diff(
            &self.file_diff,
            self.scenario == "diff-loading",
            (self.scenario == "diff-error")
                .then(|| "Couldn't load this file's diff. Retry to keep the selected file.".into()),
            move |_, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.scenario = "loaded".into();
                    cx.notify();
                });
            },
            cx,
        );
        kit::github_pr_files(
            toolbar.into_any_element(),
            files.into_any_element(),
            diff.into_any_element(),
        )
        .into_any_element()
    }
    fn draft_inputs_match(&self, cx: &App) -> bool {
        let draft = self.selected.as_ref().and_then(|id| self.drafts.get(id));
        self.draft_input_key == self.selected
            && self.comment_input.read(cx).value().as_str()
                == draft.map(|draft| draft.body.as_str()).unwrap_or_default()
            && self.reply_input.read(cx).value().as_str()
                == draft
                    .and_then(|draft| draft.reply.as_ref())
                    .map(|(_, body)| body.as_str())
                    .unwrap_or_default()
    }
    fn sync_draft_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft_input_key = self.selected.clone();
        let draft = self.selected.as_ref().and_then(|id| self.drafts.get(id));
        self.comment_input.update(cx, |input, cx| {
            input.set_value(
                draft.map(|draft| draft.body.as_str()).unwrap_or_default(),
                window,
                cx,
            )
        });
        self.reply_input.update(cx, |input, cx| {
            input.set_value(
                draft
                    .and_then(|draft| draft.reply.as_ref())
                    .map(|(_, body)| body.as_str())
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
    }
    fn select_reply(&mut self, remote_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let draft = self.drafts.entry(id).or_default();
        if draft
            .reply
            .as_ref()
            .is_some_and(|(id, body)| id != &remote_id && !body.trim().is_empty())
        {
            self.notice =
                Some("Post or clear this reply draft before replying to another comment.".into());
        } else {
            if draft.reply.as_ref().is_none_or(|(id, _)| id != &remote_id) {
                draft.reply = Some((remote_id, String::new()));
            }
            self.sync_draft_inputs(window, cx);
        }
        kit::focus_github_reply(&self.reply_input, &self.conversation_scroll, window, cx);
        cx.notify();
    }
    fn request_draft(
        &mut self,
        reply: bool,
        action: kit::GitHubDraftAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == kit::GitHubDraftAction::ClearDraft {
            if let Some(draft) = self
                .selected
                .as_ref()
                .and_then(|id| self.drafts.get_mut(id))
            {
                if reply {
                    draft.reply = None;
                } else {
                    draft.body.clear();
                }
            }
            self.sync_draft_inputs(window, cx);
            self.notice = None;
        } else {
            self.notice =
                Some("Local draft preview. No comment or reply was sent to GitHub.".into());
        }
        cx.notify();
    }
    fn render_draft(&self, reply: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let draft = self.selected.as_ref().and_then(|id| self.drafts.get(id));
        let target = if reply {
            let (id, _) = draft?.reply.as_ref()?;
            let Some(SampleDetail::PullRequest(detail)) = &self.detail else {
                return None;
            };
            Some(
                detail
                    .review_comments
                    .iter()
                    .find(|comment| &comment.remote_id == id)?,
            )
        } else {
            None
        };
        let body = if reply {
            draft
                .and_then(|draft| draft.reply.as_ref())
                .map(|(_, body)| body.as_str())
        } else {
            draft.map(|draft| draft.body.as_str())
        }
        .unwrap_or_default();
        let busy = self.scenario == "draft-publishing";
        let failed = self.scenario == "draft-error";
        let unknown = self.scenario == "draft-unknown";
        let action = if unknown {
            kit::GitHubDraftAction::CheckAgain
        } else if failed {
            kit::GitHubDraftAction::Retry
        } else {
            kit::GitHubDraftAction::Post
        };
        let owner = cx.entity().downgrade();
        let submit = kit::github_draft_action(
            reply,
            action,
            busy || (!unknown && body.trim().is_empty()),
            move |_, window, cx| {
                let _ = owner.update(cx, |view, cx| view.request_draft(reply, action, window, cx));
            },
        )
        .into_any_element();
        let owner = cx.entity().downgrade();
        let clear = kit::github_draft_action(
            reply,
            kit::GitHubDraftAction::ClearDraft,
            busy,
            move |_, window, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.request_draft(reply, kit::GitHubDraftAction::ClearDraft, window, cx)
                });
            },
        )
        .into_any_element();
        let status = if busy {
            Some("Publishing…".into())
        } else if unknown {
            Some("GitHub could not confirm whether the submitted draft was published. Draft retained.".into())
        } else {
            None
        };
        let (before, after) = if failed || unknown {
            (vec![submit], vec![clear])
        } else {
            (vec![], vec![submit, clear])
        };
        let mut editor = kit::GitHubCommentEditor::new(
            if reply {
                self.reply_input.clone()
            } else {
                self.comment_input.clone()
            },
            reply,
        )
        .controls(before, after)
        .status(status, busy, unknown)
        .error(failed.then(|| "Couldn’t publish this sample draft. Your text is retained.".into()));
        if let Some(target) = target {
            editor = editor.target(
                target.author.clone(),
                target.body.clone(),
                target.path.as_ref().map(|path| {
                    format!(
                        "{path}{}",
                        target
                            .line
                            .map(|line| format!(":{line}"))
                            .unwrap_or_default()
                    )
                }),
            );
        }
        Some(editor.render(cx).into_any_element())
    }
    fn render_pr_conversation(&self, detail: &GitHubPrInfo, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        let entry = |remote: &str,
                     label: &str,
                     author: &str,
                     time: &str,
                     body: &str,
                     review: bool,
                     location: Option<String>,
                     reply: bool| {
            let owner = owner.clone();
            let remote = remote.to_owned();
            kit::github_conversation_row(
                kit::GitHubConversationEntry::new(
                    format!("{}-{remote}", self.selected.as_deref().unwrap_or_default()),
                    label.into(),
                    author.into(),
                    time.into(),
                    body.into(),
                )
                .review(review)
                .location(location)
                .reply_author(reply.then(|| author.to_owned())),
                move |action, window, cx| {
                    let _ = owner.update(cx, |view, cx| match action {
                        kit::GitHubConversationAction::Reply => {
                            view.select_reply(remote.clone(), window, cx)
                        }
                        kit::GitHubConversationAction::AskAgent => {
                            view.notice = Some(
                                "Draft reply is a local sample action. No agent was started."
                                    .into(),
                            );
                            cx.notify();
                        }
                    });
                },
                cx,
            )
            .into_any_element()
        };
        let mut rows = Vec::new();
        for comment in &detail.issue_comments {
            rows.push(entry(
                &comment.remote_id,
                "Comment",
                &comment.author,
                &comment.created_at,
                &comment.body,
                false,
                None,
                false,
            ));
        }
        for review in &detail.reviews {
            rows.push(entry(
                &review.remote_id,
                "Changes requested",
                &review.author,
                &review.submitted_at,
                &review.body,
                true,
                None,
                false,
            ));
        }
        for comment in &detail.review_comments {
            rows.push(entry(
                &comment.remote_id,
                "Inline comment",
                &comment.author,
                &comment.created_at,
                &comment.body,
                false,
                comment.path.as_ref().map(|path| {
                    format!(
                        "{path}{}",
                        comment
                            .line
                            .map(|line| format!(":{line}"))
                            .unwrap_or_default()
                    )
                }),
                true,
            ));
        }
        kit::github_pr_conversation(
            rows,
            self.render_draft(false, cx).unwrap(),
            self.render_draft(true, cx),
            &self.conversation_scroll,
            cx,
        )
        .into_any_element()
    }
    fn render_commit(&self, ix: usize, cx: &App) -> AnyElement {
        let Some(SampleDetail::PullRequest(detail)) = &self.detail else {
            return Empty.into_any_element();
        };
        detail
            .commits
            .get(ix)
            .map(|commit| kit::github_pr_commit_row(commit, cx).into_any_element())
            .unwrap_or_else(|| Empty.into_any_element())
    }
    fn current_tab(&self) -> PrDetailTab {
        self.selected
            .as_ref()
            .and_then(|id| self.pr_tabs.get(id))
            .copied()
            .unwrap_or_default()
    }
    fn select_tab(&mut self, tab: PrDetailTab, cx: &mut Context<Self>) {
        if let Some(id) = &self.selected {
            self.pr_tabs.insert(id.clone(), tab);
        }
        cx.notify();
    }
    fn request_detail(
        &mut self,
        action: GitHubDetailAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            GitHubDetailAction::SetIssueClosed(closed) => {
                if let Some(item) = self
                    .items
                    .iter_mut()
                    .find(|item| Some(item.id.as_str()) == self.selected.as_deref())
                {
                    item.state = if closed { State::Closed } else { State::Open };
                }
                self.reconcile(cx);
            }
            GitHubDetailAction::SuggestLabels => {
                self.notice =
                    Some("Suggested labels are local samples. No model was called.".into())
            }
            GitHubDetailAction::StartTask => {
                let Some(SampleDetail::Issue(detail)) = &self.detail else {
                    return;
                };
                let selected = self.selected.clone();
                let number = detail.summary.issue.number;
                let identity = format!("threadlane/sample · #{number} · {}", self.selected_project_label());
                let owner = cx.entity().downgrade();
                issue_dialog::open_start(
                    identity,
                    detail.summary.title.clone(),
                    number,
                    &self.scenario,
                    move |form, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            if view.selected == selected {
                                view.notice = Some(format!(
                                    "Local task preview · {} · {} · {}. No agent was started.",
                                    form.model_label,
                                    form.effort.label(),
                                    form.mode.label()
                                ));
                                cx.notify();
                            }
                        });
                    },
                    window,
                    cx,
                );
            }
            GitHubDetailAction::DeleteIssue => {
                let Some(SampleDetail::Issue(detail)) = &self.detail else {
                    return;
                };
                let selected = self.selected.clone();
                let identity = format!(
                    "threadlane/sample · #{} · {}",
                    detail.summary.issue.number,
                    self.selected_project_label()
                );
                let title = detail.summary.title.clone();
                let owner = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let owner = owner.clone();
                    let selected = selected.clone();
                    kit::github_issue_delete_dialog(alert, identity.clone(), title.clone(), move |_, cx| {
                        owner.update(cx, |view, cx| {
                            if view.selected == selected {
                                view.notice = Some("Delete confirmed locally. No GitHub issue was deleted.".into());
                            } else {
                                view.notice = Some("Issue selection changed. Open the delete confirmation again.".into());
                            }
                            cx.notify();
                            true
                        }).unwrap_or(true)
                    })
                });
            }
            GitHubDetailAction::OpenTask => {
                self.notice =
                    Some("This linked task is a sample. The imported chat is unchanged.".into())
            }
            GitHubDetailAction::AddressReviews => {
                self.notice = Some(
                    "Review addressing is a local preview action. No agent was started.".into(),
                )
            }
        }
        cx.notify();
    }

    fn render_comment(&self, ix: usize, cx: &App) -> AnyElement {
        let Some(SampleDetail::Issue(detail)) = &self.detail else {
            return Empty.into_any_element();
        };
        let Some(comment) = detail.comments.get(ix) else {
            return Empty.into_any_element();
        };
        kit::github_comment(
            format!(
                "{}-{}",
                self.selected.as_deref().unwrap_or_default(),
                comment.remote_id
            ),
            comment.author.clone(),
            comment.created_at.clone(),
            comment.body.clone(),
            cx,
        )
        .into_any_element()
    }
    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.scenario == "no-project" || self.detail.is_none() {
            return kit::github_empty("Select an item to see details.".into(), false, cx)
                .into_any_element();
        }
        if self.scenario == "detail-error" {
            let owner = cx.entity().downgrade();
            return kit::github_error(
                "detail",
                "Couldn’t load details. Try refreshing.".into(),
                "Sample detail network error.".into(),
                false,
                move |action, window, cx| {
                    let _ = owner.update(cx, |view, cx| view.request(action, window, cx));
                },
                cx,
            )
            .into_any_element();
        }
        let owner = cx.entity().downgrade();
        let status = self
            .notice
            .clone()
            .or_else(|| (self.scenario == "detail-refresh").then(|| "Refreshing details…".into()))
            .or_else(|| {
                (self.scenario == "issue-pending")
                    .then(|| "Sample issue action in progress…".into())
            });
        match self.detail.as_ref().unwrap() {
            SampleDetail::Issue(detail) => {
                let controls = kit::GitHubIssueActions::new(detail.summary.issue.url.clone())
                    .linked_task(true)
                    .closed(detail.summary.state == "closed")
                    .pending(self.scenario == "issue-pending");
                let actions = kit::github_issue_actions(controls, move |action, window, cx| {
                    let _ = owner.update(cx, |view, cx| view.request_detail(action, window, cx));
                }, cx);
                let summary = &detail.summary;
                let header = kit::GitHubDetailHeader::new(
                    summary.title.clone(),
                    format!(
                        "threadlane/sample · #{} · {} · @{} · 2h ago",
                        summary.issue.number, summary.state, summary.author
                    ),
                    actions.into_any_element(),
                )
                .status(status)
                .render(cx);
                let owner = cx.entity().downgrade();
                let task = kit::github_linked_task(
                    kit::GitHubLinkedTask::new(
                        "github-sample-linked-task".into(),
                        "Refine shared conversation surfaces".into(),
                        self.selected_project_label(),
                        "Ready to review".into(),
                    )
                    .worktree(true)
                    .branch(Some("sample/shared-ui".into()))
                    .pr_number(Some(42)),
                    move |_, window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.request_detail(GitHubDetailAction::OpenTask, window, cx)
                        });
                    },
                    cx,
                );
                let comments = div()
                    .size_full()
                    .relative()
                    .child(
                        list(
                            self.comments.clone(),
                            cx.processor(|view, ix, _, cx| view.render_comment(ix, cx)),
                        )
                        .size_full()
                        .with_sizing_behavior(ListSizingBehavior::Infer),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::vertical(&self.comments)),
                    );
                let body = kit::github_issue_body(
                    detail,
                    &self.detail_body,
                    vec![task.into_any_element()],
                    Some((detail.comments.len(), comments.into_any_element())),
                    self.scenario == "detail-refresh",
                    cx,
                );
                kit::github_detail_surface(header, body.into_any_element()).into_any_element()
            }
            SampleDetail::PullRequest(detail) => {
                let actions = kit::github_pr_actions(
                    detail.url.clone(),
                    Some("Refine shared conversation surfaces".into()),
                    detail.state == "open",
                    move |action, window, cx| {
                        let _ = owner.update(cx, |view, cx| view.request_detail(action, window, cx));
                    },
                );
                let tab = self.current_tab();
                let owner = cx.entity().downgrade();
                let focus = self.tabs_focus.clone();
                let tabs = div()
                    .id("github-preview-pr-tabs-focus")
                    .role(Role::TabList)
                    .track_focus(&self.tabs_focus)
                    .key_context("GitHubPreviewTabs")
                    .on_action(cx.listener(|view, _: &PreviousDetailTab, _, cx| {
                        view.select_tab(view.current_tab().adjacent(-1), cx)
                    }))
                    .on_action(cx.listener(|view, _: &NextDetailTab, _, cx| {
                        view.select_tab(view.current_tab().adjacent(1), cx)
                    }))
                    .child(kit::github_pr_tabs(tab, move |tab, window, cx| {
                        focus.focus(window, cx);
                        let _ = owner.update(cx, |view, cx| view.select_tab(tab, cx));
                    }));
                let header = kit::GitHubDetailHeader::new(
                    detail.title.clone(),
                    format!(
                        "threadlane/sample · #{} · {} · @{} · 2h ago",
                        detail.number, detail.state, detail.author
                    ),
                    actions.into_any_element(),
                )
                .branch(format!("{} → {}", detail.head_ref, detail.base_ref))
                .tabs(tabs.into_any_element())
                .status(status)
                .render(cx);
                let body = match tab {
                    PrDetailTab::Summary => {
                        kit::github_pr_summary(detail, &self.detail_body, cx).into_any_element()
                    }
                    PrDetailTab::Conversation => self.render_pr_conversation(detail, cx),
                    PrDetailTab::Timeline => {
                        let list = (!detail.commits.is_empty()).then(|| {
                            div()
                                .relative()
                                .size_full()
                                .child(
                                    list(
                                        self.commits.clone(),
                                        cx.processor(|view, ix, _, cx| view.render_commit(ix, cx)),
                                    )
                                    .size_full()
                                    .with_sizing_behavior(ListSizingBehavior::Infer),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .child(Scrollbar::vertical(&self.commits)),
                                )
                                .into_any_element()
                        });
                        kit::github_pr_commits(detail.commits.len(), list, cx).into_any_element()
                    }
                    PrDetailTab::Code => self.render_files(detail, cx),
                };
                kit::github_detail_surface(header, body).into_any_element()
            }
        }
    }
    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix == self.visible.len() {
            return div()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(kit::github_load_more(self.scenario == "loading").on_click(
                    cx.listener(|view, _, window, cx| view.request(Action::LoadMore, window, cx)),
                ))
                .into_any_element();
        }
        let item = &self.items[self.visible[ix]];
        let project = if item.project == "sample-local" {
            self.project.clone()
        } else {
            "Other project · sample".into()
        };
        let kind = if item.pull_request {
            kit::GitHubRowKind::PullRequest
        } else if item.state == State::Closed {
            kit::GitHubRowKind::ClosedIssue
        } else {
            kit::GitHubRowKind::OpenIssue
        };
        let metadata = if item.pull_request {
            vec![
                (
                    if item.failed {
                        "1 check failing"
                    } else {
                        "Checks passing"
                    }
                    .into(),
                    item.failed,
                ),
                ("Review required".into(), false),
            ]
        } else {
            vec![
                ("ui".into(), false),
                ("sample".into(), false),
                ("1 linked task".into(), false),
            ]
        };
        let row = kit::GitHubListRow::new(
            item.id.clone(),
            kind,
            item.title.clone(),
            format!("threadlane/sample · {project} · #{}", item.number),
            "@sample-author · 2h ago".into(),
        )
        .selected(self.selected.as_deref() == Some(item.id.as_str()))
        .metadata(metadata)
        .suffix(if item.pull_request {
            item.draft.then(|| "Draft".into())
        } else {
            Some("3 comments".into())
        });
        let id = item.id.clone();
        kit::github_list_row(row, cx)
            .on_click(cx.listener(move |view, _, window, cx| {
                view.focus.focus(window, cx);
                view.select_item(id.clone(), cx);
            }))
            .into_any_element()
    }
    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.scenario == "no-project" {
            let owner = cx.entity().downgrade();
            return kit::github_no_project(
                move |action, window, cx| {
                    let _ = owner.update(cx, |view, cx| view.request(action, window, cx));
                },
                cx,
            )
            .into_any_element();
        }
        let raw = "Sample GitHub network error. This preview never sends GitHub requests.";
        if self.scenario == "error" {
            let owner = cx.entity().downgrade();
            return kit::github_error(
                "list",
                "GitHub is offline. Check your connection and refresh.".into(),
                raw.into(),
                false,
                move |action, window, cx| {
                    let _ = owner.update(cx, |view, cx| view.request(action, window, cx));
                },
                cx,
            )
            .into_any_element();
        }
        if self.visible.is_empty() {
            return kit::github_empty(
                "No sample results match these filters.".into(),
                self.scenario == "loading",
                cx,
            )
            .into_any_element();
        }
        let content = kit::github_list_surface(cx)
            .track_focus(&self.focus)
            .key_context("GitHubPreviewList")
            .on_action(cx.listener(|view, _: &PreviousItem, _, cx| view.move_selection(-1, cx)))
            .on_action(cx.listener(|view, _: &NextItem, _, cx| view.move_selection(1, cx)))
            .on_action(cx.listener(|view, _: &OpenItem, window, cx| {
                if view.pull_requests && view.detail.is_some() {
                    view.tabs_focus.focus(window, cx);
                }
            }))
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|view, ix, _, cx| view.render_row(ix, cx)),
                )
                .size_full()
                .with_sizing_behavior(ListSizingBehavior::Infer),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&self.list)),
            )
            .into_any_element();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .children((self.scenario == "partial").then(|| {
                kit::github_list_warning(
                    raw.into(),
                    false,
                    cx.listener(|view, _, window, cx| view.request(Action::Refresh, window, cx)),
                    cx,
                )
            }))
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }
}
impl Render for GitHubPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.draft_inputs_match(cx) {
            cx.defer_in(window, |view, window, cx| {
                if !view.draft_inputs_match(cx) {
                    view.sync_draft_inputs(window, cx);
                }
            });
        }
        let toolbar = kit::github_toolbar(
            self.title(),
            None,
            cx.listener(|_, _, _, cx| cx.emit(GitHubPreviewEvent::Close)),
            cx,
        );
        let toolbar = toolbar.when(
            self.current_tab() == PrDetailTab::Code && self.pull_requests,
            |toolbar| {
                toolbar.child(kit::github_back_to_pr_list().on_click(cx.listener(
                    |view, _, window, cx| {
                        view.select_tab(PrDetailTab::Summary, cx);
                        view.focus.focus(window, cx);
                    },
                )))
            },
        );
        let owner = cx.entity().downgrade();
        let scope = kit::github_scope_row(
            self.scope_label(),
            vec![
                ("".into(), "All projects".into()),
                ("sample-local".into(), self.project.clone()),
                ("sample-other".into(), "Other project · sample".into()),
            ],
            self.scope.clone(),
            move |id, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.scope = id;
                    view.reconcile(cx);
                });
            },
            cx,
        );
        let controls = kit::GitHubListControls::new(self.state)
            .pull_requests(self.pull_requests)
            .loading(self.scenario == "loading")
            .has_results(!self.visible.is_empty())
            .has_scope(self.scenario != "no-project")
            .single_project(!self.scope.is_empty() && self.scenario != "no-project");
        let owner = cx.entity().downgrade();
        let filters = kit::github_filters(
            &self.query,
            &controls,
            move |action, window, cx| {
                let _ = owner.update(cx, |view, cx| view.request(action, window, cx));
            },
            cx,
        );
        let master = kit::github_master(
            scope.into_any_element(),
            filters.into_any_element(),
            self.render_list(cx),
        );
        let detail = self.render_detail(cx);
        let content = if self.pull_requests && self.current_tab() == PrDetailTab::Code {
            detail
        } else {
            kit::github_master_detail(master.into_any_element(), detail, &self.split, window, cx)
        };
        let scenario_owner = cx.entity().downgrade();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(content))
            .child(kit::github_status(
                self.visible.len(),
                self.title(),
                self.scope_label(),
                self.state,
                false,
            ))
            .child(
                div()
                    .flex_none()
                    .p_2()
                    .child(threadlane_ui_kit::choice_picker(
                        "github-preview-state",
                        format!("Preview state: {}", scenario_label(&self.scenario)),
                        [
                            "loaded",
                            "loading",
                            "partial",
                            "error",
                            "no-project",
                            "detail-refresh",
                            "detail-error",
                            "issue-pending",
                            "issue-create-error",
                            "issue-start-error",
                            "issue-start-no-repo",
                            "issue-start-no-provider",
                            "draft-publishing",
                            "draft-error",
                            "draft-unknown",
                            "diff-loading",
                            "diff-error",
                            "viewed-loading",
                            "viewed-error",
                            "viewed-pending",
                            "viewed-unknown",
                        ]
                        .into_iter()
                        .filter(|id| {
                            self.pull_requests
                                || !["draft-", "diff-", "viewed-"]
                                    .iter()
                                    .any(|prefix| id.starts_with(prefix))
                        })
                        .map(|id| (id.into(), scenario_label(id).into()))
                        .collect(),
                        self.scenario.clone(),
                        false,
                        move |id, _, cx| {
                            let _ = scenario_owner.update(cx, |view, cx| {
                                view.scenario = id;
                                cx.notify();
                            });
                        },
                    )),
            )
    }
}

fn scenario_label(id: &str) -> &'static str {
    match id {
        "loading" => "Refreshing",
        "partial" => "Partial error",
        "error" => "Request failed",
        "no-project" => "No project",
        "detail-refresh" => "Refreshing details",
        "detail-error" => "Detail request failed",
        "issue-pending" => "Issue action pending",
        "issue-create-error" => "Issue creation fails once",
        "issue-start-error" => "Task start fails once",
        "issue-start-no-repo" => "Task requires Git repository",
        "issue-start-no-provider" => "Task needs provider",
        "draft-publishing" => "Draft publishing",
        "draft-error" => "Draft request failed",
        "draft-unknown" => "Draft publication uncertain",
        "diff-loading" => "Loading diff",
        "diff-error" => "Diff request failed",
        "viewed-loading" => "Loading viewed status",
        "viewed-error" => "Viewed status unavailable",
        "viewed-pending" => "Saving viewed status",
        "viewed-unknown" => "Viewed write uncertain",
        _ => "Loaded",
    }
}

fn samples() -> Vec<SampleItem> {
    [false, true]
        .into_iter()
        .flat_map(|pull_request| {
            (0..30).map(move |ix| {
                let project = if ix % 2 == 0 {
                    "sample-local"
                } else {
                    "sample-other"
                };
                let number = 42 + ix / 2;
                SampleItem {
                    id: format!(
                        "sample-{}-{project}-{number}",
                        if pull_request { "pr" } else { "issue" }
                    ),
                    project: project.into(),
                    number,
                    pull_request,
                    title: if ix < 2 {
                        "Keep chat activities aligned with the shared content gutter".into()
                    } else {
                        format!(
                            "Refine {} component {}",
                            if pull_request { "shared" } else { "workspace" },
                            number
                        )
                    },
                    state: if ix < 20 {
                        State::Open
                    } else if pull_request && ix >= 25 {
                        State::Merged
                    } else {
                        State::Closed
                    },
                    draft: ix % 3 == 0,
                    failed: ix % 4 == 0,
                }
            })
        })
        .collect()
}

fn sample_files() -> Vec<GitHubPrFile> {
    (0..24).map(|ix| GitHubPrFile {
        path: match ix {
            0 => "src/conversation.rs".into(),
            1 => "crates/threadlane-ui-kit/src/components/long-review-path/shared_conversation_details.rs".into(),
            2 => "assets/preview.png".into(),
            3 => "src/café file.rs".into(),
            4 => "src/obsolete.rs".into(),
            _ => format!("src/components/review_{ix}.rs"),
        },
            additions: if ix == 2 || ix == 4 { 0 } else { 4 },
            deletions: match ix { 2 => 0, 4 => 2, _ => 1 },
        change_type: match ix { 3 => "renamed", 4 => "deleted", _ => "modified" }.into(),
    }).collect()
}

fn sample_detail(item: &SampleItem) -> SampleDetail {
    let body = if item.number == 44 {
        String::new()
    } else {
        format!(
            "## Shared content alignment\n\nThis **local sample** describes {}. The same components render on desktop and web.\n\n- Keep the description and actions aligned.\n- Preserve keyboard selection through refresh.\n- Wrap long metadata at narrow widths.\n\n```rust\nlet body = github_issue_body(detail, text, tasks, comments, refreshing, cx);\n```\n\nReview the [GPUI source](https://github.com/zed-industries/zed/tree/main/crates/gpui_web).",
            item.title
        )
    };
    if item.pull_request {
        SampleDetail::PullRequest(GitHubPrInfo {
            number: item.number as u64,
            title: item.title.clone(),
            url: format!("https://github.com/threadlane/sample/pull/{}", item.number),
            state: item.state.value().into(),
            is_draft: item.draft,
            head_ref: "sample/shared-ui".into(),
            base_ref: "main".into(),
            author: "sample-author".into(),
            body,
            review_decision: Some("REVIEW_REQUIRED".into()),
            files: if item.number == 44 {
                vec![]
            } else {
                sample_files()
            },
            total_checks: 3,
            passing_checks: if item.failed { 1 } else { 2 },
            pending_checks: 1,
            failing_checks: usize::from(item.failed),
            checks: vec![
                PrCheckStatus {
                    name: "Build".into(),
                    status: "COMPLETED".into(),
                    conclusion: Some(if item.failed { "FAILURE" } else { "SUCCESS" }.into()),
                    details_url: Some(format!(
                        "https://github.com/threadlane/sample/actions/runs/{}",
                        item.number
                    )),
                },
                PrCheckStatus {
                    name: "Formatting and accessibility checks".into(),
                    status: "IN_PROGRESS".into(),
                    conclusion: None,
                    details_url: None,
                },
                PrCheckStatus {
                    name: "Documentation".into(),
                    status: "COMPLETED".into(),
                    conclusion: Some("SKIPPED".into()),
                    details_url: Some("file:///private/tmp/sample.log".into()),
                },
            ],
            issue_comments: if item.number == 44 {
                vec![]
            } else {
                vec![PrConversationComment {
                remote_id: "sample-discussion".into(), author: "sample-author".into(), created_at: "2h ago".into(),
                body: "### Shared review surfaces\nThe conversation and commit list now render through the same components on native and web.\n\n- Keep draft text when requests fail.\n- Preserve the selected PR while switching tabs.".into(), ..Default::default()
            }]
            },
            reviews: if item.number == 44 {
                vec![]
            } else {
                vec![PrReview {
                    remote_id: "sample-review".into(),
                    author: "sample-reviewer".into(),
                    submitted_at: "1h ago".into(),
                    state: "CHANGES_REQUESTED".into(),
                    ..Default::default()
                }]
            },
            review_comments: if item.number == 44 {
                vec![]
            } else {
                (0..2).map(|ix| PrReviewComment {
                remote_id: format!("sample-inline-{ix}"), author: "sample-reviewer".into(), created_at: "40m ago".into(),
                path: Some("crates/threadlane-ui-kit/src/github_conversation.rs".into()), line: Some(32 + ix),
                body: if ix == 0 { "Keep the **draft editor** aligned with the discussion and let long paths wrap.".into() }
                    else { "Reply text should survive switching to another project’s PR with the same number.".into() }, ..Default::default()
            }).collect()
            },
            commits: if item.number == 44 {
                vec![]
            } else {
                (0..18).map(|ix| GitHubPrCommit {
                oid: format!("{:07x}{}", 0xabcdef0_u64 + ix, "0".repeat(33)),
                message: if ix == 0 { "Share conversation rows and editable drafts across desktop and web, preserving publication guards and keyboard navigation".into() }
                    else { format!("Refine shared review component {ix}") },
                author: "sample-author".into(), committed_at: "2026-10-03T15:30:00Z".into(),
            }).collect()
            },
            ..Default::default()
        })
    } else {
        SampleDetail::Issue(GitHubIssueDetail {
            summary: GitHubIssueSummary {
                issue: threadlane_protocol::daemon::GitHubIssueRef { host: "github.com".into(), owner: "threadlane".into(), repo: "sample".into(),
                    number: item.number as u64, url: format!("https://github.com/threadlane/sample/issues/{}", item.number) },
                title: item.title.clone(), state: item.state.value().into(), author: "sample-author".into(), comments_count: 3,
                labels: ["ui", "sample"].into_iter().map(|name| GitHubLabel { name: name.into(), ..Default::default() }).collect(),
                assignees: vec!["sample-author".into()], ..Default::default()
            }, body,
            comments: vec![
                GitHubIssueComment { remote_id: "sample-comment-1".into(), author: "sample-reviewer".into(), created_at: "1h ago".into(),
                    body: "### Review note\nKeep the **leading edge** aligned with the description and linked tasks.".into(), ..Default::default() },
                GitHubIssueComment { remote_id: "sample-comment-2".into(), author: "sample-author".into(), created_at: "40m ago".into(),
                    body: "The narrow window keeps actions reachable and preserves `sample/shared-ui`.\n\n- Native and WASM share this row.\n- This record stays local.".into(), ..Default::default() },
                GitHubIssueComment { remote_id: "sample-comment-empty".into(), author: "sample-reviewer".into(), created_at: "20m ago".into(), ..Default::default() },
            ],
        })
    }
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
