//! Selectable Markdown and its disclosure control.

use std::sync::Arc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::text::{
    MarkdownNode, MarkdownParseContext, MarkdownPlugin, TextView, TextViewState, TextViewStyle,
    markdown_ast::Node,
};
use gpui_kit::component::{Sizable as _, v_flex};
use gpui_kit::*;
use threadlane_core::ids::DecisionId;
use threadlane_desktop::{
    Activity, ProseLimit, Reference, describe, load_decision_link, markdown_source_with_time,
};

use super::button;
use crate::tokens::{TYPE_BODY, XS};
use crate::{Desktop, Screen, Tab};

// Only the kit's Markdown constructs may create formatted content or actions.
struct LiteralMarkup {
    block: bool,
}

impl MarkdownPlugin for LiteralMarkup {
    fn is_block(&self) -> bool {
        self.block
    }

    fn name(&self) -> &str {
        "threadlane-literal-markup"
    }

    fn parse(&self, node: &Node, context: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        if matches!(
            node,
            Node::Html(_) | Node::Image(_) | Node::ImageReference(_)
        ) {
            Some(MarkdownNode::new(self.name(), ()).text(context.node_source(node)?.to_owned()))
        } else {
            None
        }
    }
}

#[derive(IntoElement)]
pub(crate) struct Prose {
    id: SharedString,
    source: String,
    space: String,
    desktop: WeakEntity<Desktop>,
    limit: ProseLimit,
    expanded: bool,
    result_seq: Option<i64>,
}

impl Desktop {
    pub(crate) fn prose(
        &self,
        id: impl Into<SharedString>,
        space: &str,
        text: String,
        cx: &Context<Self>,
    ) -> Prose {
        let id = id.into();
        let when = self.when();
        Prose {
            id: id.clone(),
            source: markdown_source_with_time(&text, Some(&when)),
            space: space.to_owned(),
            desktop: cx.entity().downgrade(),
            limit: ProseLimit::Full,
            expanded: self.state.message_expanded(&id),
            result_seq: None,
        }
    }

    pub(crate) fn open_prose_link(
        &mut self,
        space: &str,
        reference: Reference<'_>,
        cx: &mut Context<Self>,
    ) {
        match reference {
            Reference::Task(task) => self.go(
                Screen::Task {
                    space: space.to_owned(),
                    task,
                },
                cx,
            ),
            Reference::Decision(id) => self.open_linked_decision(space, id, cx),
            Reference::Web(url) => cx.open_url(url),
        }
    }

    fn open_linked_decision(&mut self, space: &str, id: DecisionId, cx: &mut Context<Self>) {
        if let Some(project) = self.state.reference_project(space, id) {
            self.reveal_decision_link(space.to_owned(), project.to_owned(), id, cx);
            return;
        }
        let mut projects: Vec<_> = self
            .state
            .targets()
            .into_iter()
            .filter_map(|(name, project)| (name == space).then_some(project.to_owned()))
            .collect();
        let current_project = match &self.screen {
            Screen::Project { project, .. } => Some(project.as_str()),
            Screen::Task { space, task } => self
                .state
                .task(space, *task)
                .map(|task| task.summary.project.as_str()),
            _ => None,
        };
        if let Some(index) =
            current_project.and_then(|name| projects.iter().position(|project| project == name))
        {
            projects.swap(0, index);
        }
        self.state
            .report(Activity::Working(format!("Looking for {id}…")));
        let core = Arc::clone(&self.core);
        let space = space.to_owned();
        let start = self.screen.clone();
        self.run(
            async move {
                let result = load_decision_link(&*core.read().await, &space, projects, id).await;
                (space, result)
            },
            move |this, (space, result), cx| match result {
                Ok(Some(link)) => {
                    let project = this.state.apply_decision_link(link);
                    this.state.report(Activity::Done(format!("Found {id}.")));
                    if this.screen == start {
                        this.reveal_decision_link(space, project, id, cx);
                    }
                }
                Ok(None) => this
                    .state
                    .report(Activity::Failed(format!("Could not find {id} in {space}."))),
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    fn reveal_decision_link(
        &mut self,
        space: String,
        project: String,
        id: DecisionId,
        cx: &mut Context<Self>,
    ) {
        let screen = Screen::Project {
            space: space.clone(),
            project,
            tab: Tab::Conversation,
        };
        self.go(screen, cx);
        self.conversation_follow.set(false);
        self.reveal_linked_decision = Some(threadlane_desktop::DecisionKey { space, id });
        self.scroll_to_decision
            .set(crate::keyboard::DecisionScroll::Card);
    }
}

impl Prose {
    pub(crate) fn limit(mut self, limit: ProseLimit) -> Self {
        self.limit = limit;
        self
    }

    pub(crate) fn result(mut self, seq: i64, expanded: bool) -> Self {
        self.limit = ProseLimit::Result;
        self.result_seq = Some(seq);
        self.expanded = expanded;
        self
    }
}

impl RenderOnce for Prose {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, cx| {
            TextViewState::markdown(&self.source, cx)
        });
        state.update(cx, |state, cx| state.set_text(&self.source, cx));
        let (cap, label) = self
            .limit
            .disclosure(self.expanded, state.read(cx).is_clamped());
        let id = self.id.clone();
        let desktop = self.desktop.clone();
        let space = self.space.clone();
        let mut view = TextView::new(&state)
            .style(TextViewStyle::default().heading_font_size(|level, base| {
                base * match level {
                    1 => 1.25,
                    2 => 1.15,
                    3 => 1.075,
                    _ => 1.0,
                }
            }))
            .selectable(true)
            .plugin(LiteralMarkup { block: true })
            .plugin(LiteralMarkup { block: false })
            .code_block_actions(|block, _, _| {
                Clipboard::new("copy-code")
                    .accessibility_label("Copy code")
                    .value(block.code())
            })
            .on_link_click(move |url, event, _, cx| {
                let activate = match event {
                    ClickEvent::Mouse(click) => {
                        matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
                    }
                    ClickEvent::Keyboard(_) => true,
                    ClickEvent::Touch(click) => !click.long_press,
                };
                if activate && let Some(reference) = Reference::from_link(url) {
                    let _gone =
                        desktop.update(cx, |view, cx| view.open_prose_link(&space, reference, cx));
                }
            })
            .w_full();
        if let Some(lines) = cap {
            view = view.max_lines(lines);
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap(XS)
            .text_size(TYPE_BODY)
            .child(view)
            .children(label.map(|label| {
                button("toggle-prose")
                    .ghost()
                    .small()
                    .tab_stop(true)
                    .label(label)
                    .on_click(move |_, _, cx| {
                        let _gone = self.desktop.update(cx, |view, cx| {
                            if let Some(seq) = self.result_seq {
                                view.state.toggle_result(&self.space, seq);
                            } else {
                                view.state.toggle_message(&self.id);
                            }
                            cx.notify();
                        });
                    })
            }))
            .id(id)
    }
}
