use super::state::{ActivityStatus, AppState, CompletionMode, MessageType, RunStatus};
use crate::{
    commands::command_description,
    login::{LoginMode, LoginProvider, LoginState},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use threadlane_agent::PlanItemStatus;

pub fn render(frame: &mut Frame, state: &AppState) {
    let sections = layout_sections(frame.area(), state);
    render_header(frame, state, sections.header);
    render_transcript(frame, state, sections.transcript);
    render_activity(frame, state, sections.activity);
    render_plan(frame, state, sections.plan);
    render_popup(frame, state, sections.popup);
    render_input(frame, state, sections.composer);
    render_footer(frame, sections.footer);
}

#[derive(Debug, PartialEq, Eq)]
pub struct LayoutSections {
    pub header: Rect,
    pub transcript: Rect,
    pub activity: Rect,
    pub plan: Rect,
    pub popup: Rect,
    pub composer: Rect,
    pub footer: Rect,
}

pub fn layout_sections(area: Rect, state: &AppState) -> LayoutSections {
    let has_activity = !state.activities.is_empty();
    let has_plan = state
        .plan
        .as_ref()
        .is_some_and(|plan| !plan.items.is_empty());
    let mut constraints = vec![Constraint::Length(3), Constraint::Min(1)];
    if has_activity {
        constraints.push(Constraint::Length(section_height(
            state.activities.len(),
            5,
        )));
    }
    if has_plan {
        constraints.push(Constraint::Length(section_height(
            state.plan.as_ref().unwrap().items.len(),
            5,
        )));
    }
    let popup_height = popup_height(area, state, has_activity, has_plan);
    if popup_height > 0 {
        constraints.push(Constraint::Length(popup_height));
    }
    constraints.extend([Constraint::Length(3), Constraint::Length(1)]);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(area);
    let header = chunks[0];
    let transcript = chunks[1];
    let mut next = 2;
    let empty = |y| Rect::new(transcript.x, y, transcript.width, 0);
    let activity = if has_activity {
        let rect = chunks[next];
        next += 1;
        rect
    } else {
        empty(transcript.y + transcript.height)
    };
    let plan = if has_plan {
        let rect = chunks[next];
        next += 1;
        rect
    } else {
        empty(activity.y + activity.height)
    };
    let popup = if popup_height > 0 {
        let rect = chunks[next];
        next += 1;
        rect
    } else {
        empty(plan.y + plan.height)
    };
    LayoutSections {
        header,
        transcript,
        activity,
        plan,
        popup,
        composer: chunks[next],
        footer: chunks[next + 1],
    }
}

fn section_height(items: usize, cap: u16) -> u16 {
    items.saturating_add(2).min(cap as usize) as u16
}

fn popup_height(area: Rect, state: &AppState, has_activity: bool, has_plan: bool) -> u16 {
    let reserved = 3
        + 3
        + 1
        + if has_activity {
            section_height(state.activities.len(), 5)
        } else {
            0
        }
        + if has_plan {
            section_height(state.plan.as_ref().unwrap().items.len(), 5)
        } else {
            0
        };
    let available = area.height.saturating_sub(2).saturating_sub(reserved + 1);
    let requested = if let Some(login) = state.login.as_ref() {
        login_popup_height(login)
    } else if state.completion.visible {
        section_height(state.completion.candidates.len().max(1), 8)
    } else {
        0
    };
    let height = requested.min(available);
    (height >= 3).then_some(height).unwrap_or(0)
}

fn login_popup_height(login: &LoginState) -> u16 {
    let rows = match login.mode {
        LoginMode::ProviderPicker => {
            LoginProvider::ALL.len() + usize::from(login.status().is_some())
        }
        LoginMode::OpenAiKey => 1 + usize::from(login.status().is_some()),
    };
    section_height(rows.max(1), 8)
}

fn render_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let status = match state.status {
        RunStatus::Running => (" ● GENERATING ", Color::Yellow),
        RunStatus::Failed => (" FAILED ", Color::Red),
        RunStatus::Cancelled => (" CANCELLED ", Color::Yellow),
        RunStatus::Succeeded => (" DONE ", Color::Green),
        RunStatus::Idle | RunStatus::Ready => (" READY ", Color::Green),
    };
    let title = Line::from(vec![
        Span::styled(
            "Threadlane Agent",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | Model: "),
        Span::styled(&state.model, Style::default().fg(Color::Magenta)),
        Span::raw(" | Workspace: "),
        Span::styled(&state.work_dir, Style::default().fg(Color::Gray)),
        Span::raw(" | "),
        Span::styled(
            status.0,
            Style::default().fg(status.1).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(Style::default().fg(Color::Cyan)),
        area,
    );
}

fn render_transcript(frame: &mut Frame, state: &AppState, area: Rect) {
    let mut lines = Vec::new();
    for msg in &state.messages {
        let (label, color, modifier) = match &msg.msg_type {
            MessageType::User => ("You: ", Color::Blue, Modifier::BOLD),
            MessageType::Assistant => ("Threadlane: ", Color::Green, Modifier::BOLD),
            MessageType::Thinking => ("Thinking: ", Color::DarkGray, Modifier::ITALIC),
            MessageType::ToolCall(_) => ("Tool: ", Color::Yellow, Modifier::empty()),
            MessageType::Error => ("Error: ", Color::Red, Modifier::BOLD),
        };
        let detail = match &msg.msg_type {
            MessageType::ToolCall(name) => format!("{name}: {}", msg.content),
            _ => msg.content.clone(),
        };
        lines.push(Line::from(vec![
            Span::styled(label, Style::default().fg(color).add_modifier(modifier)),
            Span::raw(detail),
        ]));
        lines.push(Line::raw(""));
    }
    if let Some(streaming) = &state.streaming {
        if !streaming.reasoning.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("Thinking: {}", streaming.reasoning),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if !streaming.text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "Threadlane: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&streaming.text),
            ]));
        }
    }
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let total_lines = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(inner_width))
        .sum::<usize>();
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(viewport);
    let scroll = if state.follow_tail {
        max_scroll
    } else {
        max_scroll.saturating_sub(state.scroll as usize)
    } as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Transcript "))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn render_activity(frame: &mut Frame, state: &AppState, area: Rect) {
    if state.activities.is_empty() || area.height == 0 {
        return;
    }
    let lines = state
        .activities
        .iter()
        .map(|item| {
            let (status, color) = match item.status {
                ActivityStatus::Queued => ("queued", Color::DarkGray),
                ActivityStatus::Running => ("running", Color::Yellow),
                ActivityStatus::Succeeded => ("done", Color::Green),
                ActivityStatus::Failed => ("failed", Color::Red),
                ActivityStatus::Cancelled => ("cancelled", Color::Yellow),
            };
            Line::from(vec![
                Span::styled(format!("{status:9}"), Style::default().fg(color)),
                Span::styled(
                    format!("{}: ", item.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(&item.detail),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Activity "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_plan(frame: &mut Frame, state: &AppState, area: Rect) {
    let Some(plan) = &state.plan else {
        return;
    };
    if plan.items.is_empty() || area.height == 0 {
        return;
    }
    let lines = plan
        .items
        .iter()
        .map(|item| {
            let (marker, color) = match item.status {
                PlanItemStatus::Pending => ("[ ]", Color::DarkGray),
                PlanItemStatus::InProgress => ("[>]", Color::Yellow),
                PlanItemStatus::Completed => ("[x]", Color::Green),
            };
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(&item.step),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Plan "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_popup(frame: &mut Frame, state: &AppState, area: Rect) {
    if let Some(login) = state.login.as_ref() {
        render_login(frame, login, area);
    } else {
        render_completion(frame, state, area);
    }
}

fn render_login(frame: &mut Frame, login: &LoginState, area: Rect) {
    if area.height == 0 {
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let selected_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut lines = match login.mode {
        LoginMode::ProviderPicker => LoginProvider::ALL
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                let style = if login.selected_provider() == *provider {
                    selected_style
                } else {
                    Style::default()
                };
                let _ = index;
                Line::from(vec![Span::styled(
                    truncate_plain(provider.label(), inner_width),
                    style,
                )])
            })
            .collect::<Vec<_>>(),
        LoginMode::OpenAiKey => vec![Line::from(vec![Span::styled(
            truncate_plain(LoginProvider::OpenAi.label(), inner_width),
            selected_style,
        )])],
    };

    if let Some(status) = login.status() {
        lines.push(Line::from(vec![Span::styled(
            truncate_plain(&inline_status(status), inner_width),
            login_status_style(status),
        )]));
    }

    let title = match login.mode {
        LoginMode::ProviderPicker => " Login Providers ",
        LoginMode::OpenAiKey => " Login Status ",
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn inline_status(status: &str) -> String {
    status.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn login_status_style(status: &str) -> Style {
    let lower = status.to_ascii_lowercase();
    let color = if lower.contains("saved") || lower.contains("complete") {
        Color::Green
    } else if lower.contains("failed") || lower.contains("cannot") || lower.contains("error") {
        Color::Red
    } else {
        Color::Yellow
    };
    Style::default().fg(color)
}

fn render_completion(frame: &mut Frame, state: &AppState, area: Rect) {
    if area.height == 0 || !state.completion.visible {
        return;
    }

    let title = match state.completion.mode {
        Some(CompletionMode::Command) => " Commands ",
        Some(CompletionMode::Model) => " Models ",
        None => " Completion ",
    };
    let lines = state
        .completion
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            completion_line(
                state,
                index,
                candidate,
                area.width.saturating_sub(2) as usize,
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .scroll((completion_scroll(state, area), 0)),
        area,
    );
}

fn completion_scroll(state: &AppState, area: Rect) -> u16 {
    let viewport = area.height.saturating_sub(2) as usize;
    if viewport == 0 {
        return 0;
    }
    let max_scroll = state.completion.candidates.len().saturating_sub(viewport);
    state
        .completion
        .selected
        .saturating_sub(viewport.saturating_sub(1))
        .min(max_scroll) as u16
}

fn completion_line(
    state: &AppState,
    index: usize,
    candidate: &str,
    inner_width: usize,
) -> Line<'static> {
    let selected = state.completion.selected == index;
    let selected_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    match state.completion.mode {
        Some(CompletionMode::Command) => {
            let detail = command_description(candidate);
            let mut spans = vec![styled_text(
                candidate,
                if selected {
                    selected_style
                } else {
                    Style::default()
                },
            )];
            if !detail.is_empty() {
                spans.push(styled_text(" ", Style::default()));
                spans.push(styled_text(
                    detail,
                    if selected {
                        selected_style
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ));
            }
            Line::from(truncate_spans(spans, inner_width))
        }
        _ => Line::from(vec![Span::styled(
            truncate_plain(candidate, inner_width),
            if selected {
                selected_style
            } else {
                Style::default()
            },
        )]),
    }
}

fn styled_text(text: &str, style: Style) -> (String, Style) {
    (text.to_string(), style)
}

fn truncate_spans(spans: Vec<(String, Style)>, max_width: usize) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }

    let total_width = spans
        .iter()
        .map(|(text, _)| text_width(text))
        .sum::<usize>();
    if total_width <= max_width {
        return spans
            .into_iter()
            .map(|(text, style)| Span::styled(text, style))
            .collect();
    }

    let mut remaining = max_width.saturating_sub(1);
    let mut truncated = Vec::new();
    let mut last_style = Style::default();

    for (text, style) in spans {
        if remaining == 0 {
            break;
        }
        let mut piece = String::new();
        let mut width = 0;
        for ch in text.chars() {
            let ch_width = text_width(&ch.to_string());
            if width + ch_width > remaining {
                break;
            }
            piece.push(ch);
            width += ch_width;
        }
        if width == 0 {
            continue;
        }
        remaining = remaining.saturating_sub(width);
        last_style = style;
        truncated.push(Span::styled(piece, style));
    }

    truncated.push(Span::styled("…".to_string(), last_style));
    truncated
}

fn truncate_plain(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text_width(text) <= max_width {
        return text.to_string();
    }
    let mut head = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = text_width(&ch.to_string());
        if width + ch_width > max_width.saturating_sub(1) {
            break;
        }
        head.push(ch);
        width += ch_width;
    }
    format!("{head}…")
}

fn text_width(text: &str) -> usize {
    Span::raw(text.to_string()).width()
}

fn render_input(frame: &mut Frame, state: &AppState, area: Rect) {
    let (title, text, color) = if let Some(login) = state.login.as_ref() {
        (
            match login.mode {
                LoginMode::ProviderPicker => " Login ",
                LoginMode::OpenAiKey => " OpenAI API Key ",
            },
            match login.mode {
                LoginMode::ProviderPicker => "",
                LoginMode::OpenAiKey => login.masked_key(),
            },
            if login.pending {
                Color::DarkGray
            } else {
                Color::Yellow
            },
        )
    } else if matches!(state.status, RunStatus::Running) {
        (" Prompt ", state.composer.as_str(), Color::DarkGray)
    } else {
        (" Prompt ", state.composer.as_str(), Color::Yellow)
    };
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .style(Style::default().fg(color)),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "[Enter]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Submit Prompt  "),
            Span::styled(
                "[Esc / Ctrl+C]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Quit  "),
            Span::styled(
                "[Up/Down]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Scroll Transcript"),
        ])),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::super::state::CompletionMode;
    use super::*;
    use crate::login::LoginProvider;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn empty_activity_and_plan_do_not_create_empty_sections() {
        let state = AppState::test_state();
        let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
        assert_eq!(sections.activity.height, 0);
        assert_eq!(sections.plan.height, 0);
        assert_eq!(sections.popup.height, 0);
        assert!(sections.transcript.height > 0);
        assert_eq!(sections.composer.height, 3);

        let mut empty_plan = AppState::test_state();
        empty_plan.plan = Some(threadlane_agent::SessionPlan {
            explanation: None,
            items: vec![],
        });
        let empty_plan_sections = layout_sections(Rect::new(0, 0, 100, 30), &empty_plan);
        assert_eq!(empty_plan_sections.activity.height, 0);
        assert_eq!(empty_plan_sections.plan.height, 0);
        assert_eq!(empty_plan_sections.popup.height, 0);
        assert_eq!(empty_plan_sections.transcript, sections.transcript);
        assert_eq!(empty_plan_sections.composer, sections.composer);
    }

    #[test]
    fn active_plan_and_activity_get_bounded_height() {
        let mut state = AppState::test_state_with_plan(20);
        state.activities = (0..20)
            .map(|index| super::super::state::ActivityItem {
                id: index.to_string(),
                name: "tool".into(),
                detail: "detail".into(),
                status: super::super::state::ActivityStatus::Running,
            })
            .collect();
        let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
        assert!(sections.transcript.height >= 1);
        assert!(sections.plan.height < 30);
        assert!(sections.activity.height < 30);
    }

    #[test]
    fn completion_popup_is_bounded_and_sits_above_the_prompt() {
        let mut state = AppState::test_state_with_plan(20);
        state.activities = (0..20)
            .map(|index| super::super::state::ActivityItem {
                id: index.to_string(),
                name: "tool".into(),
                detail: "detail".into(),
                status: super::super::state::ActivityStatus::Running,
            })
            .collect();
        state.show_completion(
            CompletionMode::Command,
            (0..20).map(|index| format!("/cmd-{index}")).collect(),
        );

        let sections = layout_sections(Rect::new(0, 0, 100, 24), &state);

        assert!(sections.popup.height > 0);
        assert!(sections.popup.height <= 8);
        assert_eq!(
            sections.popup.y + sections.popup.height,
            sections.composer.y
        );
        assert_eq!(sections.composer.height, 3);
        assert_eq!(sections.footer.height, 1);
    }

    #[test]
    fn tiny_terminal_omits_popup_without_an_inner_viewport() {
        let mut state = AppState::test_state();
        state.show_completion(CompletionMode::Model, vec!["gpt-4o".into()]);

        let sections = layout_sections(Rect::new(0, 0, 80, 12), &state);

        assert_eq!(sections.popup.height, 0);
    }

    #[test]
    fn render_shows_command_descriptions_and_selected_yellow_row() {
        let mut state = AppState::test_state();
        state.show_completion(
            CompletionMode::Command,
            vec!["/model".into(), "/help".into()],
        );
        state.completion.selected = 1;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }

        assert!(text.contains("Commands"));
        assert!(text.contains("/model switch model"));
        assert!(text.contains("/help show help"));

        let (selected_x, selected_y) = find_text(buffer, "/help").unwrap();
        assert_eq!(buffer[(selected_x, selected_y)].fg, Color::Yellow);
    }

    #[test]
    fn render_keeps_selected_model_visible_in_capped_popup() {
        let mut state = AppState::test_state();
        state.show_completion(
            CompletionMode::Model,
            (0..12).map(|index| format!("model-{index}")).collect(),
        );
        state.completion.selected = 11;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(find_text(buffer, "model-11").is_some());
        assert!(find_text(buffer, "model-0").is_none());

        let (selected_x, selected_y) = find_text(buffer, "model-11").unwrap();
        assert_eq!(buffer[(selected_x, selected_y)].fg, Color::Yellow);
    }

    #[test]
    fn render_keeps_selected_command_visible_on_narrow_terminal_without_wrap_drift() {
        let mut state = AppState::test_state();
        state.show_completion(
            CompletionMode::Command,
            vec![
                "/reasoning".into(),
                "/session".into(),
                "/model".into(),
                "/models".into(),
                "/reasoning".into(),
                "/session".into(),
                "/model".into(),
                "/quit".into(),
            ],
        );
        state.completion.selected = 7;

        let backend = TestBackend::new(22, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(find_text(buffer, "/quit").is_some());

        let (selected_x, selected_y) = find_text(buffer, "/quit").unwrap();
        assert_eq!(buffer[(selected_x, selected_y)].fg, Color::Yellow);
    }

    #[test]
    fn render_shows_login_provider_picker_with_existing_popup_geometry() {
        let mut state = AppState::test_state_with_plan(20);
        state.activities = (0..20)
            .map(|index| super::super::state::ActivityItem {
                id: index.to_string(),
                name: "tool".into(),
                detail: "detail".into(),
                status: super::super::state::ActivityStatus::Running,
            })
            .collect();
        state.open_login();
        state.login.as_mut().unwrap().select_next_provider();

        let sections = layout_sections(Rect::new(0, 0, 100, 30), &state);
        assert!(sections.popup.height > 0);
        assert_eq!(
            sections.popup.y + sections.popup.height,
            sections.composer.y
        );

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(find_text(buffer, "Codex").is_some());
        assert!(find_text(buffer, "OpenAI").is_some());
        assert!(find_text(buffer, "Antigravity").is_some());

        let (selected_x, selected_y) = find_text(buffer, "OpenAI").unwrap();
        assert_eq!(buffer[(selected_x, selected_y)].fg, Color::Yellow);
    }

    #[test]
    fn render_masks_openai_key_entry_and_hides_raw_key() {
        let mut state = AppState::test_state();
        state.open_login();
        let masked = {
            let login = state.login.as_mut().unwrap();
            login.select_provider(LoginProvider::OpenAi);
            login.push_paste("sk-secret-123");
            login.masked_key().to_string()
        };

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(find_text(buffer, "OpenAI API Key").is_some());
        assert!(find_text(buffer, &masked).is_some());
        assert!(find_text(buffer, "sk-secret-123").is_none());
    }

    #[test]
    fn render_shows_bounded_secret_free_login_status() {
        let mut state = AppState::test_state();
        state.open_login();
        let login = state.login.as_mut().unwrap();
        login.select_provider(LoginProvider::OpenAi);
        login.push_paste("sk-secret-123");
        login.set_status("OpenAI API key cannot be empty. Paste a key and try again.");

        let backend = TestBackend::new(28, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(buffer, y))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("OpenAI API key"));
        assert!(rendered.contains('…'));
        assert!(!rendered.contains("sk-secret-123"));
    }

    #[test]
    fn render_truncates_long_command_rows_on_narrow_terminal_without_wrapping() {
        let mut state = AppState::test_state();
        state.show_completion(
            CompletionMode::Command,
            vec!["/reasoning".into(), "/help".into()],
        );
        state.completion.selected = 0;

        let backend = TestBackend::new(18, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();

        let buffer = terminal.backend().buffer();
        let sections = layout_sections(buffer.area, &state);
        let popup_inner_width = sections.popup.width.saturating_sub(2) as usize;
        let popup_row = row_text(buffer, sections.popup.y + 1);

        assert!(sections.popup.height <= 8);
        assert_eq!(popup_row.chars().count(), buffer.area.width as usize);
        assert!(popup_row.contains('…'));
        assert!(popup_row.contains("/reasoning"));
        assert!(!popup_row.contains("set reasoning"));
        assert!(find_text(buffer, "/help").is_some());
        assert!(popup_inner_width < "/reasoning set reasoning".chars().count());
    }

    #[test]
    fn command_row_truncation_uses_terminal_display_width() {
        let mut state = AppState::test_state();
        state.show_completion(CompletionMode::Command, vec!["/模型abc".into()]);

        let line = completion_line(&state, 0, "/模型abc", 6);

        assert!(line.width() <= 6);
        assert_eq!(line.spans.last().unwrap().content, "…");
    }

    #[test]
    fn follow_tail_tracks_manual_scroll_back_to_end() {
        let mut state = AppState::test_state();
        assert!(state.follow_tail);
        state.scroll_up();
        assert!(!state.follow_tail);
        assert_eq!(state.scroll, 1);
        state.scroll_down();
        assert!(state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    fn find_text(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buffer.area.height {
            let row = row_text(buffer, y);
            if let Some(offset) = row.find(needle) {
                return Some((offset as u16, y));
            }
        }
        None
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
    }
}
