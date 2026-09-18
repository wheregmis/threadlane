//! Embedded browser surface for the right panel (macOS only).
//!
//! Thin wrapper over `gpui-wry` (same git checkout as `gpui-kit`, same
//! `gpui-pre`): real `WKWebView` child views with a Safari-style address
//! bar, multiple tabs, and a click-to-annotate picker whose picks land in
//! the chat composer.
//!
//! Known limitation (matches upstream `gpui-kit/examples/webview`): our
//! `gpui-pre` has no overlay plane (`GPUIOverlayView` exists only in Waku's
//! pinned fork), so the native view paints above GPUI menus/tooltips that
//! overlap its rect. The tab hides the view when inactive to bound this.

use base64::Engine as _;
use gpui::*;
use gpui::prelude::FluentBuilder;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Icon, IconName, Selectable, Sizable};
use threadlane_ui_state::{AppState, RequestedComposerInsert};

use super::address::{resolve_address, search_url, AddressTarget};
use super::scripts::{
    annotate_install_js, annotate_poll_js, annotate_uninstall_js, unwrap_callback_payload,
};

const DEFAULT_URL: &str = "https://gpui-kit.com";

struct BrowserTab {
    id: usize,
    url: String,
    webview: Option<Entity<gpui_wry::WebView>>,
}

pub struct BrowserView {
    focus_handle: FocusHandle,
    model: Entity<AppState>,
    address_input: Entity<InputState>,
    tabs: Vec<BrowserTab>,
    active_tab: usize,
    next_tab_id: usize,
    annotating: bool,
    annotate_task: Option<Task<()>>,
}

impl BrowserView {
    pub fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address_input = cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_URL));
        let address_events = address_input.clone();
        cx.subscribe(
            &address_events,
            |this: &mut Self, input, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let raw = input.read(cx).value().to_string();
                    this.navigate_to_input(&raw, cx);
                }
                _ => {}
            },
        )
        .detach();

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            model,
            address_input,
            tabs: Vec::new(),
            active_tab: 0,
            next_tab_id: 1,
            annotating: false,
            annotate_task: None,
        };
        this.open_tab(DEFAULT_URL, window, cx);
        this
    }

    fn navigate_to_input(&mut self, raw: &str, cx: &mut Context<Self>) {
        let target = resolve_address(raw);
        let url = match target {
            None => return,
            Some(AddressTarget::Url(url)) => url,
            Some(AddressTarget::Search(query)) => search_url(&query),
        };
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.url = url.clone();
            if let Some(webview) = tab.webview.clone() {
                webview.update(cx, |view, _| view.load_url(&url));
            }
        }
        cx.notify();
    }

    fn active_webview(&self) -> Option<Entity<gpui_wry::WebView>> {
        self.tabs
            .get(self.active_tab)
            .and_then(|tab| tab.webview.clone())
    }

    fn spawn_webview(
        &self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<gpui_wry::WebView> {
        use raw_window_handle::HasWindowHandle;

        let window_handle = window.window_handle().expect("window handle");
        let builder = wry::WebViewBuilder::new();
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let builder =
            builder.with_initialization_script(super::scripts::console_interceptor_js());
        let wry_webview = builder
            .build_as_child(&window_handle)
            .expect("wry child webview");
        let webview = cx.new(|cx| gpui_wry::WebView::new(wry_webview, window, cx));
        webview.update(cx, |view, _| view.load_url(url));
        // Inactive tabs stay hidden until selected.
        webview.update(cx, |view, _| view.hide());
        webview
    }

    /// Open a URL in a new tab and switch to it. Needs a window to create
    /// the tab's webview; callers without one use [`Self::load_url`], which
    /// reuses the active tab's existing view.
    pub fn open_tab(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let webview = self.spawn_webview(url, window, cx);
        self.tabs.push(BrowserTab {
            id,
            url: url.to_string(),
            webview: Some(webview),
        });
        self.switch_tab(id, window, cx);
    }

    /// Close a tab. The last tab becomes a fresh default tab instead of
    /// leaving the surface empty.
    pub fn close_tab(&mut self, id: usize, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            let url = DEFAULT_URL.to_string();
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.url = url.clone();
                if let Some(webview) = tab.webview.clone() {
                    webview.update(cx, |view, _| view.load_url(&url));
                }
            }
            cx.notify();
            return;
        }
        if let Some(position) = self.tabs.iter().position(|tab| tab.id == id) {
            let removed = self.tabs.remove(position);
            if let Some(webview) = removed.webview {
                webview.update(cx, |view, _| view.hide());
            }
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            } else if position < self.active_tab {
                self.active_tab -= 1;
            }
            self.stop_annotate(cx);
            self.sync_active_visibility(cx);
            cx.notify();
        }
    }

    /// Switch to a tab, creating its webview on first activation.
    pub fn switch_tab(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(position) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.stop_annotate(cx);
        self.active_tab = position;
        let url = self.tabs[position].url.clone();
        if self.tabs[position].webview.is_none() {
            let webview = self.spawn_webview(&url, window, cx);
            self.tabs[position].webview = Some(webview);
        }
        self.sync_active_visibility(cx);
        cx.notify();
    }

    pub fn tabs(&self) -> Vec<(usize, String)> {
        self.tabs
            .iter()
            .map(|tab| (tab.id, tab.url.clone()))
            .collect()
    }

    pub fn active_tab_id(&self) -> Option<usize> {
        self.tabs.get(self.active_tab).map(|tab| tab.id)
    }

    fn sync_active_visibility(&self, cx: &mut Context<Self>) {
        for (index, tab) in self.tabs.iter().enumerate() {
            if let Some(webview) = tab.webview.clone() {
                webview.update(cx, |view, _| {
                    if index == self.active_tab {
                        view.show();
                    } else {
                        view.hide();
                    }
                });
            }
        }
    }

    /// Mirrors the active tab URL into the address bar. Render-owned
    /// (it needs the window) and guarded: never clobbers focused typing,
    /// and no-ops once in sync so it cannot loop renders.
    fn sync_address_bar(&self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.url.clone())
            .unwrap_or_default();
        let focused = self
            .address_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !focused
            && self.address_input.read(cx).value().to_string() != url
        {
            self.address_input.update(cx, |input, cx| {
                input.set_value(url, window, cx);
            });
        }
    }

    /// Navigate the active tab's page. Public for agent-tool wiring and
    /// address-bar input. The address bar syncs from the active tab during
    /// render ([`Self::sync_address_bar`]), so no window is needed here.
    pub fn load_url(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.url = url.to_string();
            if let Some(webview) = tab.webview.clone() {
                webview.update(cx, |view, _| view.load_url(url));
            }
        }
        cx.notify();
    }

    /// Current committed URL, if the webview reports one.
    pub fn current_url(&self, cx: &App) -> Option<String> {
        self.active_webview().and_then(|webview| {
            webview
                .read(cx)
                .raw()
                .url()
                .ok()
                .filter(|url| !url.is_empty())
        })
    }

    /// Evaluate a synchronous script, resolving with wry's JSON-serialized
    /// result. The callback fires on the main thread; the caller awaits the
    /// receiver on a foreground task (never blocking the UI).
    pub fn evaluate_script(
        &self,
        script: &str,
        cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<String>, String> {
        let webview = self
            .active_webview()
            .ok_or_else(|| "The browser has no active tab.".to_string())?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        webview
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

    /// Capture the rendered viewport as JPEG bytes with width and height.
    pub fn take_snapshot(
        &self,
        cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(Vec<u8>, u32, u32), String>>, String> {
        use block2::RcBlock;
        use objc2::runtime::AnyObject;
        use wry::WebViewExtMacOS;

        let webview = self
            .active_webview()
            .ok_or_else(|| "The browser has no active tab.".to_string())?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let wry_wv = webview.read(cx).raw();
        let wk_wv = wry_wv.webview();

        let tx_block = tx.clone();
        let block = RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
            let res = (|| -> Result<(Vec<u8>, u32, u32), String> {
                if !error.is_null() {
                    return Err("WKWebView snapshot returned an error.".to_string());
                }
                if image.is_null() {
                    return Err("WKWebView snapshot returned empty image.".to_string());
                }
                unsafe {
                    let tiff: *mut AnyObject = objc2::msg_send![image, TIFFRepresentation];
                    if tiff.is_null() {
                        return Err("Failed to extract TIFF from snapshot image.".to_string());
                    }
                    let cls = objc2::runtime::AnyClass::get(c"NSBitmapImageRep")
                        .ok_or_else(|| "NSBitmapImageRep class missing.".to_string())?;
                    let rep: *mut AnyObject = objc2::msg_send![cls, imageRepWithData: tiff];
                    if rep.is_null() {
                        return Err("Failed to create NSBitmapImageRep from TIFF.".to_string());
                    }
                    let width: isize = objc2::msg_send![rep, pixelsWide];
                    let height: isize = objc2::msg_send![rep, pixelsHigh];

                    // NSBitmapImageFileTypeJPEG = 3
                    let nil_props: *mut AnyObject = std::ptr::null_mut();
                    let jpeg: *mut AnyObject = objc2::msg_send![
                        rep,
                        representationUsingType: 3isize,
                        properties: nil_props
                    ];
                    if jpeg.is_null() {
                        return Err("Failed to encode snapshot as JPEG.".to_string());
                    }
                    let length: usize = objc2::msg_send![jpeg, length];
                    let bytes_ptr: *const u8 = objc2::msg_send![jpeg, bytes];
                    let bytes = std::slice::from_raw_parts(bytes_ptr, length).to_vec();
                    Ok((bytes, width.max(1) as u32, height.max(1) as u32))
                }
            })();
            if let Ok(mut slot) = tx_block.lock() {
                if let Some(tx) = slot.take() {
                    let _ = tx.send(res);
                }
            }
        });

        unsafe {
            let nil_config: *mut AnyObject = std::ptr::null_mut();
            let _: () = objc2::msg_send![
                &*wk_wv,
                takeSnapshotWithConfiguration: nil_config,
                completionHandler: &*block
            ];
        }

        Ok(rx)
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = self.active_webview() {
            webview.update(cx, |view, _| {
                let _ = view.back();
            });
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let url = self
            .current_url(cx)
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        if let Some(webview) = self.active_webview() {
            webview.update(cx, |view, _| view.load_url(&url));
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible {
            self.sync_active_visibility(cx);
        } else if let Some(webview) = self.active_webview() {
            webview.update(cx, |view, _| view.hide());
        }
    }

    pub fn is_annotating(&self) -> bool {
        self.annotating
    }

    /// Toggles the click-to-annotate picker. While active, hovering
    /// highlights elements and clicking one attaches its description plus a
    /// viewport snapshot to the chat composer.
    pub fn toggle_annotate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.annotating {
            self.stop_annotate(cx);
            cx.notify();
            return;
        }
        let install = match self.evaluate_script(&annotate_install_js(), cx) {
            Ok(receiver) => receiver,
            Err(error) => {
                tracing::warn!("annotation picker install failed: {error}");
                return;
            }
        };
        self.annotating = true;
        cx.notify();
        let view = cx.entity().downgrade();
        let model = self.model.clone();
        self.annotate_task = Some(cx.spawn(async move |this, cx| {
            // Best-effort install confirmation; the picker works regardless.
            let _ = install.await;
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(300))
                    .await;
                let poll = this
                    .update(cx, |this, cx| {
                        this.evaluate_script(&annotate_poll_js(), cx).ok()
                    })
                    .ok()
                    .flatten();
                let Some(poll) = poll else { break };
                let Ok(raw) = poll.await else { break };
                let payload = unwrap_callback_payload(&raw);
                // The callback wrapper has already been removed above.
                let parsed = serde_json::from_str::<serde_json::Value>(&payload).ok();
                let (pick, active) = match parsed {
                    Some(serde_json::Value::Object(mut map)) => {
                        (map.remove("pick"), map.remove("active"))
                    }
                    _ => (None, None),
                };
                match pick {
                    Some(value) if !value.is_null() => {
                        let _ = this.update(cx, |this, cx| {
                            this.finish_annotation(Some(value), cx);
                        });
                        break;
                    }
                    _ => {}
                }
                // Picker cancelled (Escape reports active=false): stop.
                if !active.is_some_and(|value| value.as_bool().unwrap_or(false)) {
                    let _ = this.update(cx, |this, cx| {
                        this.stop_annotate(cx);
                        cx.notify();
                    });
                    break;
                }
                // Still waiting: keep the task alive only while the view does.
                if this.upgrade().is_none() {
                    break;
                }
            }
        }));
    }

    pub fn stop_annotate(&mut self, cx: &mut Context<Self>) {
        self.annotate_task.take();
        self.annotating = false;
        if let Ok(receiver) = self.evaluate_script(&annotate_uninstall_js(), cx) {
            cx.spawn(async move |_this, _cx| {
                let _ = receiver.await;
            })
            .detach();
        }
    }

    /// Formats a recorded pick and hands it to the composer with a viewport
    /// snapshot, like an attached screenshot.
    fn finish_annotation(
        &mut self,
        pick: Option<serde_json::Value>,
        cx: &mut Context<Self>,
    ) {
        self.annotating = false;
        self.annotate_task.take();
        if let Ok(receiver) = self.evaluate_script(&annotate_uninstall_js(), cx) {
            cx.spawn(async move |_this, _cx| {
                let _ = receiver.await;
            })
            .detach();
        }
        let Some(pick) = pick.filter(|value| !value.is_null()) else {
            cx.notify();
            return;
        };
        let tag = pick.get("tag").and_then(|value| value.as_str()).unwrap_or("element");
        let text = pick.get("text").and_then(|value| value.as_str()).unwrap_or("");
        let selector = pick
            .get("selector")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let href = pick.get("href").and_then(|value| value.as_str()).unwrap_or("");
        let page_title = pick.get("title").and_then(|value| value.as_str()).unwrap_or("");
        let page_url = pick.get("url").and_then(|value| value.as_str()).unwrap_or("");
        let rect = pick.get("rect");
        let geometry = rect
            .map(|rect| {
                format!(
                    "{}x{} at ({},{})",
                    rect.get("w").and_then(|value| value.as_u64()).unwrap_or(0),
                    rect.get("h").and_then(|value| value.as_u64()).unwrap_or(0),
                    rect.get("x").and_then(|value| value.as_i64()).unwrap_or(0),
                    rect.get("y").and_then(|value| value.as_i64()).unwrap_or(0),
                )
            })
            .unwrap_or_else(|| "unknown geometry".to_string());
        let mut note = format!(
            "[Browser annotation — {page_title}]({page_url})\nElement: <{tag}> `{selector}` {geometry}"
        );
        if !text.is_empty() {
            note.push_str(&format!("\nContent: \"{text}\""));
        }
        if !href.is_empty() {
            note.push_str(&format!("\nLink: {href}"));
        }
        // Viewport snapshot alongside the note, like an attached screenshot.
        let snapshot = self.take_snapshot(cx).ok();
        let model = self.model.clone();
        cx.spawn(async move |_this, cx| {
            let mut images = Vec::new();
            if let Some(snapshot) = snapshot {
                if let Ok(Ok((bytes, _, _))) = snapshot.await {
                    if !bytes.is_empty() {
                        images.push(threadlane_protocol::ImageAttachment {
                            display_name: "browser-annotation.jpg".to_string(),
                            data_url: format!(
                                "data:image/jpeg;base64,{}",
                                base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD,
                                    bytes
                                )
                            ),
                        });
                    }
                }
            }
            let _ = model.update(cx, |state, cx| {
                state.requested_composer_inserts.push(RequestedComposerInsert {
                    text: note,
                    images,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn tab_title(url: &str) -> String {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = bare.split('/').next().unwrap_or(bare);
    let short: String = host.chars().take(24).collect();
    if short.len() < host.len() {
        format!("{short}…")
    } else {
        short
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_address_bar(window, cx);
        let webview = self.active_webview();
        let active_id = self.active_tab_id();
        let tabs = self.tabs();
        let annotating = self.annotating;
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
                            .accessibility_label("Go back")
                            .tooltip("Go back")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.go_back(cx);
                            })),
                    )
                    .child(
                        Button::new("browser-reload")
                            .icon(Icon::default().path("icons/refresh-cw.svg"))
                            .accessibility_label("Reload page")
                            .tooltip("Reload page")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.reload(cx);
                            })),
                    )
                    .child(
                        Button::new("browser-annotate")
                            .icon(Icon::default().path("icons/crosshair.svg"))
                            .accessibility_label(if annotating {
                                "Stop annotating"
                            } else {
                                "Annotate page element"
                            })
                            .tooltip(if annotating {
                                "Annotating — click a page element (Esc cancels)"
                            } else {
                                "Annotate: pick a page element into the composer"
                            })
                            .ghost()
                            .xsmall()
                            .selected(annotating)
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.toggle_annotate(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.address_input).aria_label("Browser address")),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(tabs.into_iter().map(|(id, url)| {
                        let selected = Some(id) == active_id;
                        let title = tab_title(&url);
                        div()
                            .flex()
                            .items_center()
                            .rounded_md()
                            .bg(if selected {
                                cx.theme().primary.opacity(0.12)
                            } else {
                                gpui::transparent_black()
                            })
                            .child(
                                Button::new(SharedString::from(format!("browser-tab-{id}")))
                                    .label(title.clone())
                                    .accessibility_label(format!("Show browser tab {title}"))
                                    .tooltip(url.clone())
                                    .ghost()
                                    .xsmall()
                                    .selected(selected)
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        this.switch_tab(id, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("browser-tab-close-{id}")))
                                    .icon(IconName::Close)
                                    .accessibility_label(format!("Close tab {title}"))
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _event, window, cx| {
                                        this.close_tab(id, window, cx);
                                    })),
                            )
                    }))
                    .child(
                        Button::new("browser-new-tab")
                            .icon(IconName::Plus)
                            .accessibility_label("New browser tab")
                            .tooltip("New tab")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.open_tab(DEFAULT_URL, window, cx);
                            })),
                    )
                    .when(annotating, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child("Click a page element — Esc cancels"),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .overflow_hidden()
                    .children(webview),
            )
    }
}

#[cfg(test)]
mod browser_tabs_tests {
    // Narrow import: `use super::*` pulls GPUI macros into test scope and
    // blows the recursion limit (same hazard as threadlane-ui-mirror).
    use super::tab_title;

    #[test]
    fn tab_titles_show_hosts_compactly() {
        assert_eq!(tab_title("https://example.com/some/long/path"), "example.com");
        assert_eq!(tab_title("https://gpui-kit.com"), "gpui-kit.com");
        assert_eq!(
            tab_title("https://very-long-subdomain-name.example.com/x"),
            "very-long-subdomain-name…"
        );
    }
}
