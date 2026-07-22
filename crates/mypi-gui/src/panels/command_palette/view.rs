//! CommandTextInput widget implementation and completion popup view.

use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use unicode_segmentation::UnicodeSegmentation;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.CommandTextInputListBase = #(List::register_widget(vm))

    mod.widgets.CommandTextInputList = mod.widgets.CommandTextInputListBase{
        flow: Down
        width: Fill
        height: 208
    }

    mod.widgets.CommandTextInputBase = #(CommandTextInput::register_widget(vm))

    mod.widgets.CommandTextInput = mod.widgets.CommandTextInputBase{
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

            list := mod.widgets.CommandTextInputList{
                height: 208
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
pub struct CommandTextInput {
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
    selectable_widgets: Vec<WidgetRef>,

    #[rust]
    last_selected_widget: WidgetRef,

    #[rust]
    trigger_position: Option<usize>,

    #[rust]
    prev_cursor_position: usize,
}

impl Widget for CommandTextInput {
    fn set_text(&mut self, cx: &mut Cx, v: &str) {
        self.text_input_ref(cx).set_text(cx, v);
    }

    fn text(&self) -> String {
        String::new()
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.update_highlights(cx);
        self.ensure_popup_consistent(cx);

        while !self.deref.draw_walk(cx, scope, walk).is_done() {}

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
        if cx.has_key_focus(self.key_controller_text_input_ref(cx).area()) {
            if let Event::KeyDown(key_event) = event {
                let popup_visible = self.view(cx, ids!(popup)).visible();

                if popup_visible {
                    let mut eat_the_event = true;

                    match key_event.key_code {
                        KeyCode::ArrowDown => {
                            self.pointer_hover_index = None;
                            self.on_keyboard_move(cx, 1);
                        }
                        KeyCode::ArrowUp => {
                            self.pointer_hover_index = None;
                            self.on_keyboard_move(cx, -1);
                        }
                        KeyCode::ReturnKey => {
                            self.on_keyboard_controller_input_submit(cx, scope);
                        }
                        KeyCode::Escape => {
                            self.is_text_input_focus_pending = true;
                            self.hide_popup(cx);
                            self.redraw(cx);
                        }
                        _ => {
                            eat_the_event = false;
                        }
                    };

                    if eat_the_event {
                        return;
                    }
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

            for (idx, item) in self.selectable_widgets.iter().enumerate() {
                let item = item.as_view();

                if item
                    .finger_down(actions)
                    .map(|fe| fe.tap_count == 1)
                    .unwrap_or(false)
                {
                    selected_by_click = Some((&*item).clone());
                    self.keyboard_focus_index = None;
                }

                if item.finger_hover_out(actions).is_some() && Some(idx) == self.pointer_hover_index
                {
                    self.pointer_hover_index = None;
                    should_redraw = true;
                }

                if item.finger_hover_in(actions).is_some() {
                    self.pointer_hover_index = Some(idx);
                    self.keyboard_focus_index = None;
                    should_redraw = true;
                }
            }

            if should_redraw {
                self.redraw(cx);
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

impl CommandTextInput {
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

    pub fn set_keyboard_focus_index(&mut self, idx: usize) {
        if !self.selectable_widgets.is_empty() {
            self.keyboard_focus_index = Some(idx.clamp(0, self.selectable_widgets.len() - 1));
        }
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

        self.select_item(cx, scope, self.selectable_widgets[idx].clone());
    }

    fn select_item(&mut self, cx: &mut Cx, _scope: &mut Scope, selected: WidgetRef) {
        self.try_remove_trigger_and_inline_search(cx);
        self.last_selected_widget = selected;
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

    pub fn clear_items(&mut self, cx: &Cx) {
        self.list(cx, ids!(list)).clear();
        self.selectable_widgets.clear();
        self.keyboard_focus_index = None;
        self.pointer_hover_index = None;
    }

    pub fn add_item(&mut self, cx: &Cx, widget: WidgetRef) {
        self.list(cx, ids!(list)).add(widget.clone());
        self.selectable_widgets.push(widget);
        self.keyboard_focus_index = self.keyboard_focus_index.or(Some(0));
    }

    pub fn add_unselectable_item(&mut self, cx: &Cx, widget: WidgetRef) {
        self.list(cx, ids!(list)).add(widget);
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

    pub fn item_selected(&self, actions: &Actions) -> Option<WidgetRef> {
        actions
            .iter()
            .filter_map(|a| a.as_widget_action())
            .filter(|a| a.widget_uid == self.widget_uid())
            .find_map(|a| {
                if let InternalAction::ItemSelected = a.cast() {
                    Some(self.last_selected_widget.clone())
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
        let Some(idx) = self.keyboard_focus_index else {
            if !self.selectable_widgets.is_empty() {
                if delta > 0 {
                    self.keyboard_focus_index = Some(0);
                } else {
                    self.keyboard_focus_index = Some(self.selectable_widgets.len() - 1);
                }
            }
            return;
        };

        let new_index = idx
            .saturating_add_signed(delta as isize)
            .clamp(0, self.selectable_widgets.len() - 1);

        if idx != new_index {
            self.keyboard_focus_index = Some(new_index);
        }

        self.pointer_hover_index = None;
        self.redraw(cx);
    }

    fn update_highlights(&mut self, cx: &mut Cx) {
        let has_keyboard_focus = self.keyboard_focus_index.is_some();

        for (idx, item) in self.selectable_widgets.iter().enumerate() {
            let mut item = item.clone();
            script_apply_eval!(cx, item, { show_bg: true });

            if Some(idx) == self.keyboard_focus_index {
                let color = self.color_focus;
                script_apply_eval!(cx, item, {
                    draw_bg +: {
                        color: #(color)
                    }
                });
            } else if Some(idx) == self.pointer_hover_index && !has_keyboard_focus {
                let color = self.color_hover;
                script_apply_eval!(cx, item, {
                    draw_bg +: {
                        color: #(color)
                    }
                });
            } else {
                script_apply_eval!(cx, item, {
                    draw_bg +: {
                        color: #(Vec4f::all(0.))
                    }
                });
            }
        }
    }

    pub fn request_text_input_focus(&mut self) {
        self.is_text_input_focus_pending = true;
    }
}

impl CommandTextInputRef {
    pub fn should_build_items(&self, actions: &Actions) -> bool {
        self.borrow()
            .map_or(false, |inner| inner.should_build_items(actions))
    }

    pub fn clear_items(&mut self, cx: &Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.clear_items(cx);
        }
    }

    pub fn add_item(&self, cx: &Cx, widget: WidgetRef) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.add_item(cx, widget);
        }
    }

    pub fn add_unselectable_item(&self, cx: &Cx, widget: WidgetRef) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.add_unselectable_item(cx, widget);
        }
    }

    pub fn item_selected(&self, actions: &Actions) -> Option<WidgetRef> {
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

#[derive(Script, ScriptHook, Widget)]
struct List {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    deref: View,

    #[rust]
    area: Area,

    #[rust]
    items: Vec<WidgetRef>,
}

impl Widget for List {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.items.iter().for_each(|item| {
            item.handle_event(cx, event, scope);
        });
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.deref.layout);
        self.items.iter().for_each(|item| {
            item.draw_all(cx, scope);
        });
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

impl List {
    fn clear(&mut self) {
        self.items.clear();
    }

    fn add(&mut self, widget: WidgetRef) {
        self.items.push(widget);
    }
}

impl ListRef {
    fn clear(&self) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };

        inner.clear();
    }

    fn add(&self, widget: WidgetRef) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };

        inner.add(widget);
    }
}
