//! Searchable picker presentation over a host-captured snapshot.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::combobox::{Combobox, ComboboxState};
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, h_flex, v_flex};

/// One row in the picker: a model, an agent, one of an agent's advertised
/// models, or the Settings recovery action.
#[derive(Debug, Clone)]
pub struct PickerItem<T> {
    pub value: T,
    pub title: SharedString,
    /// Muted trailing text — a distinguishing id when labels collide, or an
    /// agent's status ("Connecting…", error) / the model it will run.
    pub secondary: Option<SharedString>,
    pub icon_path: Option<SharedString>,
    /// The committed "current model" mark, rendered as the check icon and
    /// kept visually distinct from the keyboard highlight.
    pub current: bool,
    /// Rows nested under an agent row (choices, the Settings recovery row)
    /// indent one icon width so the grouping reads at a glance.
    pub indented: bool,
    /// Lowercased search haystack: label, id, and provider/agent names.
    pub haystack: String,
}

impl<T> PickerItem<T> {
    pub fn haystack_for(parts: &[&str]) -> String {
        parts.join("\u{0}").to_lowercase()
    }
}

impl<T: Clone + PartialEq> SearchableListItem for PickerItem<T> {
    type Value = T;

    fn title(&self) -> SharedString {
        // The rendered row can truncate; its accessible name retains the
        // distinguishing ID or agent status carried by secondary text.
        self.secondary.as_ref().map_or_else(
            || self.title.clone(),
            |secondary| format!("{} · {secondary}", self.title).into(),
        )
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        query
            .split_whitespace()
            .all(|token| self.haystack.contains(&token.to_lowercase()))
    }

    fn render(&self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .when(self.indented && self.icon_path.is_none(), |this| {
                this.child(div().w(px(14.)).flex_shrink_0())
            })
            .when_some(self.icon_path.clone(), |this, path| {
                this.child(
                    Icon::default()
                        .path(path)
                        .xsmall()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .truncate()
                    .child(self.title.clone()),
            )
            .when_some(self.secondary.clone(), |this, secondary| {
                this.child(
                    div()
                        .min_w_0()
                        .max_w(relative(0.6))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(secondary),
                )
            })
    }
}

/// A provider group in the picker list.
#[derive(Debug, Clone)]
pub struct PickerSection<T> {
    pub header: SharedString,
    pub items: Vec<PickerItem<T>>,
}

/// Re-filters a snapshot: every whitespace-separated token must appear in a
/// row's haystack, case-insensitively; sections left empty are pruned so no
/// heading is left dangling.
pub fn filter_picker_sections<T: Clone>(
    sections: &[PickerSection<T>],
    query: &str,
) -> Vec<PickerSection<T>> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return sections.to_vec();
    }
    sections
        .iter()
        .filter_map(|section| {
            let items: Vec<PickerItem<T>> = section
                .items
                .iter()
                .filter(|item| {
                    tokens
                        .iter()
                        .all(|token| item.haystack.contains(token.as_str()))
                })
                .cloned()
                .collect();
            (!items.is_empty()).then(|| PickerSection {
                header: section.header.clone(),
                items,
            })
        })
        .collect()
}

/// Hosts own the state entity, snapshot refresh and selection subscription.
pub fn composer_model_picker<D: SearchableListDelegate>(
    state: &Entity<ComboboxState<D>>,
    label: impl Into<SharedString>,
    has_models: bool,
    provider_icon: Option<SharedString>,
    on_open: impl Fn(bool) + 'static,
    cx: &App,
) -> Div {
    let label = label.into();
    let clear_state = state.clone();
    div().min_w_0().child(
        Combobox::new(state)
            .small()
            .menu_width(rems(20.))
            .menu_max_h(rems(20.))
            .search_placeholder("Search models or agents…")
            .disabled(!has_models)
            .empty(move |_, cx| {
                v_flex()
                    .debug_selector(|| "model-picker-empty".into())
                    .items_center()
                    .gap_2()
                    .py_6()
                    .text_color(cx.theme().muted_foreground)
                    .child("No matching models")
                    .child(
                        Button::new("model-picker-clear-search")
                            .debug_selector(|| "model-picker-clear-search".into())
                            .small()
                            .label("Clear search")
                            .on_click({
                                let state = clear_state.clone();
                                move |_, window, cx| {
                                    state.update(cx, |state, cx| state.set_query("", window, cx));
                                }
                            }),
                    )
            })
            .render_trigger(move |trigger, _, _| {
                on_open(trigger.is_open());
                // Combobox owns focus and activation; the Button provides its appearance.
                crate::composer_model_button(
                    label.clone(),
                    has_models,
                    trigger.is_open(),
                    trigger.is_disabled(),
                )
                .tab_stop(false)
                .when_some(provider_icon.clone(), |button, path| {
                    button.icon(Icon::default().path(path))
                })
            })
            .h_7()
            .border_0()
            .rounded_full()
            .bg(cx.theme().transparent)
            .max_w(rems(12.5)),
    )
}

pub fn project_picker_item(label: impl Into<SharedString>, current: bool) -> PopupMenuItem {
    PopupMenuItem::new(label).checked(current)
}

pub fn new_project_picker_item() -> PopupMenuItem {
    PopupMenuItem::new("New project…")
}

pub fn project_picker_menu(
    menu: PopupMenu,
    projects: impl IntoIterator<Item = PopupMenuItem>,
    attach: PopupMenuItem,
) -> PopupMenu {
    projects
        .into_iter()
        .fold(menu, |menu, project| menu.item(project))
        .separator()
        .item(attach)
}

pub fn new_task_project_button(label: impl Into<SharedString>, path: Option<String>) -> Button {
    let label = label.into();
    let hint = path
        .map(|path| format!("Project for the new task: {path}"))
        .unwrap_or_else(|| "Choose a project for the new task".into());
    Button::new("new-task-project-picker")
        .debug_selector(|| "new-task-project-picker".into())
        .icon(IconName::Folder)
        .label(label)
        .accessibility_label(hint.clone())
        .tooltip(hint)
        .dropdown_caret(true)
        .ghost()
        .small()
}

/// `SearchableListDelegate` over one open's snapshot.
///
/// `sections` is the filtered view the list reads; `all` is the captured
/// snapshot `perform_search` re-filters from, so discovery finishing while
/// the popup is open never reorders rows under the keyboard cursor.
pub struct PickerDelegate<T> {
    all: Vec<PickerSection<T>>,
    sections: Vec<PickerSection<T>>,
}

impl<T: Clone> PickerDelegate<T> {
    pub fn new(all: Vec<PickerSection<T>>) -> Self {
        Self {
            sections: all.clone(),
            all,
        }
    }
}

impl<T: Clone + PartialEq + 'static> SearchableListDelegate for PickerDelegate<T> {
    type Item = PickerItem<T>;

    fn sections_count(&self, _: &App) -> usize {
        self.sections.len()
    }

    fn items_count(&self, section: usize) -> usize {
        self.sections.get(section).map_or(0, |s| s.items.len())
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.sections.get(ix.section)?.items.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(section_ix, section)| {
                <Vec<PickerItem<T>> as SearchableListDelegate>::position(&section.items, value)
                    .map(|ix| ix.section(section_ix))
            })
    }

    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> Task<()> {
        self.sections = filter_picker_sections(&self.all, query);
        Task::ready(())
    }

    fn render_section_header(
        &self,
        section: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let header = self.sections.get(section)?.header.clone();
        Some(
            div()
                .py_0p5()
                .px_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(header)
                .into_any_element(),
        )
    }

    /// The check icon answers "which model runs now", not "which row is
    /// highlighted" — it reads the captured current flag, not the list's
    /// keyboard selection.
    fn is_item_checked(
        &self,
        _ix: IndexPath,
        item: &Self::Item,
        _current_selection: &[(IndexPath, Self::Item)],
        _cx: &App,
    ) -> bool {
        item.current
    }
}

/// A keyboard-accessible, scrollable choice menu used by project scopes and forms.
pub fn choice_picker(
    id: &'static str,
    label: String,
    choices: Vec<(String, String)>,
    selected: String,
    disabled: bool,
    on_select: impl Fn(String, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    choice_menu(
        Button::new(id)
            .label(label.clone())
            .accessibility_label(label)
            .dropdown_caret(true)
            .disabled(disabled),
        choices,
        selected,
        on_select,
    )
}

/// Compose a themed trigger with the same checked, scrollable choice menu.
pub fn choice_menu<T: Clone + PartialEq + 'static>(
    button: Button,
    choices: Vec<(T, String)>,
    selected: T,
    on_select: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = std::rc::Rc::new(on_select);
    button.dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, _| {
        choices.iter().fold(
            menu.max_h(window.rem_size() * 20.0).scrollable(true),
            |menu, (id, label)| {
                let id = id.clone();
                let callback = on_select.clone();
                menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(id == selected)
                        .on_click(move |_, window, cx| callback(id.clone(), window, cx)),
                )
            },
        )
    })
}
