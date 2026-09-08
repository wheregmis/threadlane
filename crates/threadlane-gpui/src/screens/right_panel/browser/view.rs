//! Embedded browser surface for the right panel (macOS only).
//!
//! Thin wrapper over `gpui-wry` (same git checkout as `gpui-kit`, same
//! `gpui-pre`): a real `WKWebView` child view with a Safari-style address
//! bar. This is the Day-1 slice of the Waku port — omnibox resolution,
//! per-frame bounds sync and visibility come from `gpui-wry`'s element.
//!
//! Known limitation (matches upstream `gpui-kit/examples/webview`): our
//! `gpui-pre` has no overlay plane (`GPUIOverlayView` exists only in Waku's
//! pinned fork), so the native view paints above GPUI menus/tooltips that
//! overlap its rect. The tab hides the view when inactive to bound this.

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, IconName, Sizable};

use super::address::{AddressTarget, resolve_address, search_url};

const DEFAULT_URL: &str = "https://gpui-kit.com";

pub struct BrowserView {
    focus_handle: FocusHandle,
    webview: Entity<gpui_wry::WebView>,
    address_input: Entity<InputState>,
}

impl BrowserView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        use raw_window_handle::HasWindowHandle;

        let window_handle = window.window_handle().expect("window handle");
        let builder = wry::WebViewBuilder::new();
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let wry_webview = builder
            .build_as_child(&window_handle)
            .expect("wry child webview");
        let webview = cx.new(|cx| gpui_wry::WebView::new(wry_webview, window, cx));

        let address_input =
            cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_URL));
        webview.update(cx, |view, _| view.load_url(DEFAULT_URL));

        cx.subscribe(
            &address_input,
            |this: &mut Self, input, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let raw = input.read(cx).value().to_string();
                    this.navigate_to_input(&raw, cx);
                }
                _ => {}
            },
        )
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            webview,
            address_input,
        }
    }

    fn navigate_to_input(&mut self, raw: &str, cx: &mut Context<Self>) {
        let target = resolve_address(raw);
        let url = match target {
            None => return,
            Some(AddressTarget::Url(url)) => url,
            Some(AddressTarget::Search(query)) => search_url(&query),
        };
        self.load_url(&url, cx);
    }

    /// Navigate the page. Public for later agent-tool wiring.
    pub fn load_url(&mut self, url: &str, cx: &mut Context<Self>) {
        self.webview.update(cx, |view, _| view.load_url(url));
        cx.notify();
    }

    /// Current committed URL, if the webview reports one.
    pub fn current_url(&self, cx: &App) -> Option<String> {
        self.webview
            .read(cx)
            .raw()
            .url()
            .ok()
            .filter(|url| !url.is_empty())
    }

    /// Evaluate a synchronous script, resolving with wry's JSON-serialized
    /// result. The callback fires on the main thread; the caller awaits the
    /// receiver on a foreground task (never blocking the UI).
    pub(crate) fn evaluate_script(
        &self,
        script: &str,
        cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<String>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        self.webview
            .read(cx)
            .raw()
            .evaluate_script_with_callback(script, move |result| {
                if let Ok(mut slot) = tx.lock() {
                    if let Some(tx) = slot.take() {
                        let _ = tx.send(result);
                    }
                }
            })
            .map_err(|error| format!("Script evaluation failed to start: {error}"))?;
        Ok(rx)
    }

    pub(crate) fn go_back(&mut self, cx: &mut Context<Self>) {
        self.webview.update(cx, |view, _| {
            let _ = view.back();
        });
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let url = self.current_url(cx);
        self.webview.update(cx, |view, _| match url.as_deref() {
            Some(url) if !url.is_empty() => view.load_url(url),
            _ => view.load_url(DEFAULT_URL),
        });
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.webview.update(cx, |view, _| {
            if visible {
                view.show();
            } else {
                view.hide();
            }
        });
    }
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let webview = self.webview.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("browser-back")
                            .icon(IconName::ArrowLeft)
                            .tooltip("Back")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.go_back(cx);
                            })),
                    )
                    .child(div().flex_1().child(Input::new(&self.address_input))),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .overflow_hidden()
                    .child(webview),
            )
    }
}
