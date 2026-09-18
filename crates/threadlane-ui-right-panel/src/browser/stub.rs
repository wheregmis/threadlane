//! Non-macOS placeholder: the embedded browser is macOS-only.

use gpui::*;
use gpui_component::ActiveTheme;

pub struct BrowserView {
    tabs: Vec<(usize, String)>,
    active_tab: usize,
    next_tab_id: usize,
}

impl BrowserView {
    pub fn new(
        _model: Entity<threadlane_ui_state::AppState>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            tabs: vec![(0, "about:blank".to_string())],
            active_tab: 0,
            next_tab_id: 1,
        }
    }

    pub fn open_tab(&mut self, url: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push((id, url.to_string()));
        self.active_tab = self.tabs.len() - 1;
        cx.notify();
    }

    pub fn close_tab(&mut self, id: usize, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() > 1 {
            self.tabs.retain(|(tab_id, _)| *tab_id != id);
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            cx.notify();
        }
    }

    pub fn switch_tab(&mut self, id: usize, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(position) = self.tabs.iter().position(|(tab_id, _)| *tab_id == id) {
            self.active_tab = position;
            cx.notify();
        }
    }

    pub fn tabs(&self) -> Vec<(usize, String)> {
        self.tabs.clone()
    }

    pub fn active_tab_id(&self) -> Option<usize> {
        self.tabs.get(self.active_tab).map(|(id, _)| *id)
    }

    pub fn is_annotating(&self) -> bool {
        false
    }

    pub fn toggle_annotate(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }

    pub fn stop_annotate(&mut self, _cx: &mut Context<Self>) {}

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
