//! Non-macOS placeholder: the embedded browser is macOS-only.

use gpui::*;
use gpui_component::ActiveTheme;

pub struct BrowserView;

impl BrowserView {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }

    pub fn load_url(&mut self, _url: &str, _cx: &mut Context<Self>) {}

    pub fn go_back(&mut self, _cx: &mut Context<Self>) {}

    pub fn reload(&mut self, _cx: &mut Context<Self>) {}

    pub fn current_url(&self, _cx: &App) -> Option<String> {
        None
    }

    pub fn evaluate_script(
        &self,
        _script: &str,
        _cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<String>, String> {
        Err("The embedded browser is available on macOS only.".to_string())
    }

    pub fn take_snapshot(
        &self,
        _cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(Vec<u8>, u32, u32), String>>, String> {
        Err("The embedded browser is available on macOS only.".to_string())
    }

    pub fn set_visible(&mut self, _visible: bool, _cx: &mut Context<Self>) {}
}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Embedded browser is available on macOS only."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Open the URL in your system browser to continue."),
            )
    }
}
