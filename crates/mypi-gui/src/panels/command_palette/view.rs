//! CommandTextInput widget implementation and completion popup view.

use super::state::CommandInfo;
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use unicode_segmentation::UnicodeSegmentation;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.components.MypiCommandTextInputBase = #(MypiCommandTextInput::register_widget(vm))

    mod.components.MypiCommandTextInput = mod.components.MypiCommandTextInputBase{
        flow: Down
        height: Fit

        popup := RoundedView{
            flow: Down
            width: Fill
            height: Fit
            visible: false

            draw_bg +: {
                color: #x1f232b
                border_size: 1.0
                border_color: #x3a424e
                border_radius: 8.0

                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)

                    sdf.box_all(
                        0.0
                        0.0
                        self.rect_size.x
                        self.rect_size.y
                        self.border_radius
                        self.border_radius
                        self.border_radius
                        self.border_radius
                    )
                    sdf.fill(self.border_color)

                    sdf.box_all(
                        self.border_size
                        self.border_size
                        self.rect_size.x - self.border_size * 2.0
                        self.rect_size.y - self.border_size * 2.0
                        self.border_radius - self.border_size
                        self.border_radius - self.border_size
                        self.border_radius - self.border_size
                        self.border_radius - self.border_size
                    )
                    sdf.fill(self.color)

                    return sdf.result
                }
            }

            header_view := View{
                width: Fill
                height: 0
                visible: false
                draw_bg +: {
                    color: theme.color_fg_app
                    top_radius: instance(theme.corner_radius)
                    border_color: instance(theme.color_bevel)
                    border_width: instance(theme.beveling)
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box_all(
                            0.0
                            0.0
                            self.rect_size.x
                            self.rect_size.y
                            self.top_radius
                            self.top_radius
                            1.0
                            1.0
                        )
                        sdf.fill(self.color)
                        return sdf.result
                    }
                }

                header_label := Label{
                    draw_text +: {
                        color: theme.color_label_inner
                        text_style: theme.font_regular{
                            font_size: theme.font_size_4
                        }
                    }
                }
            }

            search_input_wrapper := RoundedView{
                height: Fit
                search_input := TextInput{
                    width: Fill
                    height: Fit
                }
            }

            list := PortalList{
                width: Fill
                height: 208
                flow: Down
                drag_scrolling: true

                CommandItem := View {
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 7
                    align: Align{y: 0.5}
                    padding: Inset{left: 10 top: 6 right: 12 bottom: 6}
                    cursor: MouseCursor.Hand
                    show_bg: true
                    draw_bg +: {
                        color: #x00000000
                        border_size: 0.0
                        border_radius: 5.0
                    }
                    active_marker := Label {
                        width: 10
                        height: Fit
                        text: ""
                        draw_text +: {
                            color: #x8fb3ff
                            text_style: theme.font_bold { font_size: 12.0 }
                        }
                    }
                    command_content := View {
                        width: Fill
                        height: Fit
                        flow: Down
                        spacing: 1
                        cmd_name := Label {
                            width: Fill
                            height: Fit
                            text: ""
                            draw_text +: {
                                color: #xdde3ea
                                text_style: theme.font_code { font_size: 10.5 }
                            }
                        }
                        cmd_desc := Label {
                            width: Fill
                            height: Fit
                            text: ""
                            draw_text +: {
                                color: #x8b93a0
                                text_style +: { font_size: 9.0 }
                            }
                        }
                    }
                }
            }
        }

        persistent := RoundedView{
            flow: Down
            width: Fill
            height: Fit
            top := View{ width: Fill, height: Fit }
            center := RoundedView{
                flow: Right
                width: Fill
                height: Fit
                left := View{ width: Fit, height: Fit }
                text_input := TextInput{ width: Fill, height: Fit }
                right := View{ width: Fit, height: Fit }
            }
            bottom := View{ width: Fill, height: Fit }
        }
    }
}

#[derive(Debug, Copy, Clone, Default)]
enum InternalAction {
    ShouldBuildItems,
    ItemSelected,
    #[default]
    None,
}

#[derive(Script, ScriptHook, Widget)]
pub struct MypiCommandTextInput {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    deref: View,

    #[live]
    pub trigger: Option<String>,

    #[live]
    pub inline_search: bool,

    #[live]
    pub color_focus: Vec4f,

    #[live]
    pub color_hover: Vec4f,

    #[rust]
    is_search_input_focus_pending: bool,

    #[rust]
    is_text_input_focus_pending: bool,

    #[rust]
    keyboard_focus_index: Option<usize>,

    #[rust]
    pointer_hover_index: Option<usize>,

    #[rust]
    items: Vec<CommandInfo>,

    #[rust]
    last_selected_command: String,

    #[rust]
    trigger_position: Option<usize>,

    #[rust]
    prev_cursor_position: usize,
}

impl Widget for MypiCommandTextInput {
    fn set_text(&mut self, cx: &mut Cx, v: &str) {
        self.text_input_ref(cx).set_text(cx, v);
    }

    fn text(&self) -> String {
        String::new()
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.ensure_popup_consistent(cx);

        while let Some(item) = self.deref.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, self.items.len());

                while let Some(item_id) = list.next_visible_item(cx) {
                    let Some(command) = self.items.get(item_id) else {
                        continue;
                    };
                    let mut item_widget = list.item(cx, item_id, id!(CommandItem));
                    item_widget
                        .label(cx, ids!(cmd_name))
                        .set_text(cx, &format!("/{}", command.name));
                    item_widget
                        .label(cx, ids!(cmd_desc))
                        .set_text(cx, &command.description);

                    let is_active = Some(item_id) == self.keyboard_focus_index;
                    item_widget
                        .label(cx, ids!(active_marker))
                        .set_text(cx, if is_active { "›" } else { "" });

                    let color = if is_active {
                        self.color_focus
                    } else if Some(item_id) == self.pointer_hover_index {
                        self.color_hover
                    } else {
                        Vec4f::all(0.)
                    };
                    let name_color = if is_active {
                        vec4(0.95, 0.97, 1.0, 1.0)
                    } else {
                        vec4(0.87, 0.89, 0.92, 1.0)
                    };
                    let mut name_label = item_widget.label(cx, ids!(cmd_name));
                    script_apply_eval!(cx, name_label, {
                        draw_text +: { color: #(name_color) }
                    });
                    script_apply_eval!(cx, item_widget, {
                        draw_bg +: { color: #(color) }
                    });
                    item_widget.draw_all_unscoped(cx);
                }
            }
        }

        if self.is_search_input_focus_pending {
            self.is_search_input_focus_pending = false;
            self.search_input_ref(cx).set_key_focus(cx);
        }

        if self.is_text_input_focus_pending {
            self.is_text_input_focus_pending = false;
            self.text_input_ref(cx).set_key_focus(cx);
        }

        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::KeyDown(key_event) = event {
            if self.view(cx, ids!(popup)).visible() {
                let handled = match key_event.key_code {
                    KeyCode::ArrowDown => {
                        self.on_keyboard_move(cx, 1);
                        true
                    }
                    KeyCode::ArrowUp => {
                        self.on_keyboard_move(cx, -1);
                        true
                    }
                    KeyCode::ReturnKey | KeyCode::Tab => {
                        self.on_keyboard_controller_input_submit(cx, scope);
                        true
                    }
                    KeyCode::Escape => {
                        self.is_text_input_focus_pending = true;
                        self.hide_popup(cx);
                        self.redraw(cx);
                        true
                    }
                    _ => false,
                };

                if handled {
                    return;
                }
            }
        }

        self.deref.handle_event(cx, event, scope);

        if cx.has_key_focus(self.text_input_ref(cx).area()) {
            if let Event::TextInput(input_event) = event {
                self.on_text_inserted(cx, scope, &input_event.input);
            }

            if self.inline_search {
                if let Some(trigger_pos) = self.trigger_position {
                    let current_pos = get_head(&self.text_input_ref(cx));
                    let current_search = self.search_text(cx);

                    if current_pos < trigger_pos || graphemes(&current_search).any(is_whitespace) {
                        self.hide_popup(cx);
                        self.redraw(cx);
                    } else if self.prev_cursor_position != current_pos {
                        cx.widget_action(self.widget_uid(), InternalAction::ShouldBuildItems);
                        self.ensure_popup_consistent(cx);
                    }
                }
            }
        }

        if let Event::Actions(actions) = event {
            let mut selected_by_click = None;
            let mut should_redraw = false;
            let list = self.portal_list(cx, ids!(list));

            for (idx, item) in list.items_with_actions(actions) {
                let item = item.as_view();

                if item
                    .finger_down(actions)
                    .map(|fe| fe.tap_count == 1)
                    .unwrap_or(false)
                {
                    selected_by_click = Some(idx);
                }

                if item.finger_hover_out(actions).is_some() && Some(idx) == self.pointer_hover_index
                {
                    self.pointer_hover_index = None;
                    should_redraw = true;
                }

                if item.finger_hover_in(actions).is_some() {
                    self.pointer_hover_index = Some(idx);
                    self.keyboard_focus_index = Some(idx);
                    should_redraw = true;
                }
            }

            if should_redraw {
                list.redraw(cx);
            }

            if let Some(selected) = selected_by_click {
                self.select_item(cx, scope, selected);
            }

            for action in actions.iter().filter_map(|a| a.as_widget_action()) {
                if action.widget_uid == self.key_controller_text_input_ref(cx).widget_uid() {
                    if let TextInputAction::KeyFocusLost = action.cast() {
                        self.hide_popup(cx);
                        self.redraw(cx);
                    }
                }

                if action.widget_uid == self.search_input_ref(cx).widget_uid() {
                    if let TextInputAction::Changed(search) = action.cast() {
                        self.search_input_ref(cx)
                            .set_text(cx, search.lines().next().unwrap_or_default());

                        cx.widget_action(self.widget_uid(), InternalAction::ShouldBuildItems);
                        self.ensure_popup_consistent(cx);
                    }
                }
            }
        }

        self.prev_cursor_position = get_head(&self.text_input_ref(cx));
        self.ensure_popup_consistent(cx);
    }
}

impl MypiCommandTextInput {
    fn ensure_popup_consistent(&mut self, cx: &mut Cx) {
        if self.view(cx, ids!(popup)).visible() {
            if self.inline_search {
                self.view(cx, ids!(search_input_wrapper))
                    .set_visible(cx, false);
            } else {
                self.view(cx, ids!(search_input_wrapper))
                    .set_visible(cx, true);
            }
        }
    }

    pub fn keyboard_focus_index(&self) -> Option<usize> {
        self.keyboard_focus_index
    }

    pub fn set_items(&mut self, cx: &mut Cx, items: Vec<CommandInfo>) {
        self.items = items;
        self.keyboard_focus_index = (!self.items.is_empty()).then_some(0);
        self.pointer_hover_index = None;
        self.portal_list(cx, ids!(list)).set_first_id(0);
        self.redraw(cx);
    }

    fn on_text_inserted(&mut self, cx: &mut Cx, _scope: &mut Scope, inserted: &str) {
        if graphemes(inserted).last() == self.trigger_grapheme() {
            self.show_popup(cx);
            self.trigger_position = Some(get_head(&self.text_input_ref(cx)));

            if self.inline_search {
                self.view(cx, ids!(search_input_wrapper))
                    .set_visible(cx, false);
            } else {
                self.view(cx, ids!(search_input_wrapper))
                    .set_visible(cx, true);
                self.is_search_input_focus_pending = true;
            }

            cx.widget_action(self.widget_uid(), InternalAction::ShouldBuildItems);
            self.ensure_popup_consistent(cx);
        }
    }

    fn on_keyboard_controller_input_submit(&mut self, cx: &mut Cx, scope: &mut Scope) {
        let Some(idx) = self.keyboard_focus_index else {
            return;
        };

        self.select_item(cx, scope, idx);
    }

    fn select_item(&mut self, cx: &mut Cx, _scope: &mut Scope, selected: usize) {
        let Some(command) = self.items.get(selected) else {
            return;
        };
        self.last_selected_command = command.name.clone();
        self.try_remove_trigger_and_inline_search(cx);
        cx.widget_action(self.widget_uid(), InternalAction::ItemSelected);
        self.hide_popup(cx);
        self.is_text_input_focus_pending = true;
        self.redraw(cx);
    }

    fn try_remove_trigger_and_inline_search(&mut self, cx: &mut Cx) {
        let mut to_remove = self.trigger_grapheme().unwrap_or_default().to_string();

        if self.inline_search {
            to_remove.push_str(&self.search_text(cx));
        }

        let text = self.text_input_ref(cx).text();
        let end = get_head(&self.text_input_ref(cx));
        let text_graphemes: Vec<&str> = text.graphemes(true).collect();
        let mut byte_index = 0;
        let mut end_grapheme_idx = 0;

        for (i, g) in text_graphemes.iter().enumerate() {
            if byte_index <= end && byte_index + g.len() > end {
                end_grapheme_idx = i;
                break;
            }
            byte_index += g.len();
        }

        let start_grapheme_idx = if end_grapheme_idx >= to_remove.graphemes(true).count() {
            end_grapheme_idx - to_remove.graphemes(true).count()
        } else {
            return;
        };

        let new_text = text_graphemes[..start_grapheme_idx].join("")
            + &text_graphemes[end_grapheme_idx..].join("");

        let new_cursor_pos = text_graphemes[..start_grapheme_idx]
            .join("")
            .graphemes(true)
            .count();

        self.text_input_ref(cx).set_cursor(
            cx,
            Cursor {
                index: new_cursor_pos,
                prefer_next_row: false,
            },
            false,
        );
        self.set_text(cx, &new_text);
    }

    fn show_popup(&mut self, cx: &mut Cx) {
        if self.inline_search {
            self.view(cx, ids!(search_input_wrapper))
                .set_visible(cx, false);
        } else {
            self.view(cx, ids!(search_input_wrapper))
                .set_visible(cx, true);
        }
        self.view(cx, ids!(popup)).set_visible(cx, true);
        self.view(cx, ids!(popup)).redraw(cx);
    }

    fn hide_popup(&mut self, cx: &mut Cx) {
        self.clear_popup(cx);
        self.view(cx, ids!(popup)).set_visible(cx, false);
    }

    pub fn reset(&mut self, cx: &mut Cx) {
        self.hide_popup(cx);
        self.text_input_ref(cx).set_text(cx, "");
    }

    fn clear_popup(&mut self, cx: &mut Cx) {
        self.trigger_position = None;
        self.search_input_ref(cx).set_text(cx, "");
        self.search_input_ref(cx).set_cursor(
            cx,
            Cursor {
                index: 0,
                prefer_next_row: false,
            },
            false,
        );
        self.clear_items(cx);
    }

    pub fn clear_items(&mut self, cx: &mut Cx) {
        self.items.clear();
        self.keyboard_focus_index = None;
        self.pointer_hover_index = None;
        self.portal_list(cx, ids!(list)).set_first_id(0);
        self.redraw(cx);
    }

    pub fn search_text(&self, cx: &Cx) -> String {
        const MAX_SEARCH_TEXT_LENGTH: usize = 100;

        if self.inline_search {
            if let Some(trigger_pos) = self.trigger_position {
                let text = self.text_input_ref(cx).text();
                let head = get_head(&self.text_input_ref(cx));

                if head > trigger_pos {
                    let text_graphemes: Vec<&str> = text.graphemes(true).collect();
                    let mut byte_pos = 0;
                    let mut trigger_grapheme_idx = None;
                    let mut head_grapheme_idx = None;
                    let mut last_grapheme_end = 0;

                    for (i, g) in text_graphemes.iter().enumerate() {
                        if byte_pos <= trigger_pos && byte_pos + g.len() > trigger_pos {
                            trigger_grapheme_idx = Some(i);
                        } else if byte_pos + g.len() == trigger_pos {
                            trigger_grapheme_idx = Some(i + 1);
                        }

                        if byte_pos <= head && byte_pos + g.len() > head {
                            head_grapheme_idx = Some(i);
                        } else if byte_pos + g.len() == head {
                            head_grapheme_idx = Some(i + 1);
                        }

                        byte_pos += g.len();
                        last_grapheme_end = byte_pos;
                    }

                    if head_grapheme_idx.is_none() && head >= last_grapheme_end {
                        head_grapheme_idx = Some(text_graphemes.len());
                    }

                    if trigger_grapheme_idx.is_none() && trigger_pos >= last_grapheme_end {
                        trigger_grapheme_idx = Some(text_graphemes.len());
                    }

                    if let (Some(t_idx), Some(h_idx)) = (trigger_grapheme_idx, head_grapheme_idx) {
                        if t_idx >= text_graphemes.len() || h_idx > text_graphemes.len() {
                            return String::new();
                        }

                        if t_idx < h_idx {
                            let length = h_idx - t_idx;
                            if length > MAX_SEARCH_TEXT_LENGTH {
                                return text_graphemes[t_idx..t_idx + MAX_SEARCH_TEXT_LENGTH]
                                    .join("");
                            }

                            let mut result = String::with_capacity(
                                text_graphemes[t_idx..h_idx].iter().map(|g| g.len()).sum(),
                            );
                            for g in &text_graphemes[t_idx..h_idx] {
                                result.push_str(g);
                            }
                            return result;
                        } else if t_idx == h_idx {
                            return String::new();
                        } else {
                            return String::new();
                        }
                    } else {
                        return String::new();
                    }
                }

                String::new()
            } else {
                String::new()
            }
        } else {
            self.search_input_ref(cx).text()
        }
    }

    pub fn item_selected(&self, actions: &Actions) -> Option<String> {
        actions
            .iter()
            .filter_map(|a| a.as_widget_action())
            .filter(|a| a.widget_uid == self.widget_uid())
            .find_map(|a| {
                if let InternalAction::ItemSelected = a.cast() {
                    Some(self.last_selected_command.clone())
                } else {
                    None
                }
            })
    }

    pub fn should_build_items(&self, actions: &Actions) -> bool {
        actions
            .iter()
            .filter_map(|a| a.as_widget_action())
            .filter(|a| a.widget_uid == self.widget_uid())
            .any(|a| matches!(a.cast(), InternalAction::ShouldBuildItems))
    }

    pub fn text_input_ref(&self, cx: &Cx) -> TextInputRef {
        self.text_input(cx, ids!(text_input))
    }

    pub fn search_input_ref(&self, cx: &Cx) -> TextInputRef {
        self.text_input(cx, ids!(search_input))
    }

    fn trigger_grapheme(&self) -> Option<&str> {
        self.trigger.as_ref().and_then(|t| graphemes(t).next())
    }

    fn key_controller_text_input_ref(&self, cx: &Cx) -> TextInputRef {
        if self.inline_search {
            self.text_input_ref(cx)
        } else {
            self.search_input_ref(cx)
        }
    }

    fn on_keyboard_move(&mut self, cx: &mut Cx, delta: i32) {
        if self.items.is_empty() {
            return;
        }

        let selected = match self.keyboard_focus_index {
            Some(idx) => idx
                .saturating_add_signed(delta as isize)
                .clamp(0, self.items.len() - 1),
            None if delta > 0 => 0,
            None => self.items.len() - 1,
        };
        self.keyboard_focus_index = Some(selected);
        self.pointer_hover_index = None;

        self.portal_list(cx, ids!(list))
            .smooth_scroll_to(cx, selected, 12.0, Some(4), 4.0);
        self.redraw(cx);
    }

    pub fn request_text_input_focus(&mut self) {
        self.is_text_input_focus_pending = true;
    }
}

impl MypiCommandTextInputRef {
    pub fn should_build_items(&self, actions: &Actions) -> bool {
        self.borrow()
            .map_or(false, |inner| inner.should_build_items(actions))
    }

    pub fn set_items(&self, cx: &mut Cx, items: Vec<CommandInfo>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_items(cx, items);
        }
    }

    pub fn item_selected(&self, actions: &Actions) -> Option<String> {
        self.borrow().and_then(|inner| inner.item_selected(actions))
    }

    pub fn text_input_ref(&self, cx: &Cx) -> TextInputRef {
        self.borrow()
            .map_or(WidgetRef::empty().as_text_input(), |inner| {
                inner.text_input_ref(cx)
            })
    }

    pub fn search_input_ref(&self, cx: &Cx) -> TextInputRef {
        self.borrow()
            .map_or(WidgetRef::empty().as_text_input(), |inner| {
                inner.search_input_ref(cx)
            })
    }

    pub fn reset(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.reset(cx);
        }
    }

    pub fn request_text_input_focus(&self) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.request_text_input_focus();
        }
    }

    pub fn search_text(&self, cx: &Cx) -> String {
        self.borrow()
            .map_or(String::new(), |inner| inner.search_text(cx))
    }
}

fn graphemes(text: &str) -> impl DoubleEndedIterator<Item = &str> {
    text.graphemes(true)
}

fn get_head(text_input: &TextInputRef) -> usize {
    text_input.borrow().map_or(0, |p| p.cursor().index)
}

fn is_whitespace(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}
