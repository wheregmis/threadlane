use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

mod search;
mod input;
mod links;
#[cfg(test)]
mod links_tests;
#[cfg(test)]
mod resize_tests;

use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenu};
use links::{visible_links, TerminalLink};
pub use links::is_web_url;
use gpui_component::WindowExt;
use threadlane_ui_kit::{self as kit, TerminalFindAction, TerminalFindStatus, TerminalSelectionGranularity};
pub use threadlane_ui_kit::{CloseTerminalFind, FindInTerminalOutput, NextTerminalMatch, PreviousTerminalMatch, TerminalLinkDestination as LinkDestination};

use search::{
    cue_row, next_find_match, reveal_offset, scan_retained_output, TerminalSearchHit,
    TerminalSearchOutcome,
};
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
/// Debounce between the last query keystroke and a worker scan.
const TERMINAL_FIND_DEBOUNCE: Duration = Duration::from_millis(120);
/// Minimum interval between background rescans while find is open and output
/// keeps arriving; scans never run when find is closed.
const TERMINAL_FIND_RESCAN_INTERVAL: Duration = Duration::from_millis(250);

/// Register shared terminal search shortcuts; the host retains worker lifecycle.
pub fn init(cx: &mut App) {
    kit::init_terminal_find(cx);
}

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

fn terminal_mouse_button(button: MouseButton) -> Option<input::TerminalMouseButton> {
    match button {
        MouseButton::Left => Some(input::TerminalMouseButton::Left),
        MouseButton::Middle => Some(input::TerminalMouseButton::Middle),
        MouseButton::Right => Some(input::TerminalMouseButton::Right),
        MouseButton::Navigate(_) => None,
    }
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

/// The I/O half of a hosted terminal: input and resize travel toward the
/// shell; dropping it ends the shell. Raw output flows into the parser
/// channel the backend was handed at spawn, so every implementation shares
/// this crate's emulator and rendering.
pub trait TerminalIo: Send {
    fn write(&self, bytes: &[u8]);
    fn resize(&self, rows: u16, cols: u16);
}

impl TerminalIo for PtySession {
    fn write(&self, bytes: &[u8]) {
        PtySession::write(self, bytes);
    }

    fn resize(&self, rows: u16, cols: u16) {
        PtySession::resize(self, rows, cols);
    }
}

/// Lifecycle channel a `TerminalBackend` reports on — kept opaque so the
/// view's internal `PtyEvent` and frame types stay private.
#[derive(Clone)]
pub struct TerminalEventSink(tokio::sync::mpsc::UnboundedSender<PtyEvent>);

impl TerminalEventSink {
    /// The shell exited or its byte stream ended.
    pub fn closed(&self) {
        let _ = self.0.send(PtyEvent::Closed);
    }

    /// A transport-level failure worth surfacing in the terminal.
    pub fn error(&self, message: String) {
        let _ = self.0.send(PtyEvent::Error(message));
    }
}

/// How a `TerminalView` obtains a shell. `LocalTerminalBackend` spawns a
/// PTY on this machine; another implementation can drive a daemon-hosted
/// PTY over the wire so terminals belong to the remote host.
pub trait TerminalBackend: Send + Sync {
    fn spawn(
        &self,
        cwd: &std::path::Path,
        rows: u16,
        cols: u16,
        output_tx: mpsc::SyncSender<Vec<u8>>,
        sink: TerminalEventSink,
    ) -> Result<Box<dyn TerminalIo>, String>;
}

/// Spawns a shell on the host this process runs on (the default).
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalTerminalBackend;

impl TerminalBackend for LocalTerminalBackend {
    fn spawn(
        &self,
        cwd: &std::path::Path,
        rows: u16,
        cols: u16,
        output_tx: mpsc::SyncSender<Vec<u8>>,
        sink: TerminalEventSink,
    ) -> Result<Box<dyn TerminalIo>, String> {
        spawn_shell(&cwd.to_path_buf(), rows, cols, output_tx, sink.0)
            .map(|session| Box::new(session) as Box<dyn TerminalIo>)
            .map_err(|error| error.to_string())
    }
}

enum PtyEvent {
    Frame(TerminalFrame),
    Closed,
    Error(String),
    /// Bytes the emulator must write back to the pty. Host-side queries
    /// (e.g. ConPTY's `ESC[6n` startup probe, which stalls the shell until
    /// answered) flow through the same parse path as output, so replies ride
    /// this event to the session's writer.
    ReplyToHost(Vec<u8>),
    /// Bounded result descriptors for one find generation. `revealed` is set
    /// only when this reply also moved the viewport (settled query or
    /// navigation), so the view can trust it as the selected match.
    SearchResults {
        generation: u64,
        hits: Vec<TerminalSearchHit>,
        total: usize,
        truncated: bool,
        scrollback_len: usize,
        alt_screen: bool,
        revealed: Option<usize>,
    },
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
    LinkEpoch(u64),
    Clear,
    Resize(u16, u16),
    SetScrollback(usize),
    /// `Some(query)` opens/updates the search for `generation`; `None` closes
    /// it so the worker stops scanning entirely.
    Find {
        generation: u64,
        query: Option<String>,
    },
    /// Worker-validated navigation: the worker rescans the buffer and
    /// resolves the hit by identity (`row` + `excerpt`) in the fresh result
    /// set — never trusting the index the view navigated from — then scrolls
    /// the viewport to it. When the identity no longer exists the viewport
    /// stays put and `revealed` comes back `None`, so a stale location can
    /// never reveal unrelated output.
    RevealMatch {
        generation: u64,
        row: usize,
        excerpt: String,
    },
}

/// Worker-side find state: the query the retained buffer is searched with.
struct WorkerFind {
    generation: u64,
    query: String,
}

/// Scans the retained buffer unless the terminal is on the alternate screen
/// (full-screen TUIs) or the query is empty. Returns the outcome plus the
/// alternate-screen flag so replies can carry it.
fn worker_find_scan(
    parser: &mut vt100::Parser,
    rows: u16,
    cols: u16,
    query: &str,
) -> (TerminalSearchOutcome, bool) {
    let alt_screen = parser.screen().alternate_screen();
    let outcome = if alt_screen || query.is_empty() {
        TerminalSearchOutcome::default()
    } else {
        scan_retained_output(parser.screen_mut(), rows, cols, query)
    };
    (outcome, alt_screen)
}

fn emit_find_results(
    parser: &mut vt100::Parser,
    rows: u16,
    cols: u16,
    find: &WorkerFind,
    revealed: Option<usize>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<PtyEvent>,
) -> Result<(), tokio::sync::mpsc::error::SendError<PtyEvent>> {
    let (outcome, alt_screen) = worker_find_scan(parser, rows, cols, &find.query);
    event_tx.send(PtyEvent::SearchResults {
        generation: find.generation,
        hits: outcome.hits,
        total: outcome.total,
        truncated: outcome.truncated,
        scrollback_len: outcome.scrollback_len,
        alt_screen,
        revealed,
    })
}

struct TerminalFrame {
    link_epoch: u64,
    links: Vec<TerminalLink>,
    screen: vt100::Screen,
    scrollback: usize,
    /// Total retained scrollback rows; `scrollback` (the view offset) can
    /// range over all of it, so the view needs both numbers.
    scrollback_len: usize,
    /// Alternate-screen state travels with the frame because
    /// `state_formatted` does not reproduce it on the reconstructed screen.
    alt_screen: bool,
}

fn visible_terminal_frame(parser: &mut vt100::Parser, rows: u16, cols: u16) -> TerminalFrame {
    // vt100 clamps deep offsets to the scrollback length, so a MAX probe is
    // the cheap way to read that length without exposing it directly.
    let saved_offset = parser.screen().scrollback();
    parser.screen_mut().set_scrollback(usize::MAX);
    let scrollback_len = parser.screen().scrollback();
    parser.screen_mut().set_scrollback(saved_offset);
    // Look back exactly one row to establish whether the viewport starts mid-token.
    parser
        .screen_mut()
        .set_scrollback(saved_offset.saturating_add(1));
    let first_continues = if parser.screen().scrollback() > saved_offset {
        parser.screen().row_wrapped(0)
    } else {
        // At the retained-history boundary a clipped first token is ambiguous.
        scrollback_len > 0
    };
    parser.screen_mut().set_scrollback(saved_offset);
    let links = visible_links(parser.screen(), first_continues);
    let mut visible = vt100::Parser::new(rows, cols, 0);
    visible.process(&parser.screen().state_formatted());
    TerminalFrame {
        link_epoch: 0,
        links,
        screen: visible.screen().clone(),
        scrollback: parser.screen().scrollback(),
        scrollback_len,
        alt_screen: parser.screen().alternate_screen(),
    }
}

impl TerminalFrame {
    fn with_link_epoch(mut self, epoch: u64) -> Self {
        self.link_epoch = epoch;
        self
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
            // Find state lives here: the worker owns the retained buffer, so
            // scans and navigation always run against the real history.
            let mut find: Option<WorkerFind> = None;
            // Deferred rescan while output keeps arriving; scanning on every
            // chunk would stall PTY parsing during output floods.
            let mut find_rescan_at: Option<Instant> = None;
            // After the PTY reader ends the buffer is still searchable, so
            // the worker keeps servicing commands until the view drops the
            // channel instead of exiting with the shell.
            let mut output_disconnected = false;
            let mut link_epoch = 0;
            // Tail bytes carried across reads so a host query split between
            // two chunks still matches (a 4-byte sequence needs a 3-byte
            // carry).
            let mut query_tail: Vec<u8> = Vec::with_capacity(3);

            loop {
                let mut commands_open = true;
                'drain: while commands_open {
                    let command = match command_rx.try_recv() {
                        Ok(command) => command,
                        Err(mpsc::TryRecvError::Empty) => break 'drain,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            commands_open = false;
                            break 'drain;
                        }
                    };
                    match command {
                        ParserCommand::LinkEpoch(epoch) => link_epoch = epoch,
                        ParserCommand::Clear => {
                            parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
                            if find.is_some() {
                                find_rescan_at = Some(Instant::now());
                            }
                        }
                        ParserCommand::Resize(new_rows, new_cols) => {
                            rows = new_rows.max(1);
                            cols = new_cols.max(1);
                            parser.screen_mut().set_size(rows, cols);
                            let offset = parser.screen().scrollback();
                            parser.screen_mut().set_scrollback(offset);
                            if find.is_some() {
                                find_rescan_at = Some(Instant::now());
                            }
                        }
                        ParserCommand::SetScrollback(offset) => {
                            parser.screen_mut().set_scrollback(offset);
                        }
                        ParserCommand::Find { generation, query } => match query {
                            Some(query) => {
                                find = Some(WorkerFind { generation, query });
                                find_rescan_at = None;
                                let find_state = find.as_ref().unwrap();
                                // A settled query reveals its newest match.
                                let (outcome, _) =
                                    worker_find_scan(&mut parser, rows, cols, &find_state.query);
                                let revealed = outcome.hits.len().checked_sub(1);
                                if let Some(index) = revealed {
                                    parser.screen_mut().set_scrollback(reveal_offset(
                                        &outcome.hits[index],
                                        outcome.scrollback_len,
                                    ));
                                }
                                if event_tx
                                    .send(PtyEvent::SearchResults {
                                        generation,
                                        hits: outcome.hits,
                                        total: outcome.total,
                                        truncated: outcome.truncated,
                                        scrollback_len: outcome.scrollback_len,
                                        alt_screen: parser.screen().alternate_screen(),
                                        revealed,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            None => {
                                find = None;
                                find_rescan_at = None;
                            }
                        },
                        ParserCommand::RevealMatch {
                            generation,
                            row,
                            excerpt,
                        } => {
                            if find
                                .as_ref()
                                .is_some_and(|state| state.generation == generation)
                            {
                                let find_state = find.as_ref().unwrap();
                                let (outcome, alt_screen) =
                                    worker_find_scan(&mut parser, rows, cols, &find_state.query);
                                // Resolve the target by identity: a rescan
                                // can renumber matches, and only the same
                                // row + text is the same line.
                                let revealed = outcome.hits.iter().position(|hit| {
                                    hit.absolute_row == row && hit.excerpt == excerpt
                                });
                                if let Some(index) = revealed {
                                    parser.screen_mut().set_scrollback(reveal_offset(
                                        &outcome.hits[index],
                                        outcome.scrollback_len,
                                    ));
                                }
                                if event_tx
                                    .send(PtyEvent::SearchResults {
                                        generation,
                                        hits: outcome.hits,
                                        total: outcome.total,
                                        truncated: outcome.truncated,
                                        scrollback_len: outcome.scrollback_len,
                                        alt_screen,
                                        revealed,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    dirty = true;
                }
                if !commands_open && output_disconnected {
                    break;
                }

                let now = Instant::now();
                if now >= next_frame {
                    if dirty {
                        if event_tx
                            .send(PtyEvent::Frame(
                                visible_terminal_frame(&mut parser, rows, cols)
                                    .with_link_epoch(link_epoch),
                            ))
                            .is_err()
                        {
                            break;
                        }
                        dirty = false;
                    }
                    // Coalesced rescan: new output refreshes results without
                    // ever moving the reading position on its own.
                    if let (Some(due), Some(find_state)) = (find_rescan_at, find.as_mut()) {
                        if now >= due {
                            find_rescan_at = None;
                            if emit_find_results(
                                &mut parser,
                                rows,
                                cols,
                                find_state,
                                None,
                                &event_tx,
                            )
                            .is_err()
                            {
                                break;
                            }
                        }
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

                if output_disconnected {
                    // Sleep to the next frame instead of recv_timeout on the
                    // command channel: a recv here would consume a command
                    // without running its match arm, and after shell exit the
                    // buffer still answers Find/Resize commands.
                    std::thread::sleep(next_frame.saturating_duration_since(Instant::now()));
                    continue;
                }

                match output_rx.recv_timeout(next_frame.saturating_duration_since(Instant::now())) {
                    Ok(bytes) => {
                        parsed_bytes = parsed_bytes.saturating_add(bytes.len());
                        saturated = terminal_parse_budget_exhausted(parsed_bytes, parse_budget);
                        let queries = terminal_host_queries(&mut query_tail, &bytes);
                        let mut start = 0;
                        for (end, kind) in queries {
                            // Feed output only up to each query so the reply
                            // reports the cursor position as of the query,
                            // not as of the end of the chunk.
                            parser.process(&bytes[start..end]);
                            start = end;
                            let reply = match kind {
                                HostQuery::Status => b"\x1b[0n".to_vec(),
                                HostQuery::Cursor => {
                                    let (row, col) = parser.screen().cursor_position();
                                    format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
                                }
                            };
                            if event_tx.send(PtyEvent::ReplyToHost(reply)).is_err() {
                                break;
                            }
                        }
                        parser.process(&bytes[start..]);
                        // vt100 bumps the view offset as rows scroll into
                        // history, so the visible content stays put.
                        if find.is_some() && find_rescan_at.is_none() {
                            find_rescan_at =
                                Some(Instant::now() + TERMINAL_FIND_RESCAN_INTERVAL);
                        }
                        dirty = true;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if dirty {
                            if event_tx
                                .send(PtyEvent::Frame(
                                    visible_terminal_frame(&mut parser, rows, cols)
                                        .with_link_epoch(link_epoch),
                                ))
                                .is_err()
                            {
                                break;
                            }
                            dirty = false;
                        }
                        output_disconnected = true;
                    }
                }
            }
        })?;
    Ok((output_tx, command_tx))
}

/// Host status queries the emulator must answer: `ESC[5n` gets a generic
/// "operating normally" report (`ESC[0n`), `ESC[6n` a cursor position report.
enum HostQuery {
    Status,
    Cursor,
}

/// Locate host queries in `bytes`, returning `(end_offset_in_bytes, kind)`
/// pairs in order so the caller can feed output segment-by-segment and reply
/// with the parser state at each query. `tail` carries up to three trailing
/// bytes into the next chunk so a query split across reads still matches —
/// those bytes were already fed to the parser, so only `bytes` is scanned and
/// a full query can never sit inside the tail alone.
fn terminal_host_queries(tail: &mut Vec<u8>, bytes: &[u8]) -> Vec<(usize, HostQuery)> {
    let mut haystack = std::mem::take(tail);
    let tail_len = haystack.len();
    haystack.extend_from_slice(bytes);
    let mut queries = Vec::new();
    for (index, window) in haystack.windows(4).enumerate() {
        let kind = if window == b"\x1b[5n" {
            HostQuery::Status
        } else if window == b"\x1b[6n" {
            HostQuery::Cursor
        } else {
            continue;
        };
        // A 4-byte match always ends at or past the tail boundary, so the
        // offset lands inside `bytes`.
        queries.push((index + 4 - tail_len, kind));
    }
    tail.extend_from_slice(&haystack[haystack.len().saturating_sub(3)..]);
    queries
}

use threadlane_ui_kit::{
    terminal_selection_bounds as selection_bounds,
    terminal_selection_present as selection_present,
    terminal_selected_excerpt as selected_excerpt,
    TERMINAL_FONT_SIZE, TERMINAL_CELL_WIDTH_FALLBACK,
};

fn should_paint_cursor(is_focused: bool, terminal_hides_cursor: bool, blink_visible: bool) -> bool {
    is_focused && !terminal_hides_cursor && blink_visible
}

/// Live state of the inline Find strip: the query input plus the worker's
/// latest bounded results for the current generation. Match locations are
/// worker-owned — the view keeps descriptors (absolute row + excerpt), never
/// terminal text it could navigate to stalely.
struct TerminalFind {
    input: Entity<InputState>,
    previous_focus: Option<FocusHandle>,
    query: String,
    generation: u64,
    pending: bool,
    failed: bool,
    hits: Vec<TerminalSearchHit>,
    total: usize,
    truncated: bool,
    /// Scrollback length the current `hits` were computed against.
    scrollback_len: usize,
    selected: Option<usize>,
    /// Identity of the selected hit (absolute row + excerpt) so a refreshed
    /// result set keeps the selection only while the same line is present.
    selected_hit: Option<TerminalSearchHit>,
    _subscription: Subscription,
}

/// A point-in-time copy of the user's terminal selection, detached from
/// the live screen so later output, resize, or a shell restart can never
/// mutate it.
pub struct TerminalSelection {
    /// Selected text as displayed, whitespace and Unicode preserved.
    pub text: String,
    /// The directory the shell was launched in — honest provenance for
    /// the excerpt, not proof of the shell's current working directory
    /// after a `cd`, and not a session worktree claim.
    pub launched_in: PathBuf,
}

/// Block-relevant selection state without the excerpt payload: whether
/// the covered cells hold any non-whitespace text, and the UTF-8 byte
/// length [`selection_snapshot`](TerminalView::selection_snapshot)
/// would produce. Cheap to poll per frame — it walks cells rather than
/// copying text out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionStatus {
    /// At least one covered cell holds non-whitespace content.
    pub has_text: bool,
    /// UTF-8 bytes of the snapshot excerpt: cell contents plus one line
    /// break per covered row that is not a soft wrap.
    pub excerpt_len: usize,
}

#[derive(Clone, Debug)]
pub struct OpenTerminalLink {
    pub url: String,
    pub destination: LinkDestination,
}

impl EventEmitter<OpenTerminalLink> for TerminalView {}

/// A persistent, focusable project shell backed by a real pseudo-terminal.
///
/// Construct it with `cx.new(|cx| TerminalView::new(project, cx))` and
/// render the resulting `Entity<TerminalView>` directly from its parent view.
pub struct TerminalView {
    links: Vec<TerminalLink>,
    link_press: Option<(TerminalLink, Point<Pixels>)>,
    link_menu: Option<Entity<PopupMenu>>,
    link_menu_focus: Option<FocusHandle>,
    link_menu_subscription: Option<Subscription>,
    link_epoch: u64,
    retry_url: Option<String>,
    // Worker echo rejects frames queued before clear/restart/geometry changes.
    frame_epoch: u64,
    dismissed_link_focus: Option<FocusHandle>,
    context_link: Option<(String, u64)>,
    context_menu_suppressed: bool,
    project: PathBuf,
    focus_handle: FocusHandle,
    screen: vt100::Screen,
    parser_command_tx: Option<mpsc::Sender<ParserCommand>>,
    session: Option<Box<dyn TerminalIo>>,
    backend: Arc<dyn TerminalBackend>,
    event_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
    status: Option<String>,
    rows: u16,
    cols: u16,
    screen_bounds: Option<Bounds<Pixels>>,
    selection_anchor: Option<(u16, u16)>,
    selection_head: Option<(u16, u16)>,
    selection_granularity: Option<(TerminalSelectionGranularity, (u16, u16))>,
    cell_width: f32,
    content_inset: f32,
    font_size: f32,
    compact: bool,
    translucent_background: bool,
    cursor_visible: bool,
    scrollback_offset: usize,
    scroll_accumulator: f32,
    mouse_wheel_accumulator: f32,
    scrollback_len: usize,
    alt_screen: bool,
    last_mouse_cell: Option<(u16, u16)>,
    find: Option<TerminalFind>,
    /// Monotonic seed for find generations so a generation is never reused
    /// across find instances or shell restarts in this view: a delayed reply
    /// from an older find can then never land under a newer one.
    find_generation_seed: u64,
}

impl TerminalView {
    pub fn new(project: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::new_with_backend(project, Arc::new(LocalTerminalBackend), cx)
    }

    /// Same as `new` with an explicit shell provider: a remote backend runs
    /// the terminal's PTY on the daemon host instead of this machine.
    pub fn new_with_backend(
        project: PathBuf,
        backend: Arc<dyn TerminalBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_start(project, backend, cx, true)
    }

    /// Same as `new` minus the real PTY: tests construct selection and
    /// screen state by hand, and a live pty-reader thread would trip the
    /// test scheduler's deterministic-thread guard.
    #[cfg(test)]
    fn new_for_test(project: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::new_with_start(project, Arc::new(LocalTerminalBackend), cx, false)
    }

    fn new_with_start(
        project: PathBuf,
        backend: Arc<dyn TerminalBackend>,
        cx: &mut Context<Self>,
        autostart: bool,
    ) -> Self {
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
            links: Vec::new(),
            link_press: None,
            link_menu: None,
            link_menu_focus: None,
            link_menu_subscription: None,
            link_epoch: 0,
            retry_url: None,
            frame_epoch: 0,
            dismissed_link_focus: None,
            context_link: None,
            context_menu_suppressed: false,
            project,
            focus_handle: cx.focus_handle(),
            screen: vt100::Parser::new(DEFAULT_ROWS, DEFAULT_COLS, 0)
                .screen()
                .clone(),
            parser_command_tx: None,
            session: None,
            backend,
            event_tx,
            status: None,
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            screen_bounds: None,
            selection_anchor: None,
            selection_head: None,
            selection_granularity: None,
            cell_width: TERMINAL_CELL_WIDTH_FALLBACK,
            content_inset: 12.0, // Default p_3 until the first resolved window frame.
            font_size: TERMINAL_FONT_SIZE,
            compact: false,
            translucent_background: false,
            cursor_visible: true,
            scrollback_offset: 0,
            scroll_accumulator: 0.0,
            mouse_wheel_accumulator: 0.0,
            scrollback_len: 0,
            alt_screen: false,
            last_mouse_cell: None,
            find: None,
            find_generation_seed: 0,
        };
        if autostart {
            terminal.start();
        }
        terminal
    }

    /// Sends raw input bytes into the terminal's shell.
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
        self.invalidate_links();
        self.session.take();
        self.parser_command_tx = None;
        self.screen = vt100::Parser::new(self.rows, self.cols, 0).screen().clone();
        self.scrollback_offset = 0;
        self.scroll_accumulator = 0.0;
        self.mouse_wheel_accumulator = 0.0;
        self.scrollback_len = 0;
        self.alt_screen = false;
        self.last_mouse_cell = None;
        self.find = None;
        self.status = None;
        self.clear_selection();
        self.start();
        cx.notify();
    }

    /// Clears both the emulator scrollback and the visible screen.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.invalidate_links();
        self.screen = vt100::Parser::new(self.rows, self.cols, 0).screen().clone();
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::Clear);
            self.sync_link_frame();
        }
        self.scrollback_offset = 0;
        self.scroll_accumulator = 0.0;
        self.mouse_wheel_accumulator = 0.0;
        self.last_mouse_cell = None;
        self.status = None;
        self.clear_selection();
        cx.notify();
    }

    /// Scrolls the terminal view by a number of lines (positive = into scrollback history, negative = towards bottom).
    fn scroll_by(&mut self, lines: f32, cx: &mut Context<Self>) {
        self.link_press = None;
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
    fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.scrollback_offset != 0 {
            self.clear_selection();
        }
        self.scrollback_offset = 0;
        self.scroll_accumulator = 0.0;
        self.set_scrollback(0);
        cx.notify();
    }

    /// Scrolls all the way to the top of available scrollback history.
    fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        let previous_offset = self.scrollback_offset;
        self.scroll_accumulator = 0.0;
        self.set_scrollback(self.scrollback_len);
        if self.scrollback_offset != previous_offset {
            self.clear_selection();
        }
        cx.notify();
    }

    /// Updates the PTY and terminal parser dimensions. Parents can call this when
    /// they have measured cell dimensions for their allocated terminal bounds.
    fn resize(&mut self, rows: u16, cols: u16, cx: &mut Context<Self>) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if (rows, cols) == (self.rows, self.cols) {
            return;
        }
        self.invalidate_links();
        self.last_mouse_cell = None;
        self.mouse_wheel_accumulator = 0.0;
        self.rows = rows;
        self.cols = cols;
        self.clear_selection();
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::Resize(rows, cols));
            self.sync_link_frame();
        }
        if let Some(session) = &self.session {
            session.resize(rows, cols);
        }
        cx.notify();
    }

    pub fn project(&self) -> &PathBuf {
        &self.project
    }

    /// Whether the visible screen holds any output. Used to confirm closing
    /// a shell that would discard scrollback/build output with one misclick.
    pub fn has_output(&self) -> bool {
        !self.screen.contents().trim().is_empty()
    }

    fn start(&mut self) {
        let result = start_parser_worker(self.rows, self.cols, self.event_tx.clone())
            .map_err(|e| e.to_string())
            .and_then(|(output_tx, command_tx)| {
                self.backend
                    .spawn(
                        &self.project,
                        self.rows,
                        self.cols,
                        output_tx,
                        TerminalEventSink(self.event_tx.clone()),
                    )
                    .map(|session| (session, command_tx))
            });
        match result {
            Ok((session, command_tx)) => {
                self.session = Some(session);
                let _ = command_tx.send(ParserCommand::LinkEpoch(self.frame_epoch));
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
                if frame.link_epoch != self.frame_epoch {
                    return;
                }
                self.link_press = None;
                if self.alt_screen != frame.alt_screen || self.scrollback_offset != frame.scrollback
                {
                    self.invalidate_links();
                    self.sync_link_frame();
                }
                if self.alt_screen != frame.alt_screen
                    || self.screen.mouse_protocol_mode() != frame.screen.mouse_protocol_mode()
                    || self.screen.mouse_protocol_encoding()
                        != frame.screen.mouse_protocol_encoding()
                {
                    self.last_mouse_cell = None;
                    self.mouse_wheel_accumulator = 0.0;
                }
                self.links = frame.links;
                self.screen = frame.screen;
                self.scrollback_offset = frame.scrollback;
                self.scrollback_len = frame.scrollback_len;
                self.alt_screen = frame.alt_screen;
            }
            PtyEvent::SearchResults {
                generation,
                hits,
                total,
                truncated,
                scrollback_len,
                alt_screen,
                revealed,
            } => {
                if self
                    .find
                    .as_ref()
                    .is_some_and(|find| find.generation == generation)
                    && self.alt_screen != alt_screen
                {
                    self.invalidate_links();
                    self.sync_link_frame();
                }
                if self.alt_screen != alt_screen {
                    self.last_mouse_cell = None;
                    self.mouse_wheel_accumulator = 0.0;
                }
                // Replies for an older query generation never land; the
                // generation moves on every query edit and on close.
                let Some(find) = &mut self.find else {
                    return;
                };
                if find.generation != generation {
                    return;
                }
                self.alt_screen = alt_screen;
                find.pending = false;
                find.failed = false;
                find.hits = hits;
                find.total = total;
                find.truncated = truncated;
                find.scrollback_len = scrollback_len;
                if let Some(index) = revealed {
                    find.selected = Some(index);
                    find.selected_hit = find.hits.get(index).cloned();
                } else if let Some(identity) = &find.selected_hit {
                    // Keep the selection glued to the same line; a hit whose
                    // row moved out of view is simply deselected, never
                    // repointed at different output.
                    find.selected = find.hits.iter().position(|hit| {
                        hit.absolute_row == identity.absolute_row
                            && hit.excerpt == identity.excerpt
                    });
                    if find.selected.is_none() {
                        find.selected_hit = None;
                    }
                }
            }
            PtyEvent::ReplyToHost(bytes) => {
                if let Some(session) = &self.session {
                    session.write(&bytes);
                }
            }
            PtyEvent::Closed => {
                if self.session.is_some() {
                    self.status = Some("Shell exited. Select Restart to open a new shell.".into());
                }
            }
            PtyEvent::Error(error) => self.status = Some(format!("Terminal read failed: {error}")),
        }
    }

    // The worker holds the real emulator and clamps deep offsets to the
    // retained length itself; the view clamps against its last-known length
    // so wheel scrolling can reach any retained row.
    fn set_scrollback(&mut self, offset: usize) {
        self.invalidate_links();
        self.scrollback_offset = offset.min(self.scrollback_len);
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::SetScrollback(self.scrollback_offset));
            self.sync_link_frame();
        }
    }

    fn send(&self, bytes: &[u8]) {
        if let Some(session) = &self.session {
            session.write(bytes);
        }
    }

    fn paste(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(bytes) = input::encode_paste(&text, self.screen.bracketed_paste()) {
            self.clear_selection();
            self.scroll_to_bottom(cx);
            self.cursor_visible = true;
            self.send(&bytes);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        if let Some(find) = &self.find {
            if find.input.read(cx).focus_handle(cx).is_focused(window) {
                // Keystrokes inside the find input belong to the input: they
                // must not reach the shell as raw PTY bytes.
                return;
            }
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.platform {
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

        if cfg!(target_os = "macos") && modifiers.platform && key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.paste(text, cx);
            }
            cx.stop_propagation();
            return;
        }

        // Platform copy shortcut: copy the selection when one exists, otherwise
        // fall through so the keystroke still sends ^C to the shell.
        if cfg!(target_os = "macos") && modifiers.platform && key.eq_ignore_ascii_case("c") {
            if let Some(text) = self.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
                return;
            }
            self.send(&[0x03]);
            cx.stop_propagation();
            return;
        }
        if !cfg!(target_os = "macos") && modifiers.control && modifiers.shift {
            if key.eq_ignore_ascii_case("c") {
                if let Some(text) = self.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                cx.stop_propagation();
                return;
            }
            if key.eq_ignore_ascii_case("v") {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.paste(text, cx);
                }
                cx.stop_propagation();
                return;
            }
        }

        let bytes = input::encode_key(
            key,
            event.keystroke.key_char.as_deref(),
            modifiers,
            self.screen.application_cursor(),
        );

        if let Some(bytes) = bytes {
            if !modifiers.control && !modifiers.alt && !modifiers.platform {
                self.clear_selection();
                self.scroll_to_bottom(cx);
                self.cursor_visible = true;
            }
            self.send(&bytes);
            cx.stop_propagation();
        }
    }

    /// Dismiss transient destinations when the owner changes the displayed terminal.
    pub fn dismiss_links(&mut self, cx: &mut Context<Self>) {
        self.context_link = None;
        self.retry_url = None;
        self.link_press = None;
        self.dismissed_link_focus = self.link_menu_focus.take();
        self.link_menu = None;
        self.link_menu_subscription = None;
        self.link_epoch = self.link_epoch.wrapping_add(1);
        cx.notify();
    }

    fn invalidate_links(&mut self) {
        self.frame_epoch = self.frame_epoch.wrapping_add(1);
        self.context_link = None;
        self.retry_url = None;
        self.link_press = None;
        self.links.clear();
        self.dismissed_link_focus = self.link_menu_focus.take();
        self.link_menu = None;
        self.link_menu_subscription = None;
        self.link_epoch = self.link_epoch.wrapping_add(1);
    }

    fn sync_link_frame(&self) {
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::LinkEpoch(self.frame_epoch));
        }
    }

    fn link_at(&self, position: Point<Pixels>) -> Option<&TerminalLink> {
        let cell = self.grid_geometry()?.link_cell_at(position)?;
        self.links.iter().find(|link| link.cells.contains(&cell))
    }

    fn activate_link(
        &mut self,
        url: String,
        destination: LinkDestination,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        if epoch == self.link_epoch && !self.alt_screen && is_web_url(&url) {
            cx.emit(OpenTerminalLink { url, destination });
        }
    }

    fn link_commands(
        menu: PopupMenu,
        url: String,
        terminal: WeakEntity<Self>,
        epoch: u64,
    ) -> PopupMenu {
        kit::terminal_link_commands(
            menu,
            url,
            cfg!(target_os = "macos"),
            move |url, destination, _, cx| {
                let _ = terminal.update(cx, |terminal, cx| {
                    terminal.activate_link(url, destination, epoch, cx)
                });
            },
        )
    }

    fn output_context_menu(
        terminal: Entity<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu, window, cx| {
            let view = terminal.read(cx);
            if view.context_menu_suppressed {
                return menu;
            }
            let output = view.screen_text();
            let selection = view.selected_text();
            let presentation = kit::TerminalOutputMenu::new(view.font_size)
                .compact(view.compact)
                .blend(view.translucent_background)
                .selection(selection.is_some());
            let menu = if let Some((url, epoch)) = view.context_link.clone() {
                Self::link_commands(menu, url, terminal.downgrade(), epoch).separator()
            } else {
                menu
            };
            let terminal = terminal.clone();
            presentation.render(
                menu,
                move |action, window, cx| {
                    terminal.update(cx, |view, cx| {
                        use kit::TerminalOutputAction as Action;
                        match action {
                            Action::CopySelection => {
                                if let Some(text) = &selection {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                }
                            }
                            Action::CopyOutput => {
                                cx.write_to_clipboard(ClipboardItem::new_string(output.clone()));
                            }
                            Action::FontSize(size) => {
                                view.invalidate_links();
                                view.font_size = size;
                                view.sync_link_frame();
                                cx.notify();
                            }
                            Action::ToggleCompact => {
                                view.invalidate_links();
                                view.compact = !view.compact;
                                view.sync_link_frame();
                                cx.notify();
                            }
                            Action::ToggleBackground => {
                                view.translucent_background = !view.translucent_background;
                                cx.notify();
                            }
                            Action::Find => {
                                view.open_find(&FindInTerminalOutput, window, cx);
                            }
                            Action::Paste => view.paste_from_clipboard(cx),
                            Action::SelectAll => view.select_all(cx),
                            Action::Clear => view.clear(cx),
                            Action::Restart => view.restart(cx),
                        }
                    });
                },
                window,
                cx,
            )
        }
    }

    pub fn retry_link(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.retry_url = Some(url);
        self.open_links(window, cx);
    }

    /// A standard menu owns keyboard navigation and a frozen, deduplicated viewport snapshot.
    pub fn open_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        let terminal = cx.weak_entity();
        let epoch = self.link_epoch;
        let picker = kit::TerminalLinkPicker::new(self.links.iter().map(|link| link.url.clone()))
            .retry_url(self.retry_url.clone())
            .in_app_browser(cfg!(target_os = "macos"))
            .full_screen(self.alt_screen);
        let menu = PopupMenu::build(window, cx, |menu, _, _| {
            picker.render(
                menu.action_context(self.focus_handle.clone()),
                move |url, destination, _, cx| {
                    let _ = terminal.update(cx, |terminal, cx| {
                        terminal.activate_link(url, destination, epoch, cx)
                    });
                },
            )
        });
        self.link_menu_subscription = Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.link_menu_focus = None;
            this.link_menu = None;
            cx.notify();
        }));
        self.link_menu_focus = Some(menu.read(cx).focus_handle(cx));
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.link_menu = Some(menu);
        cx.notify();
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

    fn row_height(&self) -> f32 {
        self.font_size * kit::terminal_line_height(self.compact)
    }

    fn grid_geometry(&self) -> Option<kit::TerminalGridGeometry> {
        Some(kit::TerminalGridGeometry::new(
            self.screen_bounds?, (self.rows, self.cols), self.cell_width,
            self.row_height(), self.content_inset,
        ))
    }

    fn cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        self.grid_geometry()?.cell_at(position)
    }

    fn report_cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        self.grid_geometry()?.link_cell_at(position)
    }

    fn mouse_reporting_active(&self) -> bool {
        self.scrollback_offset == 0
            && self.screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    fn report_mouse(
        &mut self,
        position: Point<Pixels>,
        kind: input::MouseEventKind,
        button: input::TerminalMouseButton,
        modifiers: Modifiers,
    ) {
        let mode = self.screen.mouse_protocol_mode();
        if !self.mouse_reporting_active() {
            return;
        }
        let cell = match kind {
            input::MouseEventKind::Release
            | input::MouseEventKind::Motion {
                button: Some(_), ..
            } => self.cell_at(position),
            input::MouseEventKind::Press | input::MouseEventKind::Motion { button: None } => {
                self.report_cell_at(position)
            }
        };
        let Some(cell) = cell else {
            return;
        };
        if matches!(kind, input::MouseEventKind::Motion { .. })
            && self.last_mouse_cell == Some(cell)
        {
            return;
        }
        let Some(bytes) = input::encode_mouse(
            mode,
            self.screen.mouse_protocol_encoding(),
            kind,
            button,
            cell,
            modifiers,
        ) else {
            return;
        };
        self.send(&bytes);
        if matches!(kind, input::MouseEventKind::Motion { .. }) {
            self.last_mouse_cell = Some(cell);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.last_mouse_cell = None;
        self.context_menu_suppressed = event.button == MouseButton::Right
            && !event.modifiers.shift
            && self.mouse_reporting_active();
        if !event.modifiers.shift && self.mouse_reporting_active() {
            self.focus_handle.focus(window, cx);
            if let Some(button) = terminal_mouse_button(event.button) {
                self.report_mouse(
                    event.position,
                    input::MouseEventKind::Press,
                    button,
                    event.modifiers,
                );
                cx.stop_propagation();
            }
            return;
        }
        if event.button == MouseButton::Right {
            self.context_link = self
                .link_at(event.position)
                .map(|link| (link.url.clone(), self.link_epoch));
        } else if event.button == MouseButton::Left {
            self.begin_selection(event, window, cx);
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.last_mouse_cell = None;
        if !event.modifiers.shift && self.mouse_reporting_active() {
            if let Some(button) = terminal_mouse_button(event.button) {
                self.report_mouse(
                    event.position,
                    input::MouseEventKind::Release,
                    button,
                    event.modifiers,
                );
                cx.stop_propagation();
            }
            return;
        }
        if event.button == MouseButton::Left {
            self.end_selection(event, window, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.shift {
            self.last_mouse_cell = None;
        }
        if !event.modifiers.shift && self.mouse_reporting_active() {
            self.report_mouse(
                event.position,
                input::MouseEventKind::Motion {
                    button: event.pressed_button,
                },
                input::TerminalMouseButton::Left,
                event.modifiers,
            );
            cx.stop_propagation();
        } else {
            self.extend_selection(event, window, cx);
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, row_height: f32, cx: &mut Context<Self>) {
        if event.modifiers.shift {
            let delta = match event.delta {
                ScrollDelta::Lines(lines) => lines.y * 2.0,
                ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / row_height,
            };
            self.scroll_by(delta, cx);
            return;
        }
        let delta = match event.delta {
            ScrollDelta::Lines(lines) => lines.y * 2.0,
            ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / row_height,
        };
        if self.mouse_reporting_active() {
            self.mouse_wheel_accumulator += delta;
            let steps = self.mouse_wheel_accumulator.trunc() as isize;
            self.mouse_wheel_accumulator -= steps as f32;
            let count = steps.unsigned_abs().min(32);
            if count > 0 {
                let button = if steps > 0 {
                    input::TerminalMouseButton::WheelUp
                } else {
                    input::TerminalMouseButton::WheelDown
                };
                for _ in 0..count {
                    self.report_mouse(
                        event.position,
                        input::MouseEventKind::Press,
                        button,
                        event.modifiers,
                    );
                }
            }
            cx.stop_propagation();
        } else if self.alt_screen {
            self.scroll_accumulator += delta;
            let steps = self.scroll_accumulator.trunc() as isize;
            self.scroll_accumulator -= steps as f32;
            let count = steps.unsigned_abs().min(32);
            if count > 0 {
                let key = if steps > 0 { "up" } else { "down" };
                if let Some(bytes) = input::encode_key(
                    key,
                    None,
                    Modifiers::default(),
                    self.screen.application_cursor(),
                ) {
                    for _ in 0..count {
                        self.send(&bytes);
                    }
                }
            }
            cx.stop_propagation();
        } else if delta.abs() > 0.01 {
            self.scroll_by(delta, cx);
        }
    }

    fn selected_text(&self) -> Option<String> {
        selected_excerpt(
            &self.screen,
            self.selection_anchor,
            self.selection_head,
            self.cols,
        )
    }

    /// Whether a real selection currently covers at least one cell. A
    /// cheap presence check for affordances tracking selection state —
    /// read the text with [`selection_snapshot`](Self::selection_snapshot).
    pub fn has_selection(&self) -> bool {
        selection_present(self.selection_anchor, self.selection_head, self.cols)
    }

    /// Read-only copy of the current selection for cross-surface
    /// handoffs. `None` while no real selection exists (no anchors, or a
    /// zero-width drag). The text is copied out of the live screen at
    /// call time, so coordinates are never reinterpreted against older
    /// frames and later output, resize, or a restart cannot change the
    /// snapshot.
    pub fn selection_snapshot(&self) -> Option<TerminalSelection> {
        let text = self.selected_text()?;
        Some(TerminalSelection {
            text,
            launched_in: self.project.clone(),
        })
    }

    /// Availability facts about the current selection, derived by
    /// walking the covered cells instead of copying the excerpt —
    /// affordances that refresh on selection changes can evaluate it on
    /// every notify without paying for a string copy. Mirrors the
    /// accounting `contents_between` performs.
    pub fn selection_status(&self) -> Option<SelectionStatus> {
        let (anchor, head) = (self.selection_anchor?, self.selection_head?);
        let (start, end) = selection_bounds(anchor, head, self.cols)?;
        let mut has_text = false;
        let mut excerpt_len = 0usize;
        for row in start.0..=end.0 {
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { self.cols };
            for col in from..to {
                if let Some(cell) = self.screen.cell(row, col) {
                    let contents = cell.contents();
                    excerpt_len += contents.len();
                    has_text |= !contents.trim().is_empty();
                }
            }
            if row != end.0 && !self.screen.row_wrapped(row) {
                excerpt_len += 1;
            }
        }
        Some(SelectionStatus {
            has_text,
            excerpt_len,
        })
    }

    fn clear_selection(&mut self) {
        self.link_press = None;
        self.selection_anchor = None;
        self.selection_head = None;
        self.selection_granularity = None;
    }

    fn begin_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.link_press = None;
        let modifier = if cfg!(target_os = "macos") {
            event.modifiers.platform
        } else {
            event.modifiers.control
        };
        if modifier && event.click_count == 1 {
            self.link_press = self
                .link_at(event.position)
                .cloned()
                .map(|link| (link, event.position));
        }
        let Some(cell) = self.cell_at(event.position) else {
            return;
        };
        let granularity = match event.click_count {
            2 => Some(TerminalSelectionGranularity::Word),
            count if count >= 3 => Some(TerminalSelectionGranularity::Line),
            _ => None,
        };
        if let Some(granularity) = granularity {
            if let Some((anchor, head)) =
                kit::terminal_granular_selection(&self.screen, cell, cell, self.cols, granularity)
            {
                self.selection_anchor = Some(anchor);
                self.selection_head = Some(head);
                self.selection_granularity = Some((granularity, cell));
            }
        } else {
            self.selection_anchor = Some(cell);
            self.selection_head = Some(cell);
            self.selection_granularity = None;
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn extend_selection(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.link_press.as_ref().is_some_and(|(_, start)| {
            (event.position.x - start.x).abs() > px(3.)
                || (event.position.y - start.y).abs() > px(3.)
        }) {
            self.link_press = None;
        }
        if event.dragging() && self.selection_anchor.is_some() {
            if let (Some((granularity, origin)), Some(cell)) =
                (self.selection_granularity, self.cell_at(event.position))
            {
                if let Some((anchor, head)) = kit::terminal_granular_selection(
                    &self.screen,
                    origin,
                    cell,
                    self.cols,
                    granularity,
                ) {
                    self.selection_anchor = Some(anchor);
                    self.selection_head = Some(head);
                }
            } else {
                self.selection_head = self.cell_at(event.position);
            }
            cx.notify();
        }
    }

    fn end_selection(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((link, start)) = self.link_press.take() {
            let modifier = if cfg!(target_os = "macos") {
                event.modifiers.platform
            } else {
                event.modifiers.control
            };
            if modifier
                && (event.position.x - start.x).abs() <= px(3.)
                && (event.position.y - start.y).abs() <= px(3.)
                && self.link_at(event.position) == Some(&link)
            {
                let destination = if cfg!(target_os = "macos") {
                    LinkDestination::Threadlane
                } else {
                    LinkDestination::DefaultBrowser
                };
                self.activate_link(link.url, destination, self.link_epoch, cx);
                self.clear_selection();
                return;
            }
        }
        if let (Some((granularity, origin)), Some(cell)) =
            (self.selection_granularity, self.cell_at(event.position))
        {
            if let Some((anchor, head)) =
                kit::terminal_granular_selection(&self.screen, origin, cell, self.cols, granularity)
            {
                self.selection_anchor = Some(anchor);
                self.selection_head = Some(head);
            }
        } else {
            self.selection_head = self.cell_at(event.position).or(self.selection_head);
        }
        cx.notify();
    }
    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection_anchor = Some((0, 0));
        self.selection_head = Some((self.rows.saturating_sub(1), self.cols));
        cx.notify();
    }

    fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste(text, cx);
        }
    }

    /// Opens the inline find strip (or re-selects the query when it is
    /// already open). Called by the FindInTerminalOutput action, the toolbar
    /// button, and the context-menu item — one command, one path.
    pub fn open_find(
        &mut self,
        _: &FindInTerminalOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        if self.find.is_none() {
            let generation = self.next_find_generation();
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("Find in retained output…")
            });
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let query = input.read(cx).value().to_string();
                        this.update_find_query(query, cx);
                    }
                },
            );
            self.find = Some(TerminalFind {
                input,
                previous_focus: window.focused(cx),
                query: String::new(),
                generation,
                pending: false,
                failed: false,
                hits: Vec::new(),
                total: 0,
                truncated: false,
                scrollback_len: self.scrollback_len,
                selected: None,
                selected_hit: None,
                _subscription: subscription,
            });
            self.send_find_query();
        }
        let Some(find) = &self.find else {
            return;
        };
        find.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn close_find(
        &mut self,
        _: &CloseTerminalFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(find) = self.find.take() else {
            cx.propagate();
            return;
        };
        if window.has_active_dialog(cx) {
            self.find = Some(find);
            cx.propagate();
            return;
        }
        // Stop worker-side scanning: no background work while find is closed.
        if let Some(parser) = &self.parser_command_tx {
            let _ = parser.send(ParserCommand::Find {
                generation: find.generation,
                query: None,
            });
        }
        let focus = find
            .previous_focus
            .unwrap_or_else(|| self.focus_handle.clone());
        window.focus(&focus, cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn next_find_generation(&mut self) -> u64 {
        self.find_generation_seed += 1;
        self.find_generation_seed
    }

    fn update_find_query(&mut self, query: String, cx: &mut Context<Self>) {
        if self
            .find
            .as_ref()
            .is_none_or(|find| find.query == query)
        {
            return;
        }
        let generation = self.next_find_generation();
        let find = self.find.as_mut().unwrap();
        find.query = query;
        find.generation = generation;
        find.failed = false;
        find.hits.clear();
        find.total = 0;
        find.selected = None;
        find.selected_hit = None;
        find.pending = !find.query.is_empty();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TERMINAL_FIND_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let Some(find) = &mut this.find else {
                    return;
                };
                if find.generation != generation {
                    return;
                }
                this.send_find_query();
                cx.notify();
            });
        })
        .detach();
    }

    /// Sends the current query to the parser worker, or marks the strip
    /// failed when the worker is gone (dead session, closed channel).
    fn send_find_query(&mut self) {
        let Some(find) = &mut self.find else {
            return;
        };
        let sent = self.parser_command_tx.as_ref().is_some_and(|parser| {
            parser
                .send(ParserCommand::Find {
                    generation: find.generation,
                    query: Some(find.query.clone()),
                })
                .is_ok()
        });
        if !sent {
            find.failed = true;
            find.pending = false;
        }
    }

    fn retry_find(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        find.failed = false;
        find.pending = !find.query.is_empty();
        self.send_find_query();
        cx.notify();
    }

    fn navigate_find(&mut self, previous: bool, cx: &mut Context<Self>) {
        let alt_screen = self.alt_screen;
        let Some(find) = &mut self.find else {
            return;
        };
        if find.pending || find.failed || find.hits.is_empty() || alt_screen {
            return;
        }
        let Some(index) = next_find_match(find.selected, find.hits.len(), previous) else {
            return;
        };
        find.selected = Some(index);
        find.selected_hit = find.hits.get(index).cloned();
        let generation = find.generation;
        // Navigation asks the worker to reveal by hit identity; the worker
        // resolves it in a fresh scan so output churn cannot reveal a
        // different line.
        if let (Some(hit), Some(parser)) = (&find.selected_hit, &self.parser_command_tx) {
            let _ = parser.send(ParserCommand::RevealMatch {
                generation,
                row: hit.absolute_row,
                excerpt: hit.excerpt.clone(),
            });
        }
        cx.notify();
    }

    fn next_terminal_match(
        &mut self,
        _: &NextTerminalMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_find(false, cx);
        cx.stop_propagation();
    }

    fn previous_terminal_match(
        &mut self,
        _: &PreviousTerminalMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_find(true, cx);
        cx.stop_propagation();
    }

    fn find_status(&self) -> TerminalFindStatus {
        let Some(find) = &self.find else {
            return TerminalFindStatus::Empty;
        };
        if self.alt_screen {
            TerminalFindStatus::Unavailable
        } else if find.failed {
            TerminalFindStatus::Failed
        } else if find.query.is_empty() {
            TerminalFindStatus::Empty
        } else if find.pending {
            TerminalFindStatus::Searching
        } else {
            TerminalFindStatus::results(find.total, find.hits.len(), find.selected)
        }
    }

    fn render_find_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(find) = &self.find else {
            return div().into_any_element();
        };
        let owner = cx.weak_entity();
        kit::TerminalFindStrip::new(&find.input, self.find_status())
            .scrolled(self.scrollback_offset > 0)
            .excerpt(
                find.selected
                    .and_then(|index| find.hits.get(index))
                    .map(|hit| hit.excerpt.clone().into()),
            )
            .render(
                move |action, window, cx| {
                    let _ = owner.update(cx, |host, cx| match action {
                        TerminalFindAction::Previous => host.navigate_find(true, cx),
                        TerminalFindAction::Next => host.navigate_find(false, cx),
                        TerminalFindAction::Retry => host.retry_find(cx),
                        TerminalFindAction::JumpToLive => host.scroll_to_bottom(cx),
                        TerminalFindAction::Close => {
                            host.close_find(&CloseTerminalFind, window, cx)
                        }
                    });
                },
                cx,
            )
            .into_any_element()
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(focus) = self.dismissed_link_focus.take() {
            if focus.contains_focused(window, cx) {
                self.focus_handle.focus(window, cx);
            }
        }
        let terminal_resize = cx.entity().clone();
        let terminal_actions = cx.entity().clone();
        let is_focused = self.focus_handle.is_focused(window);
        let metrics = kit::TerminalTextMetrics::measure(self.font_size, self.compact, window, cx);
        self.cell_width = metrics.cell_width();
        self.content_inset = metrics.inset(window);
        let row_height = metrics.row_height();

        // The selected find hit paints as a row-level background cue: it
        // sits behind the per-cell ANSI colors and is never part of copied
        // text, so a selection cannot change what Copy produces.
        let find_cue_row = self.find.as_ref().and_then(|find| {
            let hit = find.selected.and_then(|index| find.hits.get(index))?;
            cue_row(hit, self.scrollback_len, self.scrollback_offset, self.rows)
        });

        let screen_grid = kit::TerminalGrid::new(
            ("pty-terminal-screen", self.link_epoch as usize),
            &self.screen,
            metrics.clone(),
        )
        .selection(self.selection_anchor, self.selection_head)
        .cursor(should_paint_cursor(
            is_focused,
            self.screen.hide_cursor(),
            self.cursor_visible,
        ))
        .find_cue(find_cue_row)
        .links(
            self.links
                .iter()
                .map(|link| (link.url.as_str(), link.cells.as_slice())),
        )
        .on_layout(move |bounds, window, cx| {
            let (rows, cols) = metrics.grid_size(bounds.size, window);
            let inset = metrics.inset(window);
            terminal_resize.update(cx, |terminal, cx| {
                if terminal.screen_bounds != Some(bounds) {
                    terminal.link_press = None;
                }
                terminal.screen_bounds = Some(bounds);
                terminal.content_inset = inset;
                terminal.resize(rows, cols, cx);
            });
        })
        .render(cx);

        let status_banner = self.status.as_ref().map(|status_text| {
            let restart = terminal_actions.clone();
            let is_error = status_text.starts_with("Terminal read failed")
                || status_text.starts_with("Unable to start terminal");
            kit::terminal_status(
                status_text.clone(),
                is_error,
                move |_, cx| {
                    restart.update(cx, |view, cx| view.restart(cx));
                },
                cx,
            )
        });
        let autoscroll_pill = (self.scrollback_offset > 0).then(|| {
            let terminal = terminal_actions.clone();
            kit::terminal_live_output(
                self.scrollback_offset,
                move |_, cx| {
                    terminal.update(cx, |view, cx| view.scroll_to_bottom(cx));
                },
                cx,
            )
        });

        let find_strip = self.find.as_ref().map(|_| self.render_find_strip(cx));

        kit::terminal_output_surface("pty-terminal-root", self.translucent_background, cx)
            .key_context("Terminal")
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::close_find))
            .on_action(cx.listener(Self::next_terminal_match))
            .on_action(cx.listener(Self::previous_terminal_match))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .children(find_strip)
            .children(self.link_menu.as_ref().map(kit::terminal_link_overlay))
            .child({
                let grid = screen_grid
                    .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                        this.scroll_wheel(event, row_height, cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
                    .children(autoscroll_pill);
                grid.context_menu(Self::output_context_menu(terminal_actions.clone()))
                    .into_any_element()
            })
            .children(status_banner)
    }
}

/// `std::fs::canonicalize` returns verbatim `\\?\` paths on Windows, which
/// children (cmd.exe most visibly) cannot use as a working directory, so
/// downgrade the common drive-letter form back to a plain path.
/// Intentionally duplicated in `threadlane-daemon::terminal`: this leaf UI
/// crate takes no dependency on server-side crates, and the helper is small
/// enough to keep in sync by hand.
#[cfg(windows)]
fn simplified_cwd(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    match text.strip_prefix("\\\\?\\") {
        Some(rest) if rest.len() >= 2 && rest.as_bytes()[1] == b':' => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Shell programs to try, in order. On Windows, POSIX-style `SHELL` values
/// (e.g. `/bin/sh` inherited from Git Bash or MSYS) are not spawnable via
/// CreateProcess, so `SHELL`/`COMSPEC` only qualify when they point at a real
/// file — and even a real file can fail to spawn (a batch script or data
/// file), so callers must fall through the list on spawn errors.
/// Intentionally duplicated in `threadlane-daemon::terminal` — keep in sync.
fn shell_candidates() -> Vec<String> {
    if cfg!(windows) {
        let mut candidates = Vec::new();
        if let Some(shell) = std::env::var("SHELL")
            .ok()
            .filter(|shell| Path::new(shell).is_file())
        {
            candidates.push(shell);
        }
        if let Some(comspec) = std::env::var("COMSPEC")
            .ok()
            .filter(|comspec| !comspec.is_empty() && Path::new(comspec).is_file())
        {
            candidates.push(comspec);
        }
        candidates.push("cmd.exe".into());
        candidates
    } else {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
    }
}

fn spawn_shell(
    project: &PathBuf,
    rows: u16,
    cols: u16,
    output_tx: mpsc::SyncSender<Vec<u8>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
) -> Result<PtySession, Box<dyn std::error::Error + Send + Sync>> {
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    // Selection logic mirrors `threadlane-daemon::terminal` — keep in sync.
    let mut last_spawn_error = None;
    let mut child = None;
    for shell in shell_candidates() {
        let mut command = CommandBuilder::new(shell);
        #[cfg(windows)]
        command.cwd(simplified_cwd(project));
        #[cfg(not(windows))]
        command.cwd(project);
        command.env("TERM", "xterm-256color");
        if !cfg!(windows) {
            command.arg("-i");
        }
        match pair.slave.spawn_command(command) {
            Ok(spawned) => {
                child = Some(spawned);
                break;
            }
            Err(error) => last_spawn_error = Some(error),
        }
    }
    let child = child.ok_or_else(|| {
        format!(
            "could not spawn shell in {}: {}",
            project.display(),
            last_spawn_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no shell candidates".into())
        )
    })?;
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
        next_terminal_wake, selected_excerpt, selection_bounds,
        selection_present, should_paint_cursor, start_parser_worker, terminal_frame_policy,
        terminal_parse_budget_exhausted, ParserCommand, PtyEvent, TerminalWake,
        TERMINAL_FIND_RESCAN_INTERVAL, TERMINAL_PARSE_BUDGET_PER_FRAME,
    };

    /// Receives worker events until a `SearchResults` event arrives.
    async fn next_search_results(
        event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    ) -> PtyEvent {
        for _ in 0..200 {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for terminal search results")
                .expect("terminal worker channel closed");
            if matches!(event, PtyEvent::SearchResults { .. }) {
                return event;
            }
        }
        panic!("expected terminal search results");
    }

    /// Waits for the next painted frame; used to know previously sent output
    /// bytes were parsed before issuing a command that scans the buffer.
    async fn next_frame(
        event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    ) {
        for _ in 0..200 {
            let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .expect("timed out waiting for a terminal frame")
                .expect("terminal worker channel closed");
            if matches!(event, PtyEvent::Frame(_)) {
                return;
            }
        }
        panic!("expected a terminal frame");
    }

    #[test]
    fn terminal_selection_excerpt_follows_the_drag_endpoints() {
        let mut parser = vt100::Parser::new(4, 20, 0);
        parser.process(b"build failed\r\nsecond line");
        let screen = parser.screen();

        // No anchors — nothing selected.
        assert!(selected_excerpt(screen, None, None, 20).is_none());
        // A zero-width drag is not a selection.
        assert!(!selection_present(Some((0, 0)), Some((0, 0)), 20));
        assert!(selected_excerpt(screen, Some((0, 0)), Some((0, 0)), 20).is_none());
        // Forward and reversed drags cover the same cells: a forward
        // drag's head cell is exclusive, a reversed drag's anchor is
        // extended by one to compensate.
        assert!(selection_present(Some((0, 0)), Some((0, 12)), 20));
        assert_eq!(
            selected_excerpt(screen, Some((0, 0)), Some((0, 12)), 20).as_deref(),
            Some("build failed")
        );
        assert_eq!(
            selected_excerpt(screen, Some((0, 11)), Some((0, 0)), 20).as_deref(),
            Some("build failed")
        );
        // Cross-line selections keep the line break.
        assert_eq!(
            selected_excerpt(screen, Some((0, 6)), Some((1, 6)), 20).as_deref(),
            Some("failed\nsecond")
        );
        // Snapshots are recomputed against the current screen, never a
        // remembered copy — output landing after the drag changes the
        // next snapshot rather than being silently re-read as the old text.
        parser.process(b"\x1b[Hchanged");
        assert_ne!(
            selected_excerpt(parser.screen(), Some((0, 6)), Some((1, 6)), 20).as_deref(),
            Some("failed\nsecond")
        );
    }

    #[gpui::test]
    fn screen_resets_invalidate_a_selection_instead_of_reinterpreting_it(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext as _;
        let terminal = cx.update(|cx| {
            cx.new(|cx| super::TerminalView::new_for_test(std::path::PathBuf::from("/tmp"), cx))
        });
        terminal.update(cx, |terminal, cx| {
            let mut parser = vt100::Parser::new(4, 20, 0);
            parser.process(b"select me");
            terminal.apply_event(super::PtyEvent::Frame(super::TerminalFrame {
                link_epoch: 0,
                links: Vec::new(),
                screen: parser.screen().clone(),
                scrollback: 0,
                scrollback_len: 0,
                alt_screen: false,
            }));
            terminal.selection_anchor = Some((0, 0));
            terminal.selection_head = Some((0, 9));
            assert!(terminal.has_selection());
            let snapshot = terminal.selection_snapshot().expect("selection snapshot");
            assert_eq!(snapshot.text, "select me");
            assert_eq!(snapshot.launched_in, std::path::PathBuf::from("/tmp"));
            let status = terminal.selection_status().expect("selection status");
            assert!(status.has_text);
            assert_eq!(status.excerpt_len, 9);
            // Geometry changes clear the selection rather than re-mapping
            // the old cell coordinates onto the resized screen.
            terminal.resize(8, 40, cx);
            assert!(!terminal.has_selection());
            assert!(terminal.selection_snapshot().is_none());
            assert!(terminal.selection_status().is_none());
        });
        terminal.update(cx, |terminal, cx| {
            // Clearing the screen invalidates the selection the same way:
            // the retained coordinates must not be re-read against a
            // blank or restarted screen.
            terminal.selection_anchor = Some((0, 0));
            terminal.selection_head = Some((0, 9));
            assert!(terminal.has_selection());
            terminal.clear(cx);
            assert!(!terminal.has_selection());
            assert!(terminal.selection_snapshot().is_none());
        });
    }

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
                link_epoch: 0,
                links: Vec::new(),
                screen: parser.screen().clone(),
                scrollback: 0,
                scrollback_len: 0,
                alt_screen: false,
            }))
            .unwrap();
        parser.process(b"\rnew");
        event_tx
            .send(PtyEvent::Frame(super::TerminalFrame {
                link_epoch: 0,
                links: Vec::new(),
                screen: parser.screen().clone(),
                scrollback: 0,
                scrollback_len: 0,
                alt_screen: false,
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

    #[tokio::test]
    async fn parser_worker_find_covers_retained_scrollback_and_reveals_newest() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        let mut input = String::from("needle deep\r\n");
        for line in 0..40 {
            input.push_str(&format!("line {line}\r\n"));
        }
        input.push_str("needle live");
        output_tx.send(input.into_bytes()).unwrap();
        next_frame(&mut event_rx).await;

        command_tx
            .send(ParserCommand::Find { generation: 7, query: Some("needle".to_string()) })
            .unwrap();
        let PtyEvent::SearchResults { generation, hits, total, scrollback_len, alt_screen, revealed, .. } =
            next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert_eq!(generation, 7);
        assert_eq!(total, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].absolute_row, 0);
        assert!(hits[0].excerpt.contains("needle deep"));
        assert_eq!(scrollback_len, 38);
        assert!(!alt_screen);
        // A settled query reveals the newest match — here the live row.
        assert_eq!(revealed, Some(1));
    }

    #[tokio::test]
    async fn parser_worker_reveal_moves_the_view_to_a_scrollback_match() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        let mut input = String::from("needle deep\r\n");
        for line in 0..40 {
            input.push_str(&format!("line {line}\r\n"));
        }
        output_tx.send(input.into_bytes()).unwrap();
        next_frame(&mut event_rx).await;

        command_tx
            .send(ParserCommand::Find { generation: 1, query: Some("needle".to_string()) })
            .unwrap();
        let _ = next_search_results(&mut event_rx).await;

        // The worker resolves the hit by identity in a fresh scan.
        command_tx
            .send(ParserCommand::RevealMatch {
                generation: 1,
                row: 0,
                excerpt: "needle deep".to_string(),
            })
            .unwrap();
        let PtyEvent::SearchResults { revealed, .. } = next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert_eq!(revealed, Some(0));

        // The reveal moved the parser's view into deep scrollback; the next
        // painted frame carries that offset to the view.
        let mut saw_deep_frame = false;
        for _ in 0..200 {
            let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
                .await
                .expect("timed out waiting for a terminal frame")
                .expect("terminal worker channel closed");
            if let PtyEvent::Frame(frame) = event {
                if frame.scrollback > 0 {
                    saw_deep_frame = true;
                    break;
                }
            }
        }
        assert!(saw_deep_frame, "expected a frame scrolled into scrollback");
    }

    #[tokio::test]
    async fn parser_worker_stale_reveal_generation_is_ignored() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        command_tx
            .send(ParserCommand::Find { generation: 4, query: Some("needle".to_string()) })
            .unwrap();
        let _ = next_search_results(&mut event_rx).await;

        command_tx
            .send(ParserCommand::RevealMatch {
                generation: 999,
                row: 0,
                excerpt: "needle".to_string(),
            })
            .unwrap();
        // No rescan is scheduled for a mismatched generation: draining the
        // channel for a while must not surface another SearchResults.
        let saw_results = tokio::time::timeout(Duration::from_millis(400), async {
            loop {
                match event_rx.recv().await {
                    Some(PtyEvent::SearchResults { .. }) => break true,
                    Some(_) | None => {}
                }
            }
        })
        .await;
        assert!(saw_results.is_err(), "stale generation produced results");
    }

    #[tokio::test]
    async fn parser_worker_reveal_refuses_a_stale_hit_identity() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle one\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        command_tx
            .send(ParserCommand::Find { generation: 8, query: Some("needle".to_string()) })
            .unwrap();
        let _ = next_search_results(&mut event_rx).await;

        // A clear + rewrite removes the hit the view navigated to. The
        // identity can no longer be confirmed, so the worker must NOT reveal
        // a different line in its place.
        command_tx.send(ParserCommand::Clear).unwrap();
        let _ = next_search_results(&mut event_rx).await;
        output_tx.send(b"needle other\r\n".to_vec()).unwrap();
        let _ = next_search_results(&mut event_rx).await;

        command_tx
            .send(ParserCommand::RevealMatch {
                generation: 8,
                row: 0,
                excerpt: "needle one".to_string(),
            })
            .unwrap();
        let PtyEvent::SearchResults { revealed, .. } = next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert_eq!(revealed, None);
    }

    #[tokio::test]
    async fn parser_worker_suspends_and_resumes_find_on_alternate_screen() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle top\r\nmore\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        output_tx.send(b"\x1b[?1049hfull screen app".to_vec()).unwrap();
        next_frame(&mut event_rx).await;

        command_tx
            .send(ParserCommand::Find { generation: 2, query: Some("needle".to_string()) })
            .unwrap();
        let PtyEvent::SearchResults { alt_screen, hits, total, .. } =
            next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert!(alt_screen);
        assert!(hits.is_empty());
        assert_eq!(total, 0);

        // Leaving the alternate screen rescans and reports the restored
        // primary buffer on the next coalesced rescan tick.
        output_tx.send(b"\x1b[?1049l".to_vec()).unwrap();
        let PtyEvent::SearchResults { alt_screen, total, .. } =
            next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert!(!alt_screen);
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn parser_worker_refreshes_results_as_output_arrives() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle one\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        command_tx
            .send(ParserCommand::Find { generation: 3, query: Some("needle".to_string()) })
            .unwrap();
        let PtyEvent::SearchResults { total, revealed, .. } =
            next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert_eq!(total, 1);
        assert_eq!(revealed, Some(0));

        // New output updates the result list via a coalesced rescan without
        // moving the reading position (revealed is None on refreshes).
        output_tx.send(b"needle two\r\n".to_vec()).unwrap();
        let PtyEvent::SearchResults { total, revealed, .. } =
            next_search_results(&mut event_rx).await
        else {
            panic!("expected search results");
        };
        assert_eq!(total, 2);
        assert_eq!(revealed, None);
    }

    #[tokio::test]
    async fn parser_worker_retains_output_when_find_resizes_the_grid() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(6, 40, event_tx).unwrap();
        output_tx
            .send(b"needle first\r\nneedle second\r\nneedle third\r\nprompt> ".to_vec())
            .unwrap();
        next_frame(&mut event_rx).await;
        command_tx
            .send(ParserCommand::Find {
                generation: 7,
                query: Some("needle".to_string()),
            })
            .unwrap();
        let _ = next_search_results(&mut event_rx).await;

        for rows in [2, 6] {
            command_tx.send(ParserCommand::Resize(rows, 40)).unwrap();
            let PtyEvent::SearchResults { total, .. } = next_search_results(&mut event_rx).await else {
                panic!("expected search results after resize");
            };
            assert_eq!(
                total, 3,
                "Find must retain all output after resizing to {rows} rows"
            );
        }
    }

    #[test]
    fn parser_resize_preserves_ansi_and_an_incomplete_escape_sequence() {
        let mut parser = vt100::Parser::new(6, 40, 20);
        parser.process(b"\x1b[36mfirst\x1b[0m\r\nsecond\r\nthird\r\n\x1b[");
        parser.screen_mut().set_size(2, 40);
        parser.process(b"31mred");
        assert_eq!(parser.screen().contents(), "third\nred");
        assert_eq!(
            parser.screen().cell(1, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );

        parser.screen_mut().set_size(6, 40);
        assert_eq!(parser.screen().contents(), "first\nsecond\nthird\nred");
        assert_eq!(parser.screen().cursor_position(), (3, 3));
        assert_eq!(
            parser.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Idx(6)
        );
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[tokio::test]
    async fn parser_worker_invalidates_results_on_clear_and_resize() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        command_tx
            .send(ParserCommand::Find { generation: 5, query: Some("needle".to_string()) })
            .unwrap();
        let _ = next_search_results(&mut event_rx).await;

        command_tx.send(ParserCommand::Clear).unwrap();
        let PtyEvent::SearchResults { total, .. } = next_search_results(&mut event_rx).await else {
            panic!("expected search results");
        };
        assert_eq!(total, 0);

        output_tx.send(b"needle again\r\n".to_vec()).unwrap();
        let _ = next_search_results(&mut event_rx).await;
        command_tx.send(ParserCommand::Resize(2, 8)).unwrap();
        let PtyEvent::SearchResults { total, .. } = next_search_results(&mut event_rx).await else {
            panic!("expected search results");
        };
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn parser_worker_keeps_retained_output_searchable_after_output_ends() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle retained\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        drop(output_tx);
        // Let the worker observe the output channel closing.
        tokio::time::sleep(Duration::from_millis(100)).await;

        command_tx
            .send(ParserCommand::Find { generation: 6, query: Some("needle".to_string()) })
            .unwrap();
        let PtyEvent::SearchResults { total, .. } = next_search_results(&mut event_rx).await else {
            panic!("expected search results");
        };
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn parser_worker_does_not_scan_while_find_is_closed() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (output_tx, command_tx) = start_parser_worker(4, 16, event_tx).unwrap();

        output_tx.send(b"needle\r\n".to_vec()).unwrap();
        next_frame(&mut event_rx).await;
        command_tx
            .send(ParserCommand::Find { generation: 1, query: Some("needle".to_string()) })
            .unwrap();
        let _ = next_search_results(&mut event_rx).await;

        command_tx
            .send(ParserCommand::Find { generation: 1, query: None })
            .unwrap();
        // Keep output flowing well past the rescan interval: no search may
        // run after the strip closed.
        let window = TERMINAL_FIND_RESCAN_INTERVAL + Duration::from_millis(400);
        let mut produced_results = false;
        let _ = tokio::time::timeout(window, async {
            for _ in 0..8 {
                output_tx.send(b"needle more\r\n".to_vec()).unwrap();
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            loop {
                match event_rx.recv().await {
                    Some(PtyEvent::SearchResults { .. }) => produced_results = true,
                    Some(_) => {}
                    None => break,
                }
            }
        })
        .await;
        assert!(!produced_results);
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
