//! Shared ANSI cell presentation and geometry; hosts own parsing, PTY I/O and frame identity.
use gpui::{prelude::*, *};
use gpui_component::{ActiveTheme, ThemeMode};
use std::collections::HashMap;

/// Raster-bound terminal metrics shared by paint, resizing and pointer hit testing.
pub const TERMINAL_FONT_SIZE: f32 = 13.0;
pub const TERMINAL_LINE_HEIGHT: f32 = 1.35;
pub const TERMINAL_COMPACT_LINE_HEIGHT: f32 = 1.15;
pub const TERMINAL_CELL_WIDTH_FALLBACK: f32 = 7.8;

pub fn terminal_line_height(compact: bool) -> f32 {
    if compact {
        TERMINAL_COMPACT_LINE_HEIGHT
    } else {
        TERMINAL_LINE_HEIGHT
    }
}

#[derive(Clone)]
pub struct TerminalTextMetrics {
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    cell_width: f32,
}

impl TerminalTextMetrics {
    pub fn measure(font_size: f32, compact: bool, window: &Window, cx: &App) -> Self {
        let font_family = cx.theme().mono_font_family.clone();
        let font_id = window
            .text_system()
            .resolve_font(&font(font_family.clone()));
        let width = window
            .text_system()
            .layout_width(font_id, px(font_size), '0')
            .as_f32();
        Self {
            font_family,
            font_size,
            line_height: terminal_line_height(compact),
            cell_width: if width > 0.0 {
                width
            } else {
                TERMINAL_CELL_WIDTH_FALLBACK
            },
        }
    }
    pub fn cell_width(&self) -> f32 {
        self.cell_width
    }
    pub fn row_height(&self) -> f32 {
        self.font_size * self.line_height
    }
    pub fn inset(&self, window: &Window) -> f32 {
        // Resolve during prepaint: cached child views can survive a Root zoom change.
        window.rem_size().as_f32() * 0.75
    }
    pub fn grid_size(&self, size: Size<Pixels>, window: &Window) -> (u16, u16) {
        (
            ((size.height.as_f32() - self.inset(window) * 2.0) / self.row_height())
                .floor()
                .max(1.0) as u16,
            ((size.width.as_f32() - self.inset(window) * 2.0) / self.cell_width)
                .floor()
                .max(1.0) as u16,
        )
    }
}

/// The same viewport geometry serves selection drags and strict link hit testing.
#[derive(Debug)]
pub struct TerminalGridGeometry {
    bounds: Bounds<Pixels>,
    rows: u16,
    cols: u16,
    cell_width: f32,
    row_height: f32,
    inset: f32,
}
impl TerminalGridGeometry {
    pub fn new(
        bounds: Bounds<Pixels>,
        size: (u16, u16),
        cell_width: f32,
        row_height: f32,
        inset: f32,
    ) -> Self {
        Self {
            bounds,
            rows: size.0,
            cols: size.1,
            cell_width,
            row_height,
            inset,
        }
    }
    pub fn cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        if self.rows == 0 || self.cols == 0 || self.cell_width <= 0.0 || self.row_height <= 0.0 {
            return None;
        }
        let x = ((position.x - self.bounds.left()).as_f32() - self.inset) / self.cell_width;
        let y = ((position.y - self.bounds.top()).as_f32() - self.inset) / self.row_height;
        Some((
            y.floor().clamp(0.0, f32::from(self.rows - 1)) as u16,
            x.floor().clamp(0.0, f32::from(self.cols - 1)) as u16,
        ))
    }
    pub fn link_cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let x = (position.x - self.bounds.left()).as_f32() - self.inset;
        let y = (position.y - self.bounds.top()).as_f32() - self.inset;
        if !self.bounds.contains(&position)
            || x < 0.0
            || y < 0.0
            || x >= f32::from(self.cols) * self.cell_width
            || y >= f32::from(self.rows) * self.row_height
        {
            return None;
        }
        self.cell_at(position)
    }
}

pub fn terminal_selection_bounds(
    anchor: (u16, u16),
    head: (u16, u16),
    cols: u16,
) -> Option<((u16, u16), (u16, u16))> {
    if anchor == head {
        return None;
    }
    if head < anchor {
        Some((head, (anchor.0, anchor.1.saturating_add(1).min(cols))))
    } else {
        Some((anchor, head))
    }
}
pub fn terminal_selection_present(
    anchor: Option<(u16, u16)>,
    head: Option<(u16, u16)>,
    cols: u16,
) -> bool {
    match (anchor, head) {
        (Some(anchor), Some(head)) => terminal_selection_bounds(anchor, head, cols).is_some(),
        _ => false,
    }
}
pub fn terminal_selected_excerpt(
    screen: &vt100::Screen,
    anchor: Option<(u16, u16)>,
    head: Option<(u16, u16)>,
    cols: u16,
) -> Option<String> {
    let (start, end) = terminal_selection_bounds(anchor?, head?, cols)?;
    Some(screen.contents_between(start.0, start.1, end.0, end.1))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalSelectionGranularity {
    Word,
    Line,
}

/// Returns the range for a word or line drag while preserving the direction
/// of the drag in the existing anchor/head selection representation.
pub fn terminal_granular_selection(
    screen: &vt100::Screen,
    origin: (u16, u16),
    head: (u16, u16),
    cols: u16,
    granularity: TerminalSelectionGranularity,
) -> Option<((u16, u16), (u16, u16))> {
    let origin_range = match granularity {
        TerminalSelectionGranularity::Word => terminal_word_bounds(screen, origin, cols)?,
        TerminalSelectionGranularity::Line => terminal_line_bounds(origin, head, cols),
    };
    let head_range = match granularity {
        TerminalSelectionGranularity::Word => terminal_word_bounds(screen, head, cols)?,
        TerminalSelectionGranularity::Line => terminal_line_bounds(origin, head, cols),
    };
    let origin_ix = usize::from(origin.0) * usize::from(cols) + usize::from(origin.1);
    let head_ix = usize::from(head.0) * usize::from(cols) + usize::from(head.1);
    let (start, end, reversed) = if head_ix >= origin_ix {
        (origin_range.0, head_range.1, false)
    } else {
        (head_range.0, origin_range.1, true)
    };
    if start == end {
        return None;
    }
    if reversed {
        let anchor = (end.0, end.1.saturating_sub(1));
        Some((anchor, start))
    } else {
        Some((start, end))
    }
}

pub fn terminal_word_bounds(
    screen: &vt100::Screen,
    (row, col): (u16, u16),
    cols: u16,
) -> Option<((u16, u16), (u16, u16))> {
    if row >= screen.size().0 || col >= cols {
        return None;
    }
    let col = if screen
        .cell(row, col)
        .is_some_and(vt100::Cell::is_wide_continuation)
    {
        col.saturating_sub(1)
    } else {
        col
    };
    let class = terminal_word_class(screen.cell(row, col).map(vt100::Cell::contents));
    let mut start = col;
    while start > 0 && terminal_word_class_at(screen, row, start - 1) == class {
        if screen
            .cell(row, start - 1)
            .is_some_and(vt100::Cell::is_wide_continuation)
        {
            start = start.saturating_sub(2);
        } else {
            start -= 1;
        }
    }
    let mut end = col.saturating_add(1);
    if screen.cell(row, col).is_some_and(vt100::Cell::is_wide) {
        end = end.saturating_add(1);
    }
    while end < cols && terminal_word_class_at(screen, row, end) == class {
        end += if screen.cell(row, end).is_some_and(vt100::Cell::is_wide) {
            2
        } else {
            1
        };
    }
    Some(((row, start), (row, end.min(cols))))
}

fn terminal_word_class_at(screen: &vt100::Screen, row: u16, col: u16) -> TerminalWordClass {
    let cell = screen.cell(row, col);
    let contents = if cell.is_some_and(vt100::Cell::is_wide_continuation) && col > 0 {
        screen.cell(row, col - 1).map(vt100::Cell::contents)
    } else {
        cell.map(vt100::Cell::contents)
    };
    terminal_word_class(contents)
}

pub fn terminal_line_bounds(
    origin: (u16, u16),
    head: (u16, u16),
    cols: u16,
) -> ((u16, u16), (u16, u16)) {
    let start = origin.0.min(head.0);
    let end = origin.0.max(head.0);
    ((start, 0), (end, cols))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalWordClass {
    Whitespace,
    Word,
    Punctuation,
}

fn terminal_word_class(contents: Option<&str>) -> TerminalWordClass {
    let character = contents.and_then(|text| text.chars().next()).unwrap_or(' ');
    if character.is_whitespace() {
        TerminalWordClass::Whitespace
    } else if character.is_alphanumeric() || character == '_' {
        TerminalWordClass::Word
    } else {
        TerminalWordClass::Punctuation
    }
}

type GridLayoutCallback = Box<dyn FnOnce(Bounds<Pixels>, &mut Window, &mut App)>;

/// A controlled viewport over a host-supplied parsed terminal frame.
pub struct TerminalGrid<'a> {
    id: ElementId,
    screen: &'a vt100::Screen,
    metrics: TerminalTextMetrics,
    selection: Option<((u16, u16), (u16, u16))>,
    cursor: bool,
    find_cue: Option<u16>,
    links: Vec<(&'a str, &'a [(u16, u16)])>,
    on_layout: Option<GridLayoutCallback>,
}
impl<'a> TerminalGrid<'a> {
    pub fn new(
        id: impl Into<ElementId>,
        screen: &'a vt100::Screen,
        metrics: TerminalTextMetrics,
    ) -> Self {
        Self {
            id: id.into(),
            screen,
            metrics,
            selection: None,
            cursor: false,
            find_cue: None,
            links: Vec::new(),
            on_layout: None,
        }
    }
    pub fn selection(mut self, anchor: Option<(u16, u16)>, head: Option<(u16, u16)>) -> Self {
        self.selection = anchor
            .zip(head)
            .and_then(|(a, h)| terminal_selection_bounds(a, h, self.screen.size().1));
        self
    }
    pub fn cursor(mut self, visible: bool) -> Self {
        self.cursor = visible;
        self
    }
    pub fn find_cue(mut self, row: Option<u16>) -> Self {
        self.find_cue = row;
        self
    }
    pub fn links(mut self, links: impl IntoIterator<Item = (&'a str, &'a [(u16, u16)])>) -> Self {
        self.links = links.into_iter().collect();
        self
    }
    /// Observe the viewport border box, independent of output rows and padding.
    pub fn on_layout(
        mut self,
        callback: impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_layout = Some(Box::new(callback));
        self
    }
    pub fn render(self, cx: &App) -> Stateful<Div> {
        let theme = cx.theme().colors;
        let light = cx.theme().mode == ThemeMode::Light;
        let (rows, cols) = self.screen.size();
        let (cursor_row, cursor_col) = self.screen.cursor_position();
        let paint_cursor = self.cursor && !self.screen.hide_cursor();
        let link_cells: HashMap<_, _> = self
            .links
            .iter()
            .flat_map(|(url, cells)| cells.iter().map(move |cell| (*cell, *url)))
            .collect();
        let mut lines = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let max_col = (0..cols)
                .rev()
                .find(|&col| {
                    self.screen.cell(row, col).is_some_and(|cell| {
                        (!cell.contents().is_empty() && cell.contents() != " ")
                            || (row == cursor_row && col == cursor_col)
                    })
                })
                .map_or(0, |col| col + 1);
            let mut spans = Vec::new();
            let mut text = String::new();
            let mut current = None;
            let mut span_col = 0;
            for col in 0..max_col {
                let Some(cell) = self.screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let selected = self
                    .selection
                    .is_some_and(|(start, end)| (row, col) >= start && (row, col) < end);
                let cursor = paint_cursor && row == cursor_row && col == cursor_col;
                let fg = if selected {
                    theme.accent_foreground
                } else if cursor {
                    theme.background
                } else {
                    ansi_to_hsla(cell.fgcolor(), theme.foreground, light)
                        .unwrap_or(theme.foreground)
                };
                let bg = if selected {
                    Some(theme.accent)
                } else if cursor {
                    Some(theme.primary)
                } else {
                    match cell.bgcolor() {
                        vt100::Color::Default => None,
                        other => ansi_to_hsla(other, theme.background, light),
                    }
                };
                let style = (
                    fg,
                    bg,
                    cell.bold(),
                    cursor,
                    link_cells.get(&(row, col)).copied(),
                );
                if current != Some(style) {
                    if let Some(previous) = current {
                        spans.push(render_span(
                            row,
                            span_col,
                            std::mem::take(&mut text),
                            previous,
                        ));
                    }
                    span_col = col;
                    current = Some(style);
                }
                text.push_str(if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                });
            }
            if let Some(style) = current {
                spans.push(render_span(row, span_col, text, style));
            }
            if spans.is_empty() {
                spans.push(div().child(" ").into_any_element());
            }
            lines.push(
                div()
                    // Raster typography belongs to output rows. Status controls
                    // and menus keep the host's interface font and text scale.
                    .font_family(self.metrics.font_family.clone())
                    .text_size(px(self.metrics.font_size))
                    .line_height(relative(self.metrics.line_height))
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(self.metrics.row_height()))
                    .when(self.find_cue == Some(row), |line| {
                        line.bg(theme.accent.opacity(0.18))
                    })
                    .children(spans),
            );
        }
        div()
            .id(self.id)
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .p_3()
            .cursor_text()
            .children(lines)
            .children(self.on_layout.map(|callback| {
                // Explicit anchors avoid the static position after the last row.
                canvas(
                    move |bounds, window, cx| callback(bounds, window, cx),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .size_full()
            }))
    }
}

fn render_span(
    row: u16,
    col: u16,
    text: String,
    style: (Hsla, Option<Hsla>, bool, bool, Option<&str>),
) -> AnyElement {
    let (fg, bg, bold, _, link) = style;
    let span = div()
        .child(text)
        .text_color(fg)
        .when_some(bg, |span, bg| span.bg(bg))
        .when(bold, |span| span.font_weight(FontWeight::BOLD));
    if let Some(url) = link {
        let gesture = if cfg!(target_os = "macos") {
            "Cmd-click to open in Threadlane browser"
        } else {
            "Ctrl-click to open in default browser"
        };
        let tooltip = format!("{url} — {gesture}");
        span.id((
            "terminal-grid-link",
            row as usize * (u16::MAX as usize + 1) + col as usize,
        ))
        .underline()
        .cursor_pointer()
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .into_any_element()
    } else {
        span.into_any_element()
    }
}

// Audited data-color exception: ANSI/RGB colors are terminal payload, not product UI tokens.
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

#[cfg(test)]
mod tests {
    use super::{ansi_index_to_hsla, rgb_to_hsla};
    #[test]
    fn selection_copy_preserves_last_column_and_reversed_drags() {
        let mut parser = vt100::Parser::new(2, 4, 0);
        parser.process(b"abcd\r\nefgh");
        assert_eq!(
            super::terminal_selected_excerpt(parser.screen(), Some((0, 3)), Some((0, 0)), 4)
                .as_deref(),
            Some("abcd")
        );
        assert_eq!(
            super::terminal_selected_excerpt(parser.screen(), Some((0, 0)), Some((1, 4)), 4)
                .as_deref(),
            Some("abcd\nefgh")
        );
    }

    #[test]
    fn geometry_clamps_drags_but_rejects_link_padding() {
        let bounds = gpui::Bounds::new(
            gpui::point(gpui::px(20.), gpui::px(30.)),
            gpui::size(gpui::px(120.), gpui::px(80.)),
        );
        for inset in [9., 12., 15.] {
            let geometry = super::TerminalGridGeometry::new(bounds, (3, 10), 8., 18., inset);
            let first = gpui::point(
                bounds.left() + gpui::px(inset + 1.),
                bounds.top() + gpui::px(inset + 1.),
            );
            assert_eq!(geometry.cell_at(first), Some((0, 0)));
            assert_eq!(geometry.link_cell_at(first), Some((0, 0)));
            assert_eq!(geometry.link_cell_at(bounds.origin), None);
            assert_eq!(geometry.cell_at(bounds.origin), Some((0, 0)));
            let outside = gpui::point(gpui::px(400.), gpui::px(400.));
            assert_eq!(geometry.cell_at(outside), Some((2, 9)));
            assert_eq!(geometry.link_cell_at(outside), None);
        }
    }

    #[test]
    fn xterm_color_cube_uses_standard_channel_levels() {
        assert_eq!(ansi_index_to_hsla(17, false), rgb_to_hsla(0, 0, 95));
        assert_eq!(ansi_index_to_hsla(67, false), rgb_to_hsla(95, 135, 175));
    }

    #[test]
    fn granular_selection_expands_words_lines_and_reverse_drags() {
        let mut parser = vt100::Parser::new(3, 12, 0);
        parser.process(b"foo.bar baz\nsecond row");
        let screen = parser.screen();
        assert_eq!(
            super::terminal_word_bounds(screen, (0, 1), 12),
            Some(((0, 0), (0, 3)))
        );
        assert_eq!(
            super::terminal_word_bounds(screen, (0, 3), 12),
            Some(((0, 3), (0, 4)))
        );
        let mut wide_parser = vt100::Parser::new(1, 8, 0);
        wide_parser.process("界foo".as_bytes());
        assert_eq!(
            super::terminal_word_bounds(wide_parser.screen(), (0, 1), 8),
            Some(((0, 0), (0, 5)))
        );
        assert_eq!(
            super::terminal_granular_selection(
                screen,
                (0, 1),
                (0, 5),
                12,
                super::TerminalSelectionGranularity::Word,
            ),
            Some(((0, 0), (0, 7)))
        );
        assert_eq!(
            super::terminal_granular_selection(
                screen,
                (0, 5),
                (0, 1),
                12,
                super::TerminalSelectionGranularity::Word,
            ),
            Some(((0, 6), (0, 0)))
        );
        assert_eq!(
            super::terminal_granular_selection(
                screen,
                (0, 1),
                (1, 2),
                12,
                super::TerminalSelectionGranularity::Line,
            ),
            Some(((0, 0), (1, 12)))
        );
    }
}
