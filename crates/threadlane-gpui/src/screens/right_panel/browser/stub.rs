//! Non-macOS placeholder: the embedded browser is macOS-only.

use gpui::*;

pub struct BrowserView;

impl BrowserView {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }

    pub fn load_url(&mut self, _url: &str, _cx: &mut Context<Self>) {}

    pub(crate) fn go_back(&mut self, _cx: &mut Context<Self>) {}

    pub(crate) fn reload(&mut self, _cx: &mut Context<Self>) {}

    pub fn current_url(&self, _cx: &App) -> Option<String> {
        None
    }

    pub fn set_visible(&mut self, _visible: bool, _cx: &mut Context<Self>) {}
}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("Embedded browser is available on macOS only")
    }
}
