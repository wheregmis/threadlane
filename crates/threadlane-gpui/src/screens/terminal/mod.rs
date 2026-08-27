use std::future::Future;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::ThemeMode;
use gpui_component::{ActiveTheme, ElementExt, Icon, IconName, Sizable};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

const DEFAULT_ROWS: u16 = 30;
const DEFAULT_COLS: u16 = 120;
const SCROLLBACK_ROWS: usize = 10_000;
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const TERMINAL_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const TERMINAL_FLOOD_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const TERMINAL_READ_CHUNK_BYTES: usize = 8192;
const TERMINAL_OUTPUT_BUFFERED_CHUNKS: usize = 8;
const TERMINAL_PARSE_BUDGET_PER_FRAME: usize = TERMINAL_READ_CHUNK_BYTES * 2;

fn terminal_frame_policy(saturated: bool) -> (Duration, usize) {
    if saturated {
        (
            TERMINAL_FLOOD_FRAME_INTERVAL,
            TERMINAL_PARSE_BUDGET_PER_FRAME * 2,
        )
    } else {
        (TERMINAL_FRAME_INTERVAL, TERMINAL_PARSE_BUDGET_PER_FRAME)
    }
}

fn terminal_parse_budget_exhausted(parsed_bytes: usize, parse_budget: usize) -> bool {
    parsed_bytes >= parse_budget
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            if let Err(error) = writer.write_all(bytes).and_then(|_| writer.flush()) {
                tracing::warn!("failed to write to terminal PTY: {error}");
            }
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        if let Err(error) = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            tracing::warn!("failed to resize terminal PTY: {error}");
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

enum PtyEvent {
    Frame(TerminalFrame),
    Closed,
    Error(String),
}

enum TerminalWake {
    Events(Vec<PtyEvent>),
    Blink,
    Disconnected,
}

async fn next_terminal_wake(
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    blink: impl Future<Output = ()>,
) -> TerminalWake {
    tokio::select! {
        biased;
        event = event_rx.recv() => {
            let Some(event) = event else {
                return TerminalWake::Disconnected;
            };
            let mut events = Vec::new();
            let mut pending_frame = None;
            let mut next = Some(event);
            while let Some(event) = next {
                match event {
                    PtyEvent::Frame(frame) => pending_frame = Some(frame),
                    event => {
                        if let Some(frame) = pending_frame.take() {
                            events.push(PtyEvent::Frame(frame));
                        }
                        events.push(event);
                    }
                }
                next = event_rx.try_recv().ok();
            }
            if let Some(frame) = pending_frame {
                events.push(PtyEvent::Frame(frame));
            }
            TerminalWake::Events(events)
        }
        _ = blink => TerminalWake::Blink,
    }
}

enum ParserCommand {
    Clear,
    Resize(u16, u16),
    SetScrollback(usize),
}

struct TerminalFrame {
    screen: vt100::Screen,
    scrollback: usize,
}

fn visible_terminal_frame(parser: &vt100::Parser, rows: u16, cols: u16) -> TerminalFrame {
    let mut visible = vt100::Parser::new(rows, cols, 0);
    visible.process(&parser.screen().state_formatted());
    TerminalFrame {
        screen: visible.screen().clone(),
        scrollback: parser.screen().scrollback(),
    }
}

fn start_parser_worker(
    rows: u16,
    cols: u16,
    event_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
) -> std::io::Result<(mpsc::SyncSender<Vec<u8>>, mpsc::Sender<ParserCommand>)> {
    let (output_tx, output_rx) = mpsc::sync_channel::<Vec<u8>>(TERMINAL_OUTPUT_BUFFERED_CHUNKS);
    let (command_tx, command_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("threadlane-gpui-terminal-parser".into())
        .spawn(move || {
            let mut rows = rows;
            let mut cols = cols;
            let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
            let mut dirty = false;
            let mut parsed_bytes = 0;
            let (mut frame_interval, mut parse_budget) = terminal_frame_policy(false);
            let mut saturated = false;
            let mut next_frame = Instant::now() + frame_interval;

            loop {
                while let Ok(command) = command_rx.try_recv() {
                    match command {
                        ParserCommand::Clear => {
                            parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
                        }
                        ParserCommand::Resize(new_rows, new_cols) => {
                            rows = new_rows.max(1);
                            cols = new_cols.max(1);
                            parser.screen_mut().set_size(rows, cols);
                            let offset = parser.screen().scrollback().min(rows as usize);
                            parser.screen_mut().set_scrollback(offset);
                        }
                        ParserCommand::SetScrollback(offset) => {
                            parser
                                .screen_mut()
                                .set_scrollback(offset.min(rows as usize));
                        }
                    }
                    dirty = true;
                }

                let now = Instant::now();
                if now >= next_frame {
                    if dirty {
                        if event_tx
                            .send(PtyEvent::Frame(visible_terminal_frame(&parser, rows, cols)))
                            .is_err()
                        {
                            break;
                        }
                        dirty = false;
                    }
                    (frame_interval, parse_budget) = terminal_frame_policy(saturated);
                    parsed_bytes = 0;
                    saturated = false;
                    next_frame = now + frame_interval;
                }

                if terminal_parse_budget_exhausted(parsed_bytes, parse_budget) {
                    std::thread::sleep(next_frame.saturating_duration_since(Instant::now()));
                    continue;
                }

                match output_rx.recv_timeout(next_frame.saturating_duration_since(Instant::now())) {
                    Ok(bytes) => {
                        parsed_bytes = parsed_bytes.saturating_add(bytes.len());
                        saturated = terminal_parse_budget_exhausted(parsed_bytes, parse_budget);
                        parser.process(&bytes);
                        let offset = parser.screen().scrollback().min(rows as usize);
                        parser.screen_mut().set_scrollback(offset);
                        dirty = true;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if dirty {
                            let _ = event_tx
                                .send(PtyEvent::Frame(visible_terminal_frame(&parser, rows, cols)));
                        }
                        break;
                    }
                }
            }
        })?;
    Ok((output_tx, command_tx))
}

fn selection_bounds(
    anchor: (u16, u16),
    head: (u16, u16),
    cols: u16,
) -> Option<((u16, u16), (u16, u16))> {
    if anchor == head {
        return None;
    }
    if head < anchor {
        Some((
            head,
            (
                anchor.0,
                anchor.1.saturating_add(1).min(cols.saturating_sub(1)),
            ),
        ))
    } else {
        Some((anchor, head))
    }
}

fn should_paint_cursor(is_focused: bool, terminal_hides_cursor: bool, blink_visible: bool) -> bool {
    is_focused && !terminal_hides_cursor && blink_visible
}

/// A persistent, focusable project shell backed by a real pseudo-terminal.
///
/// Construct it with `cx.new(|cx| TerminalView::new(project, cx))` and
/// render the resulting `Entity<TerminalView>` directly from its parent view.
pub struct TerminalView {
    project: PathBuf,
    focus_handle: FocusHandle,
    screen: vt100::Screen,
    parser_command_tx: Option<mpsc::Sender<ParserCommand>>,
    session: Option<PtySession>,
    event_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    status: Option<String>,
    rows: u16,
    cols: u16,
    screen_bounds: Option<Bounds<Pixels>>,
    selection_anchor: Option<(u16, u16)>,
    selection_head: Option<(u16, u16)>,
    cell_width: f32,
    cursor_visible: bool,
    scrollback_offset: usize,
    scroll_accumulator: f32,
}

impl TerminalView {
    pub(crate) fn new(project: PathBuf, cx: &mut Context<Self>) -> Self {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        cx.spawn(async move |this, cx| loop {
            let wake = next_terminal_wake(
                &mut event_rx,
                cx.background_executor().timer(CURSOR_BLINK_INTERVAL),
            )
            .await;
            let (events, blink) = match wake {
                TerminalWake::Events(events) => (events, false),
                TerminalWake::Blink => (Vec::new(), true),
                TerminalWake::Disconnected => break,
            };
            if this
                .update(cx, |this, cx| {
                    for event in events {
                        this.apply_event(event);
                    }
                    if blink {
                        this.cursor_visible = !this.cursor_visible;
                    } else {
                        this.cursor_visible = true;
                    }
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();

        let mut terminal = Self {
            project,
            focus_handle: cx.focus_handle(),
            screen: vt100::Parser::new(DEFAULT_ROWS, DEFAULT_COLS, 0)
                .screen()
                .clone(),
            parser_command_tx: None,
            session: None,
            event_tx,
            status: None,
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            screen_bounds: None,
            selection_anchor: None,
            selection_head: None,
            cell_width: 7.8,
            cursor_visible: true,
            scrollback_offset: 0,
            scroll_accumulator: 0.0,
        };
        terminal.start();
        terminal
    }

    /// Sends raw input bytes into the terminal PTY.
    pub fn send_input(&self, input: &str) {
        if let Some(session) = &self.session {
            session.write(input.as_bytes());
        }
    }

    /// Switches the shell to another project, restarting it in the new cwd.
    pub fn set_project(&mut self, project: PathBuf, cx: &mut Context<Self>) {
        if self.project != project {
            self.project = project;
            self.restart(cx);
        }
    }

    /// Terminates the current shell and starts a fresh login-capable interactive shell.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.session.take();
        self.parser_command_tx = None;
        self.screen = vt100::Parser::new(self.rows, self.cols, 0).screen().clone();
        self.scrollback_offset = 0;
        self.scroll_accumulator = 0.0;
        self.status = None;
        self.start();
        cx.notify();
    }

    /// Clears both the emulator scrollback and the visible screen.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.screen = vt100::Parser::new(self.rows, self.cols, 0).screen().clone();
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::Clear);
        }
        self.scrollback_offset = 0;
        self.scroll_accumulator = 0.0;
        self.status = None;
        cx.notify();
    }

    /// Scrolls the terminal view by a number of lines (positive = into scrollback history, negative = towards bottom).
    pub fn scroll_by(&mut self, lines: f32, cx: &mut Context<Self>) {
        self.scroll_accumulator += lines;
        let whole_lines = self.scroll_accumulator.trunc() as isize;
        if whole_lines != 0 {
            let previous_offset = self.scrollback_offset;
            self.scroll_accumulator -= whole_lines as f32;
            let current = self.scrollback_offset as isize;
            let new_offset = (current + whole_lines).max(0) as usize;
            self.set_scrollback(new_offset);
            if self.scrollback_offset != previous_offset {
                self.clear_selection();
            }
            cx.notify();
        }
    }

    /// Resets scrollback to the bottom (live / auto-scroll mode).
    pub fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.scrollback_offset != 0 {
            self.clear_selection();
        }
        self.scrollback_offset = 0;
        self.scroll_accumulator = 0.0;
        self.set_scrollback(0);
        cx.notify();
    }

    /// Scrolls all the way to the top of available scrollback history.
    pub fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        let previous_offset = self.scrollback_offset;
        self.scroll_accumulator = 0.0;
        self.set_scrollback(SCROLLBACK_ROWS);
        if self.scrollback_offset != previous_offset {
            self.clear_selection();
        }
        cx.notify();
    }

    /// Updates the PTY and terminal parser dimensions. Parents can call this when
    /// they have measured cell dimensions for their allocated terminal bounds.
    pub fn resize(&mut self, rows: u16, cols: u16, cx: &mut Context<Self>) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if (rows, cols) == (self.rows, self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.clear_selection();
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::Resize(rows, cols));
        }
        if let Some(session) = &self.session {
            session.resize(rows, cols);
        }
        cx.notify();
    }

    pub fn project(&self) -> &PathBuf {
        &self.project
    }

    fn start(&mut self) {
        let result = start_parser_worker(self.rows, self.cols, self.event_tx.clone())
            .map_err(anyhow::Error::from)
            .and_then(|(output_tx, command_tx)| {
                spawn_shell(
                    &self.project,
                    self.rows,
                    self.cols,
                    output_tx,
                    self.event_tx.clone(),
                )
                .map(|session| (session, command_tx))
            });
        match result {
            Ok((session, command_tx)) => {
                self.session = Some(session);
                self.parser_command_tx = Some(command_tx);
            }
            Err(error) => {
                self.session = None;
                self.parser_command_tx = None;
                self.status = Some(format!("Unable to start terminal: {error}"));
            }
        }
    }

    fn apply_event(&mut self, event: PtyEvent) {
        match event {
            PtyEvent::Frame(frame) => {
                self.screen = frame.screen;
                self.scrollback_offset = frame.scrollback;
            }
            PtyEvent::Closed => {
                if self.session.is_some() {
                    self.status = Some("Shell exited. Select Restart to open a new shell.".into());
                }
            }
            PtyEvent::Error(error) => self.status = Some(format!("Terminal read failed: {error}")),
        }
    }

    // vt100's visible_rows subtracts the offset from the live row count, so
    // keep it within one screen even when scrollback history is much larger.
    fn set_scrollback(&mut self, offset: usize) {
        self.scrollback_offset = offset.min(self.rows as usize);
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::SetScrollback(self.scrollback_offset));
        }
    }

    fn send(&self, bytes: &[u8]) {
        if let Some(session) = &self.session {
            session.write(bytes);
        }
    }

    fn paste(&self, text: String) {
        if self.screen.bracketed_paste() {
            self.send(b"\x1b[200~");
            self.send(text.as_bytes());
            self.send(b"\x1b[201~");
        } else {
            self.send(text.as_bytes());
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.shift {
            match key {
                "pageup" => {
                    self.scroll_by((self.rows / 2).max(1) as f32, cx);
                    cx.stop_propagation();
                    return;
                }
                "pagedown" => {
                    self.scroll_by(-((self.rows / 2).max(1) as f32), cx);
                    cx.stop_propagation();
                    return;
                }
                "home" => {
                    self.scroll_to_top(cx);
                    cx.stop_propagation();
                    return;
                }
                "end" => {
                    self.scroll_to_bottom(cx);
                    cx.stop_propagation();
                    return;
                }
                "up" => {
                    self.scroll_by(1.0, cx);
                    cx.stop_propagation();
                    return;
                }
                "down" => {
                    self.scroll_by(-1.0, cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        if modifiers.platform && key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.paste(text);
            }
            cx.stop_propagation();
            return;
        }

        // Platform copy shortcut: copy the selection when one exists, otherwise
        // fall through so the keystroke still sends ^C to the shell.
        if modifiers.platform && key.eq_ignore_ascii_case("c") {
            if let Some(text) = self.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
                return;
            }
            self.send(&[0x03]);
            cx.stop_propagation();
            return;
        }

        let bytes: Option<Vec<u8>> = if modifiers.control {
            match key.to_ascii_lowercase().as_str() {
                "v" if !modifiers.platform => cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .map(|text| {
                        if self.screen.bracketed_paste() {
                            [
                                b"\x1b[200~".as_slice(),
                                text.as_bytes(),
                                b"\x1b[201~".as_slice(),
                            ]
                            .concat()
                        } else {
                            text.into_bytes()
                        }
                    }),
                letter if letter.len() == 1 && letter.as_bytes()[0].is_ascii_lowercase() => {
                    // Convert Ctrl+<letter> to the standard terminal control character.
                    Some(vec![letter.as_bytes()[0] - b'a' + 1])
                }
                _ => None,
            }
        } else {
            match key {
                "enter" => Some(b"\r".to_vec()),
                "backspace" => Some(vec![0x7f]),
                "tab" => Some(b"\t".to_vec()),
                "up" => Some(self.cursor_key(b'A', modifiers)),
                "down" => Some(self.cursor_key(b'B', modifiers)),
                "right" => Some(self.cursor_key(b'C', modifiers)),
                "left" => Some(self.cursor_key(b'D', modifiers)),
                "home" => Some(b"\x1b[H".to_vec()),
                "end" => Some(b"\x1b[F".to_vec()),
                "delete" => Some(b"\x1b[3~".to_vec()),
                "pageup" => Some(b"\x1b[5~".to_vec()),
                "pagedown" => Some(b"\x1b[6~".to_vec()),
                "f1" => Some(b"\x1bOP".to_vec()),
                "f2" => Some(b"\x1bOQ".to_vec()),
                "f3" => Some(b"\x1bOR".to_vec()),
                "f4" => Some(b"\x1bOS".to_vec()),
                "f5" => Some(b"\x1b[15~".to_vec()),
                "f6" => Some(b"\x1b[17~".to_vec()),
                "f7" => Some(b"\x1b[18~".to_vec()),
                "f8" => Some(b"\x1b[19~".to_vec()),
                "f9" => Some(b"\x1b[20~".to_vec()),
                "f10" => Some(b"\x1b[21~".to_vec()),
                "f11" => Some(b"\x1b[23~".to_vec()),
                "f12" => Some(b"\x1b[24~".to_vec()),
                "escape" => Some(vec![0x1b]),
                _ if !modifiers.platform => event.keystroke.key_char.as_ref().map(|text| {
                    let mut bytes = Vec::with_capacity(text.len() + usize::from(modifiers.alt));
                    if modifiers.alt {
                        bytes.push(0x1b);
                    }
                    bytes.extend_from_slice(text.as_bytes());
                    bytes
                }),
                _ => None,
            }
        };

        if let Some(bytes) = bytes {
            self.send(&bytes);
            cx.stop_propagation();
        }
    }

    fn cursor_key(&self, key: u8, modifiers: Modifiers) -> Vec<u8> {
        let modifier = 1
            + u8::from(modifiers.shift)
            + 2 * u8::from(modifiers.alt)
            + 4 * u8::from(modifiers.control);
        if modifier == 1 {
            if self.screen.application_cursor() {
                vec![0x1b, b'O', key]
            } else {
                vec![0x1b, b'[', key]
            }
        } else {
            format!("\x1b[1;{modifier}{}", key as char).into_bytes()
        }
    }

    fn screen_text(&self) -> String {
        let mut text = self.screen.contents();
        if let Some(status) = &self.status {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(status);
        }
        text
    }

    fn cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let bounds = self.screen_bounds?;
        let x = ((position.x - bounds.left()).as_f32() - 12.0) / self.cell_width;
        let y = ((position.y - bounds.top()).as_f32() - 12.0) / 17.55;
        Some((
            y.floor()
                .max(0.0)
                .min(f32::from(self.rows.saturating_sub(1))) as u16,
            x.floor()
                .max(0.0)
                .min(f32::from(self.cols.saturating_sub(1))) as u16,
        ))
    }

    fn selected_text(&self) -> Option<String> {
        let (anchor, head) = (self.selection_anchor?, self.selection_head?);
        let (start, end) = selection_bounds(anchor, head, self.cols)?;
        Some(self.screen.contents_between(start.0, start.1, end.0, end.1))
    }

    fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_head = None;
    }

    fn begin_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection_anchor = self.cell_at(event.position);
        self.selection_head = self.selection_anchor;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn extend_selection(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.dragging() && self.selection_anchor.is_some() {
            self.selection_head = self.cell_at(event.position);
            cx.notify();
        }
    }

    fn end_selection(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection_head = self.cell_at(event.position).or(self.selection_head);
        cx.notify();
    }
    fn is_cell_selected(&self, row: u16, col: u16) -> bool {
        let (Some(anchor), Some(head)) = (self.selection_anchor, self.selection_head) else {
            return false;
        };
        let Some((start, end)) = selection_bounds(anchor, head, self.cols) else {
            return false;
        };
        let pos = (row, col);
        pos >= start && pos <= end
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection_anchor = Some((0, 0));
        self.selection_head = Some((self.rows.saturating_sub(1), self.cols.saturating_sub(1)));
        cx.notify();
    }

    pub fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste(text);
        }
    }
}

fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-4 {
        return hsla(0.0, 0.0, l, 1.0);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < 1e-4 {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() < 1e-4 {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    hsla(h, s, l, 1.0)
}

fn ansi_index_to_hsla(idx: u8, is_light_theme: bool) -> Hsla {
    // The base palette is tuned for dark backgrounds. On light themes the
    // achromatic entries are inverted and bright chromatic colors are darkened
    // so they stay legible against a light terminal background.
    let adjust = |color: Hsla| -> Hsla {
        if is_light_theme {
            hsla(color.h, color.s, (color.l - 0.18).max(0.0), color.a)
        } else {
            color
        }
    };
    match idx {
        // Standard 16 ANSI colors
        0 => hsla(0.0, 0.0, if is_light_theme { 0.95 } else { 0.15 }, 1.0), // Black
        1 => adjust(hsla(0.0, 0.75, 0.60, 1.0)),                            // Red
        2 => adjust(hsla(0.35, 0.65, 0.55, 1.0)),                           // Green
        3 => adjust(hsla(0.12, 0.80, 0.60, 1.0)),                           // Yellow
        4 => adjust(hsla(0.60, 0.75, 0.65, 1.0)),                           // Blue
        5 => adjust(hsla(0.82, 0.65, 0.65, 1.0)),                           // Magenta
        6 => adjust(hsla(0.50, 0.75, 0.60, 1.0)),                           // Cyan
        7 => hsla(0.0, 0.0, if is_light_theme { 0.25 } else { 0.85 }, 1.0), // White (Dim)
        8 => hsla(0.0, 0.0, if is_light_theme { 0.55 } else { 0.45 }, 1.0), // Bright Black (Gray)
        9 => adjust(hsla(0.0, 0.85, 0.70, 1.0)),                            // Bright Red
        10 => adjust(hsla(0.35, 0.75, 0.65, 1.0)),                          // Bright Green
        11 => adjust(hsla(0.12, 0.90, 0.70, 1.0)),                          // Bright Yellow
        12 => adjust(hsla(0.60, 0.85, 0.75, 1.0)),                          // Bright Blue
        13 => adjust(hsla(0.82, 0.75, 0.75, 1.0)),                          // Bright Magenta
        14 => adjust(hsla(0.50, 0.85, 0.70, 1.0)),                          // Bright Cyan
        15 => hsla(0.0, 0.0, if is_light_theme { 0.05 } else { 0.98 }, 1.0), // Bright White
        // 216 Color cube: 16..=231
        16..=231 => {
            let n = idx - 16;
            let levels = [0, 95, 135, 175, 215, 255];
            let b = levels[(n % 6) as usize];
            let g = levels[((n / 6) % 6) as usize];
            let r = levels[(n / 36) as usize];
            rgb_to_hsla(r, g, b)
        }
        // 24 Grayscale ramp: 232..=255
        232..=255 => {
            let gray = (idx - 232) as f32 / 23.0 * 0.9 + 0.05;
            hsla(0.0, 0.0, gray, 1.0)
        }
    }
}

fn ansi_to_hsla(color: vt100::Color, default_fg: Hsla, is_light_theme: bool) -> Option<Hsla> {
    match color {
        vt100::Color::Default => Some(default_fg),
        vt100::Color::Idx(idx) => Some(ansi_index_to_hsla(idx, is_light_theme)),
        vt100::Color::Rgb(r, g, b) => Some(rgb_to_hsla(r, g, b)),
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let is_light_theme = cx.theme().mode == ThemeMode::Light;
        let terminal_resize = cx.entity().clone();
        let terminal_actions = cx.entity().clone();
        let is_focused = self.focus_handle.is_focused(window);
        let font_id = window.text_system().resolve_font(&font(".ZedMono"));
        let measured_cell_width = window
            .text_system()
            .layout_width(font_id, px(13.0), '0')
            .as_f32();
        if measured_cell_width > 0.0 {
            self.cell_width = measured_cell_width;
        }
        let cell_width = self.cell_width;

        let screen = &self.screen;
        let (cursor_row, cursor_col) = screen.cursor_position();
        let hide_cursor = screen.hide_cursor();

        let mut screen_lines = Vec::with_capacity(self.rows as usize);
        for row in 0..self.rows {
            let mut row_spans = Vec::new();
            let mut current_span_text = String::new();
            let mut current_style: Option<(Option<Hsla>, Option<Hsla>, bool, bool)> = None;

            // Find the rightmost non-empty column or cursor column
            let mut max_col = 0;
            for col in (0..self.cols).rev() {
                if let Some(cell) = screen.cell(row, col) {
                    let contents = cell.contents();
                    if (!contents.is_empty() && contents != " ")
                        || (row == cursor_row && col == cursor_col)
                    {
                        max_col = col + 1;
                        break;
                    }
                }
            }

            for col in 0..max_col {
                let is_cursor = should_paint_cursor(is_focused, hide_cursor, self.cursor_visible)
                    && row == cursor_row
                    && col == cursor_col;
                let is_selected = self.is_cell_selected(row, col);

                if let Some(cell) = screen.cell(row, col) {
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let cell_content = cell.contents();
                    let char_str = if cell_content.is_empty() {
                        " "
                    } else {
                        cell_content
                    };

                    let fg = if is_selected {
                        Some(theme.accent_foreground)
                    } else if is_cursor {
                        Some(theme.background)
                    } else {
                        ansi_to_hsla(cell.fgcolor(), theme.foreground, is_light_theme)
                    };

                    let bg = if is_selected {
                        Some(theme.accent)
                    } else if is_cursor {
                        Some(theme.primary)
                    } else {
                        match cell.bgcolor() {
                            vt100::Color::Default => None,
                            other => ansi_to_hsla(other, theme.background, is_light_theme),
                        }
                    };

                    let style = (fg, bg, cell.bold(), is_cursor);

                    if current_style == Some(style) {
                        current_span_text.push_str(char_str);
                    } else {
                        if let Some((cfg, cbg, bold, _cur)) = current_style {
                            if !current_span_text.is_empty() {
                                let mut span = div().child(current_span_text.clone());
                                if let Some(c) = cfg {
                                    span = span.text_color(c);
                                }
                                if let Some(c) = cbg {
                                    span = span.bg(c);
                                }
                                if bold {
                                    span = span.font_weight(FontWeight::BOLD);
                                }
                                row_spans.push(span);
                            }
                        }
                        current_span_text.clear();
                        current_span_text.push_str(char_str);
                        current_style = Some(style);
                    }
                }
            }

            if let Some((cfg, cbg, bold, _cur)) = current_style {
                if !current_span_text.is_empty() {
                    let mut span = div().child(current_span_text);
                    if let Some(c) = cfg {
                        span = span.text_color(c);
                    }
                    if let Some(c) = cbg {
                        span = span.bg(c);
                    }
                    if bold {
                        span = span.font_weight(FontWeight::BOLD);
                    }
                    row_spans.push(span);
                }
            }

            if row_spans.is_empty() {
                row_spans.push(div().child(" "));
            }

            screen_lines.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(17.55))
                    .children(row_spans),
            );
        }

        let status_banner = self.status.as_ref().map(|status_text| {
            let restart_handle = terminal_actions.clone();
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .mt_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::Info).xsmall().text_color(theme.warning))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.foreground)
                                .child(status_text.clone()),
                        ),
                )
                .child(
                    Button::new("terminal-restart-banner-btn")
                        .label("Restart Shell")
                        .icon(IconName::Redo)
                        .ghost()
                        .xsmall()
                        .on_click(move |_event, _window, cx| {
                            restart_handle.update(cx, |t, cx| t.restart(cx));
                        }),
                )
        });

        let autoscroll_pill = if self.scrollback_offset > 0 {
            let scroll_to_bottom_handle = terminal_actions.clone();
            Some(
                div().absolute().bottom(px(14.0)).right(px(24.0)).child(
                    Button::new("terminal-autoscroll-pill")
                        .label(format!(
                            "↓ Scroll to Bottom ({} lines up)",
                            self.scrollback_offset
                        ))
                        .icon(IconName::ChevronDown)
                        .xsmall()
                        .on_click(move |_event, _window, cx| {
                            scroll_to_bottom_handle.update(cx, |t, cx| t.scroll_to_bottom(cx));
                        }),
                ),
            )
        } else {
            None
        };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme.background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .child(
                div()
                    .id("pty-terminal-screen")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .font_family(".ZedMono")
                    .text_size(px(13.0))
                    .line_height(relative(1.35))
                    .cursor_text()
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                        let delta = match event.delta {
                            ScrollDelta::Lines(lines) => lines.y * 2.0,
                            ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / 17.55,
                        };
                        if delta.abs() > 0.01 {
                            this.scroll_by(delta, cx);
                        }
                    }))
                    .on_prepaint(move |bounds, _, cx| {
                        let rows = ((bounds.size.height.as_f32() - 24.0) / 17.55).floor() as u16;
                        let cols =
                            ((bounds.size.width.as_f32() - 24.0) / cell_width).floor() as u16;
                        terminal_resize.update(cx, |terminal, cx| {
                            terminal.screen_bounds = Some(bounds);
                            terminal.resize(rows, cols, cx);
                        });
                    })
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_selection))
                    .on_mouse_move(cx.listener(Self::extend_selection))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::end_selection))
                    .children(screen_lines)
                    .children(status_banner)
                    .children(autoscroll_pill)
                    .context_menu({
                        let terminal = terminal_actions.clone();
                        move |menu, _window, cx| {
                            let (output, selection) = {
                                let terminal = terminal.read(cx);
                                (terminal.screen_text(), terminal.selected_text())
                            };
                            let mut menu = menu;
                            if let Some(selection) = &selection {
                                let selection = selection.clone();
                                menu = menu.item(PopupMenuItem::new("Copy Selection").on_click(
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            selection.clone(),
                                        ));
                                    },
                                ));
                            }
                            let t_paste = terminal.clone();
                            let t_select = terminal.clone();
                            let t_clear = terminal.clone();
                            let t_restart = terminal.clone();
                            menu.item(PopupMenuItem::new("Copy Terminal Output").on_click(
                                move |_event, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        output.clone(),
                                    ));
                                },
                            ))
                            .item(PopupMenuItem::new("Paste").on_click(
                                move |_event, _window, cx| {
                                    t_paste.update(cx, |terminal, cx| {
                                        terminal.paste_from_clipboard(cx)
                                    });
                                },
                            ))
                            .item(PopupMenuItem::new("Select All").on_click(
                                move |_event, _window, cx| {
                                    t_select.update(cx, |terminal, cx| terminal.select_all(cx));
                                },
                            ))
                            .item(PopupMenuItem::new("Clear Terminal").on_click(
                                move |_event, _window, cx| {
                                    t_clear.update(cx, |terminal, cx| terminal.clear(cx));
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Restart Terminal").on_click(
                                    move |_event, _window, cx| {
                                        t_restart.update(cx, |terminal, cx| terminal.restart(cx));
                                    },
                                ),
                            )
                        }
                    }),
            )
    }
}

fn spawn_shell(
    project: &PathBuf,
    rows: u16,
    cols: u16,
    output_tx: mpsc::SyncSender<Vec<u8>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
) -> anyhow::Result<PtySession> {
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut command = CommandBuilder::new(shell);
    command.cwd(project);
    command.env("TERM", "xterm-256color");
    command.arg("-i");

    let child = pair.slave.spawn_command(command)?;
    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    drop(pair.slave);

    std::thread::Builder::new()
        .name("threadlane-gpui-pty-reader".into())
        .spawn(move || {
            let mut buffer = [0_u8; TERMINAL_READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = event_tx.send(PtyEvent::Closed);
                        break;
                    }
                    Ok(read) => {
                        if output_tx.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = event_tx.send(PtyEvent::Error(error.to_string()));
                        break;
                    }
                }
            }
        })?;

    Ok(PtySession {
        master: pair.master,
        writer,
        child,
    })
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};
    use std::time::Duration;

    use super::{
        ansi_index_to_hsla, next_terminal_wake, rgb_to_hsla, selection_bounds, should_paint_cursor,
        start_parser_worker, terminal_frame_policy, terminal_parse_budget_exhausted, ParserCommand,
        PtyEvent, TerminalWake, TERMINAL_PARSE_BUDGET_PER_FRAME,
    };

    #[test]
    fn terminal_parser_yields_at_its_frame_budget() {
        assert!(!terminal_parse_budget_exhausted(
            TERMINAL_PARSE_BUDGET_PER_FRAME - 1,
            TERMINAL_PARSE_BUDGET_PER_FRAME,
        ));
        assert!(terminal_parse_budget_exhausted(
            TERMINAL_PARSE_BUDGET_PER_FRAME,
            TERMINAL_PARSE_BUDGET_PER_FRAME,
        ));
    }

    #[test]
    fn saturated_terminal_halves_redraws_and_approximately_preserves_parse_throughput() {
        let (normal_interval, normal_budget) = terminal_frame_policy(false);
        let (flood_interval, flood_budget) = terminal_frame_policy(true);

        assert_eq!(normal_interval, Duration::from_millis(16));
        assert_eq!(flood_interval, Duration::from_millis(33));
        assert_eq!(flood_budget, normal_budget * 2);
    }

    #[tokio::test]
    async fn terminal_wake_coalesces_queued_frames_immediately() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut parser = vt100::Parser::new(2, 8, 0);
        parser.process(b"old");
        event_tx
            .send(PtyEvent::Frame(super::TerminalFrame {
                screen: parser.screen().clone(),
                scrollback: 0,
            }))
            .unwrap();
        parser.process(b"\rnew");
        event_tx
            .send(PtyEvent::Frame(super::TerminalFrame {
                screen: parser.screen().clone(),
                scrollback: 0,
            }))
            .unwrap();
        event_tx.send(PtyEvent::Error("closed".into())).unwrap();

        let TerminalWake::Events(events) = next_terminal_wake(&mut event_rx, pending()).await
        else {
            panic!("expected terminal events");
        };
        assert_eq!(events.len(), 2);
        let PtyEvent::Frame(frame) = &events[0] else {
            panic!("expected latest terminal frame");
        };
        assert_eq!(frame.screen.contents(), "new");
        assert!(matches!(events[1], PtyEvent::Error(_)));
    }

    #[tokio::test]
    async fn terminal_wake_uses_the_cursor_timer_when_idle() {
        let (_event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(matches!(
            next_terminal_wake(&mut event_rx, ready(())).await,
            TerminalWake::Blink
        ));
    }

    #[tokio::test]
    async fn parser_worker_publishes_output_and_clear_frames() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(2, 8, event_tx).unwrap();

        output_tx.send(b"hello".to_vec()).unwrap();
        let PtyEvent::Frame(frame) = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected parsed terminal frame");
        };
        assert_eq!(frame.screen.contents(), "hello");

        command_tx.send(ParserCommand::Clear).unwrap();
        let PtyEvent::Frame(frame) = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected cleared terminal frame");
        };
        assert_eq!(frame.screen.contents(), "");
    }

    #[test]
    fn xterm_color_cube_uses_standard_channel_levels() {
        assert_eq!(ansi_index_to_hsla(17, false), rgb_to_hsla(0, 0, 95));
        assert_eq!(ansi_index_to_hsla(67, false), rgb_to_hsla(95, 135, 175));
    }

    #[test]
    fn backward_selection_includes_its_anchor_cell() {
        assert_eq!(selection_bounds((0, 8), (0, 0), 20), Some(((0, 0), (0, 9))));
        assert_eq!(selection_bounds((0, 0), (0, 8), 20), Some(((0, 0), (0, 8))));
    }

    #[test]
    fn cursor_blink_only_controls_a_focused_visible_cursor() {
        assert!(should_paint_cursor(true, false, true));
        assert!(!should_paint_cursor(true, false, false));
        assert!(!should_paint_cursor(false, false, true));
        assert!(!should_paint_cursor(true, true, true));
    }
}
