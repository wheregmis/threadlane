//! Local browser host: shared controls, with no network or native webview service.
use gpui::{prelude::*, *};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::ActiveTheme;
use threadlane_ui_kit as kit;

const SAMPLE_URL: &str = "https://example.com/preview";

pub struct BrowserPreview {
    address: Entity<InputState>,
    tabs: Vec<(usize, String)>,
    active: usize,
    next_id: usize,
    scroll: ScrollHandle,
    revealed: Option<(usize, usize)>,
    annotating: bool,
    notice: String,
    focus: FocusHandle,
    _escape: Subscription,
}

impl BrowserPreview {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).default_value(SAMPLE_URL));
        cx.subscribe(&address, |host, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let value = input.read(cx).value().trim().to_owned();
                if let Some((_, url)) = host.tabs.get_mut(host.active) {
                    if !value.is_empty() {
                        *url = value;
                        host.notice =
                            "Address updated locally. Page navigation runs in the desktop browser."
                                .into();
                        cx.notify();
                    }
                }
            }
        })
        .detach();
        let owner = cx.entity().downgrade();
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key == "escape" && event.keystroke.modifiers == Modifiers::default()
            {
                let _ = owner.update(cx, |host, cx| {
                    if host.annotating && host.focus.contains_focused(window, cx) {
                        host.annotating = false;
                        cx.stop_propagation();
                        cx.notify();
                    }
                });
            }
        });
        Self {
            address,
            tabs: vec![(0, SAMPLE_URL.into())],
            active: 0,
            next_id: 1,
            scroll: ScrollHandle::new(),
            revealed: None,
            annotating: false,
            notice: "Sample tabs · Shared browser controls · Page content is hosted by the desktop webview.".into(),
            focus: cx.focus_handle(),
            _escape: escape,
        }
    }

    fn request(&mut self, action: kit::BrowserAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            kit::BrowserAction::NewTab => {
                self.tabs.push((self.next_id, SAMPLE_URL.into()));
                self.next_id += 1;
                self.active = self.tabs.len() - 1;
                self.annotating = false;
            }
            kit::BrowserAction::SelectTab(id) => {
                if let Some(index) = self.tabs.iter().position(|(tab, _)| *tab == id) {
                    self.active = index;
                    self.annotating = false;
                }
            }
            kit::BrowserAction::CloseTab(id) => {
                if let Some(index) = self.tabs.iter().position(|(tab, _)| *tab == id) {
                    if self.tabs.len() == 1 {
                        self.tabs[0].1 = SAMPLE_URL.into();
                    } else {
                        self.tabs.remove(index);
                        if index < self.active {
                            self.active -= 1;
                        }
                        self.active = self.active.min(self.tabs.len() - 1);
                        self.annotating = false;
                    }
                }
            }
            kit::BrowserAction::ToggleAnnotate => {
                self.focus.focus(window, cx);
                self.annotating = !self.annotating;
            }
            kit::BrowserAction::Back | kit::BrowserAction::Forward | kit::BrowserAction::Reload => {
                self.notice =
                    "Page navigation runs in the desktop browser. These preview tabs stay local."
                        .into();
            }
        }
        if let Some((_, url)) = self.tabs.get(self.active) {
            self.address
                .update(cx, |input, cx| input.set_value(url.clone(), window, cx));
        }
        cx.notify();
    }
}

impl Render for BrowserPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.get(self.active);
        let selection = active.map(|(id, _)| (*id, self.active));
        if self.revealed != selection {
            self.scroll.scroll_to_item(self.active);
            self.revealed = selection;
        }
        kit::browser_chrome(
            &self.address,
            &self.tabs,
            active.map(|(id, _)| *id),
            &self.scroll,
            self.annotating,
            active.is_some_and(|(_, url)| url.starts_with("https://")),
            cx.listener(|host, action: &kit::BrowserAction, window, cx| {
                host.request(*action, window, cx)
            }),
            cx,
        )
        .track_focus(&self.focus)
        .when(!self.tabs.is_empty(), |panel| {
            panel.child(
                kit::browser_viewport(cx)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_4()
                    .child(div().text_sm().child("Browser preview"))
                    .child(
                        div()
                            .text_xs()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.notice.clone()),
                    ),
            )
        })
    }
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
