//! Embedded browser surface for the right panel.
//!
//! Thin wrapper over `wry`: native webviews hosted per platform (see
//! `webview.rs` — WKWebView on macOS, WebKitGTK on Linux/X11, WebView2 on
//! Windows). Safari-style address bar, multiple tabs, and a click-to-annotate
//! picker whose picks land in the chat composer.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::WindowExt;
use threadlane_ui_state::{AppState, RequestedComposerInsert};

use super::address::{resolve_address, search_url, AddressTarget};
use super::scripts::{
    annotate_install_js, annotate_poll_js, annotate_uninstall_js, unwrap_callback_payload,
};

const DEFAULT_URL: &str = "https://github.com/wheregmis/threadlane";

struct BrowserTab {
    id: usize,
    url: String,
    /// URL requested but not yet committed by the webview. While set, the
    /// address bar shows it instead of the live URL, which still reports the
    /// previous page during provisional navigation.
    pending_url: Option<String>,
    webview: Option<Entity<super::webview::ComposedWebView>>,
}

pub struct BrowserView {
    focus_handle: FocusHandle,
    model: Entity<AppState>,
    address_input: Entity<InputState>,
    tabs: Vec<BrowserTab>,
    active_tab: usize,
    tab_scroll: ScrollHandle,
    revealed_tab: Option<(usize, usize)>,
    next_tab_id: usize,
    annotating: bool,
    annotate_task: Option<Task<()>>,
    url_watch_task: Option<Task<()>>,
    visible: bool,
    _annotation_escape: Subscription,
}

impl BrowserView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            tab_scroll: ScrollHandle::new(),
            revealed_tab: None,
            next_tab_id: 1,
            annotating: false,
            annotate_task: None,
            url_watch_task: None,
            visible: false,
            _annotation_escape: Self::annotation_escape_subscription(cx),
        };
        this.open_tab(DEFAULT_URL, window, cx);
        this
    }

    fn annotation_escape_subscription(cx: &mut Context<Self>) -> Subscription {
        let browser = cx.entity().downgrade();
        cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key != "escape" || event.keystroke.modifiers != Modifiers::default()
            {
                return;
            }
            let _ = browser.update(cx, |browser, cx| {
                if browser.annotating && browser.focus_handle.contains_focused(window, cx) {
                    browser.stop_annotate(cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            });
        })
    }

    fn navigate_to_input(&mut self, raw: &str, cx: &mut Context<Self>) {
        let target = resolve_address(raw);
        let url = match target {
            None => return,
            Some(AddressTarget::Url(url)) => url,
            Some(AddressTarget::Search(query)) => search_url(&query),
        };
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.pending_url = Some(url.clone());
            if let Some(webview) = tab.webview.clone() {
                webview.update(cx, |view, _| view.load_url(&url));
            }
        }
        cx.notify();
    }

    fn active_webview(&self) -> Option<Entity<super::webview::ComposedWebView>> {
        self.tabs
            .get(self.active_tab)
            .and_then(|tab| tab.webview.clone())
    }

    fn spawn_webview(
        &self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<super::webview::ComposedWebView>, String> {
        let builder = wry::WebViewBuilder::new();
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let builder = builder.with_initialization_script(super::scripts::console_interceptor_js());

        #[cfg(target_os = "macos")]
        let webview = {
            use raw_window_handle::HasWindowHandle;

            let window_handle = window
                .window_handle()
                .map_err(|_| "Browser window is unavailable. Retry Open link…".to_string())?;
            let wry_webview = builder.build_as_child(&window_handle).map_err(|_| {
                "Browser could not start. Retry Open link… or choose Open in default browser."
                    .to_string()
            })?;
            cx.new(|cx| super::webview::ComposedWebView::new(wry_webview, window, cx))
        };

        // On Windows the WebView2 controller is a child HWND of the GPUI
        // window — composition surfaces are DirectComposition visuals an
        // HWND can't join, so there is no reparenting step.
        #[cfg(target_os = "windows")]
        let webview = {
            use raw_window_handle::HasWindowHandle;

            let window_handle = HasWindowHandle::window_handle(window)
                .map_err(|_| "Browser window is unavailable. Retry Open link…".to_string())?;
            let wry_webview = builder.build_as_child(&window_handle).map_err(|error| {
                format!(
                    "Browser could not start. Retry Open link… or choose Open in default browser. ({error})"
                )
            })?;
            cx.new(|cx| super::webview::ComposedWebView::new(wry_webview, window, cx))
        };

        // On Linux the webview must be built as a child of the composition
        // surface's X window, so the surface is created first; see webview.rs.
        #[cfg(target_os = "linux")]
        let webview = {
            use super::webview as wv;

            wv::ensure_gtk_init()?;
            wv::ensure_gtk_pump(cx);
            let surface = window
                .enable_window_composition()
                .and_then(|composition| composition.create_native_surface())
                .ok()
                // A Wayland surface has no XID to parent into; drop it so the
                // fallback below can fail cleanly with the X11 requirement.
                .filter(|surface| wv::surface_xid(surface).is_some());
            let wry_webview = wv::build_child(builder, surface.as_ref(), window)?;
            cx.new(|cx| super::webview::ComposedWebView::new(wry_webview, surface, window, cx))
        };

        webview.update(cx, |view, _| view.load_url(url));
        // Inactive tabs stay hidden until selected.
        webview.update(cx, |view, cx| {
            view.hide();
            cx.notify();
        });
        Ok(webview)
    }

    /// Open a URL in a new tab and switch to it. Needs a window to create
    /// the tab's webview; callers without one use [`Self::load_url`], which
    /// reuses the active tab's existing view.
    pub fn open_tab(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.try_open_tab(url, window, cx) {
            window.push_notification(error, cx);
        }
    }

    pub fn focus_address(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let url = tab.pending_url.as_ref().unwrap_or(&tab.url).clone();
            self.address_input
                .update(cx, |input, cx| input.set_value(url, window, cx));
        }
        self.address_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    pub fn try_open_tab(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let webview = self.spawn_webview(url, window, cx)?;
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(BrowserTab {
            id,
            url: url.to_string(),
            pending_url: Some(url.to_string()),
            webview: Some(webview),
        });
        self.switch_tab(id, window, cx);
        Ok(())
    }

    /// Close a tab. The last tab becomes a fresh default tab instead of
    /// leaving the surface empty.
    pub fn close_tab(&mut self, id: usize, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            let url = DEFAULT_URL.to_string();
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.pending_url = Some(url.clone());
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
                webview.update(cx, |view, cx| {
                    view.hide();
                    cx.notify();
                });
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
            let webview = match self.spawn_webview(&url, window, cx) {
                Ok(webview) => webview,
                Err(error) => {
                    window.push_notification(error, cx);
                    return;
                }
            };
            self.tabs[position].pending_url = Some(url.clone());
            self.tabs[position].webview = Some(webview);
        }
        self.sync_active_visibility(cx);
        cx.notify();
    }

    pub fn tabs(&self, cx: &App) -> Vec<(usize, String)> {
        self.tabs
            .iter()
            .map(|tab| {
                let url = tab
                    .webview
                    .as_ref()
                    .and_then(|webview| webview.read(cx).raw().url().ok())
                    .filter(|url| !url.is_empty())
                    .unwrap_or_else(|| tab.url.clone());
                (tab.id, url)
            })
            .collect()
    }

    pub fn active_tab_id(&self) -> Option<usize> {
        self.tabs.get(self.active_tab).map(|tab| tab.id)
    }

    fn sync_active_visibility(&self, cx: &mut Context<Self>) {
        for (index, tab) in self.tabs.iter().enumerate() {
            if let Some(webview) = tab.webview.clone() {
                webview.update(cx, |view, cx| {
                    if self.visible && index == self.active_tab {
                        view.show();
                    } else {
                        view.hide();
                    }
                    cx.notify();
                });
            }
        }
    }

    /// Mirrors the active tab URL into the address bar. Render-owned
    /// (it needs the window) and guarded: never clobbers focused typing,
    /// and no-ops once in sync so it cannot loop renders.
    fn sync_address_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Prefer the live URL: in-page navigation (SPA pushes, redirects,
        // link clicks) never updates `tab.url`, so the bar went stale. While
        // a requested navigation is still provisional WKWebView reports the
        // old page, so keep showing the pending URL until the live one
        // commits to it or diverges (redirect / in-page nav wins the race).
        let url = self
            .tabs
            .get_mut(self.active_tab)
            .map(|tab| {
                let live = tab
                    .webview
                    .as_ref()
                    .and_then(|webview| webview.read(cx).raw().url().ok())
                    .filter(|url| !url.is_empty());
                match (tab.pending_url.clone(), live) {
                    (Some(pending), Some(live)) if live == pending => {
                        tab.url = live.clone();
                        tab.pending_url = None;
                        live
                    }
                    (Some(_), Some(live)) if live != tab.url => {
                        tab.pending_url = None;
                        tab.url = live.clone();
                        live
                    }
                    (Some(pending), _) => pending,
                    (None, Some(live)) => live,
                    (None, None) => tab.url.clone(),
                }
            })
            .unwrap_or_default();
        let focused = self
            .address_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if !focused && self.address_input.read(cx).value().to_string() != url {
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
            tab.pending_url = Some(url.to_string());
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
    /// `crop` is an optional `[x, y, w, h]` rect in view points (≈ CSS pixels
    /// at page zoom 1); `None` snapshots the whole viewport.
    #[cfg(target_os = "macos")]
    pub fn take_snapshot(
        &self,
        crop: Option<[f64; 4]>,
        cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(Vec<u8>, u32, u32), String>>, String> {
        use block2::RcBlock;
        use objc2::runtime::AnyObject;
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
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
            // WKSnapshotConfiguration::rect is in WKWebView view coordinates;
            // when unset the full bounds are captured. The crop comes from the
            // picker's viewport-space getBoundingClientRect union, which lines
            // up while page zoom is 1.
            let config: *mut AnyObject = if let Some([x, y, w, h]) = crop {
                let class = objc2::runtime::AnyClass::get(c"WKSnapshotConfiguration")
                    .ok_or_else(|| "WKSnapshotConfiguration class missing.".to_string())?;
                let config: *mut AnyObject = objc2::msg_send![class, new];
                if config.is_null() {
                    return Err("Failed to create WKSnapshotConfiguration.".to_string());
                }
                let rect = CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
                let _: () = objc2::msg_send![config, setRect: rect];
                config
            } else {
                std::ptr::null_mut()
            };
            let _: () = objc2::msg_send![
                &*wk_wv,
                takeSnapshotWithConfiguration: config,
                completionHandler: &*block
            ];
            // `new` returned a +1 object; WebKit retains it for the call.
            if !config.is_null() {
                let _: () = objc2::msg_send![config, release];
            }
        }

        Ok(rx)
    }

    /// Capture the rendered viewport as PNG bytes with width and height.
    /// `crop` is an optional `[x, y, w, h]` rect in view points; `None`
    /// snapshots the whole viewport.
    #[cfg(target_os = "linux")]
    pub fn take_snapshot(
        &self,
        crop: Option<[f64; 4]>,
        cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(Vec<u8>, u32, u32), String>>, String> {
        use webkit2gtk::WebViewExt as _;
        use wry::WebViewExtUnix as _;

        let webview = self
            .active_webview()
            .ok_or_else(|| "The browser has no active tab.".to_string())?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let wk_wv = webview.read(cx).raw().webview();

        // The reply runs on the GTK main context, which the pump spawned in
        // `spawn_webview` keeps iterating from the UI thread.
        wk_wv.snapshot(
            webkit2gtk::SnapshotRegion::Visible,
            webkit2gtk::SnapshotOptions::NONE,
            webkit2gtk::gio::Cancellable::NONE,
            move |result| {
                let res = result
                    .map_err(|error| format!("WebKitGTK snapshot failed: {error}"))
                    .and_then(|surface| snapshot_to_png(&surface, crop));
                if let Ok(mut slot) = tx.lock() {
                    if let Some(tx) = slot.take() {
                        let _ = tx.send(res);
                    }
                }
            },
        );
        Ok(rx)
    }

    /// Capture the rendered viewport as PNG bytes with width and height.
    /// `crop` is an optional `([x, y, w, h], viewport_w, viewport_h)` tuple in
    /// CSS pixels; `None` snapshots the whole viewport. Screenshots go through
    /// the DevTools protocol because `wry` exposes no `CapturePreview`
    /// wrapper; the reply is base64 PNG data.
    #[cfg(target_os = "windows")]
    pub fn take_snapshot(
        &self,
        crop: Option<([f64; 4], f64, f64)>,
        cx: &App,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(Vec<u8>, u32, u32), String>>, String> {
        use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
        use wry::WebViewExtWindows as _;

        let webview = self
            .active_webview()
            .ok_or_else(|| "The browser has no active tab.".to_string())?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let wv2 = webview.read(cx).raw().webview();

        // WebView2 dispatches completion callbacks on the UI thread, so the
        // handler only forwards the raw reply; base64 and PNG work happens on
        // a worker thread instead of stalling the GPUI message pump.
        let (raw_tx, raw_rx) = std::sync::mpsc::channel();
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                let _ = raw_tx.send(
                    result
                        .map_err(|error| format!("WebView2 screenshot failed: {error}"))
                        .map(|()| json.to_string()),
                );
                Ok(())
            }));
        std::thread::spawn(move || {
            let res = raw_rx
                .recv()
                .map_err(|_| "WebView2 screenshot callback dropped.".to_string())
                .and_then(|result| result)
                .and_then(|json| decode_capture_response(&json, crop));
            if let Ok(mut slot) = tx.lock() {
                if let Some(tx) = slot.take() {
                    let _ = tx.send(res);
                }
            }
        });
        unsafe {
            wv2.CallDevToolsProtocolMethod(
                windows_core::w!("Page.captureScreenshot"),
                windows_core::w!(r#"{"format":"png","captureBeyondViewport":false}"#),
                &handler,
            )
            .map_err(|error| format!("WebView2 screenshot failed to start: {error}"))?;
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

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = self.active_webview() {
            webview.update(cx, |view, _| {
                // wry has no forward(); go straight to the history entry.
                let _ = view.raw().evaluate_script("history.forward();");
            });
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = self.active_webview() {
            // wry reload() is a real reload: bypassing the cache is the user's
            // call, unlike re-navigating which resubmits nothing for POSTs.
            let _ = webview.update(cx, |view, _| view.raw().reload());
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible {
            self.start_url_watch(cx);
        } else {
            self.url_watch_task.take();
        }
        self.sync_active_visibility(cx);
    }

    /// Adopts live webview URL changes so in-page navigation (link clicks,
    /// SPA pushes, history moves) refreshes the address bar. wry gives no
    /// commit callback, so this polls while the panel is visible and renders
    /// only when the URL actually changed.
    fn start_url_watch(&mut self, cx: &mut Context<Self>) {
        if self.url_watch_task.is_some() {
            return;
        }
        self.url_watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(700))
                    .await;
                let keep_running = this
                    .update(cx, |this, cx| {
                        if !this.visible {
                            return false;
                        }
                        let Some(tab) = this.tabs.get_mut(this.active_tab) else {
                            return true;
                        };
                        let live = tab
                            .webview
                            .as_ref()
                            .and_then(|webview| webview.read(cx).raw().url().ok())
                            .filter(|url| !url.is_empty());
                        if let Some(live) = live {
                            if live != tab.url {
                                // Committed — to the pending request or a
                                // redirect / in-page move. Adopt it.
                                tab.url = live;
                                tab.pending_url = None;
                                cx.notify();
                            }
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    pub fn is_annotating(&self) -> bool {
        self.annotating
    }

    /// Toggles the annotate overlay. While active, the in-page picker selects
    /// elements and collects an optional comment; attaching hands the
    /// description plus an element-cropped snapshot to the chat composer.
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
        self.focus_handle.focus(window, cx);
        self.annotating = true;
        cx.notify();
        self.annotate_task = Some(cx.spawn(async move |this, cx| {
            // The picker bails with "retry" while the document is mid-load;
            // reinstall briefly rather than dropping the user's session.
            let mut installed = install
                .await
                .map(|raw| unwrap_callback_payload(&raw))
                .unwrap_or_default();
            let mut install_retries = 0u8;
            while installed.trim_matches('"') == "retry" && install_retries < 3 {
                install_retries += 1;
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(400))
                    .await;
                let retry = this
                    .update(cx, |this, cx| {
                        this.evaluate_script(&annotate_install_js(), cx).ok()
                    })
                    .ok()
                    .flatten();
                installed = match retry {
                    Some(receiver) => receiver
                        .await
                        .map(|raw| unwrap_callback_payload(&raw))
                        .unwrap_or_default(),
                    None => break,
                };
            }
            let mut misses = 0u8;
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
                // Transient eval failures (navigation, teardown) shouldn't end
                // the session; several misses in a row mean the page is gone.
                let Some(poll) = poll else {
                    misses += 1;
                    if misses >= 8 {
                        break;
                    }
                    continue;
                };
                let Ok(raw) = poll.await else {
                    misses += 1;
                    if misses >= 8 {
                        break;
                    }
                    continue;
                };
                misses = 0;
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
                    break;
                }
                // Still waiting: keep the task alive only while the view does.
                if this.upgrade().is_none() {
                    break;
                }
            }
            // Any exit that didn't deliver a pick still owes the view a
            // cleared annotating flag (finish_annotation does its own).
            let _ = this.update(cx, |this, cx| {
                if this.annotating {
                    this.stop_annotate(cx);
                    cx.notify();
                }
            });
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
    fn finish_annotation(&mut self, pick: Option<serde_json::Value>, cx: &mut Context<Self>) {
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
        let note = format_annotation_note(&pick);
        // Snapshot cropped to the annotated element union so the attachment
        // shows context around the target instead of the whole viewport.
        #[cfg(not(target_os = "windows"))]
        let crop = pick.get("crop").and_then(|rect| {
            let get = |key: &str| rect.get(key).and_then(|value| value.as_f64());
            Some([get("x")?, get("y")?, get("w")?, get("h")?])
        });
        // Windows PNGs come back in device pixels, so the CSS-pixel rect
        // travels with the viewport it was measured in for scaling.
        #[cfg(target_os = "windows")]
        let crop = pick.get("crop").and_then(|rect| {
            let get = |key: &str| rect.get(key).and_then(|value| value.as_f64());
            let viewport = pick.get("viewport")?;
            Some((
                [get("x")?, get("y")?, get("w")?, get("h")?],
                viewport.get("w").and_then(|w| w.as_f64())?,
                viewport.get("h").and_then(|h| h.as_f64())?,
            ))
        });
        let snapshot = self.take_snapshot(crop, cx).ok();
        let model = self.model.clone();
        cx.spawn(async move |_this, cx| {
            let mut images = Vec::new();
            if let Some(snapshot) = snapshot {
                if let Ok(Ok((bytes, _, _))) = snapshot.await {
                    if !bytes.is_empty() {
                        // macOS snapshots are JPEG, Linux/Windows ones PNG.
                        #[cfg(target_os = "macos")]
                        let (ext, mime) = ("jpg", "image/jpeg");
                        #[cfg(not(target_os = "macos"))]
                        let (ext, mime) = ("png", "image/png");
                        images.push(threadlane_protocol::ImageAttachment {
                            display_name: format!("browser-annotation.{ext}"),
                            data_url: format!(
                                "data:{mime};base64,{}",
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
                state
                    .requested_composer_inserts
                    .push(RequestedComposerInsert {
                        text: note,
                        images,
                        session_id: None,
                        work_dir: None,
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

#[cfg(test)]
use threadlane_ui_kit::browser_tab_title as tab_title;

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_address_bar(window, cx);
        let webview = self.active_webview();
        let active_id = self.active_tab_id();
        let selection = active_id.map(|id| (id, self.active_tab));
        if self.revealed_tab != selection {
            self.tab_scroll.scroll_to_item(self.active_tab);
            self.revealed_tab = selection;
        }
        let tabs = self.tabs(cx);
        threadlane_ui_kit::browser_chrome(
            &self.address_input,
            &tabs,
            active_id,
            &self.tab_scroll,
            self.annotating,
            self.current_url(cx).as_deref().is_some_and(|url| url.starts_with("https://")),
            cx.listener(|this, action: &threadlane_ui_kit::BrowserAction, window, cx| {
                use threadlane_ui_kit::BrowserAction;
                match *action {
                    BrowserAction::Back => this.go_back(cx),
                    BrowserAction::Forward => this.go_forward(cx),
                    BrowserAction::Reload => this.reload(cx),
                    BrowserAction::ToggleAnnotate => this.toggle_annotate(window, cx),
                    BrowserAction::SelectTab(id) => this.switch_tab(id, window, cx),
                    BrowserAction::CloseTab(id) => this.close_tab(id, window, cx),
                    BrowserAction::NewTab => this.open_tab(DEFAULT_URL, window, cx),
                }
            }),
            cx,
        )
        .track_focus(&self.focus_handle)
        .when(!self.tabs.is_empty(), |panel| {
            panel.child(threadlane_ui_kit::browser_viewport(cx).children(webview))
        })
    }
}

/// Formats a picked annotation into a compact, human-readable note for the composer.
/// If a user comment was typed, places the comment clearly at the top.
pub fn format_annotation_note(pick: &serde_json::Value) -> String {
    let comment = pick
        .get("comment")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let page_title = pick
        .get("title")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let page_url = pick
        .get("url")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();

    let element_line = |element: &serde_json::Value| {
        let tag = element
            .get("tag")
            .and_then(|value| value.as_str())
            .unwrap_or("element");
        let selector = element
            .get("selector")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let mut line = format!("<{tag}>");
        if !selector.is_empty() {
            line.push_str(&format!(" `{selector}`"));
        }
        if let Some(name) = ["name", "text"].iter().find_map(|key| {
            element
                .get(key)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
        }) {
            line.push_str(&format!(" \"{name}\""));
        }
        if let Some(href) = element
            .get("href")
            .and_then(|value| value.as_str())
            .filter(|href| !href.is_empty())
        {
            line.push_str(&format!(" → {href}"));
        }
        line
    };

    let elements: Vec<&serde_json::Value> = pick
        .get("elements")
        .and_then(|value| value.as_array())
        .map(|elements| elements.iter().collect())
        .unwrap_or_default();

    let context_tag = if !page_title.is_empty() && !page_url.is_empty() {
        format!("[Browser: {page_title}]({page_url})")
    } else if !page_url.is_empty() {
        format!("[Browser: {page_url}]({page_url})")
    } else if !page_title.is_empty() {
        format!("[Browser: {page_title}]")
    } else {
        "[Browser annotation]".to_string()
    };

    let mut note = String::new();
    if !comment.is_empty() {
        note.push_str(comment);
        note.push_str("\n\n");
    }

    if elements.len() == 1 {
        note.push_str(&format!("{context_tag} — {}", element_line(elements[0])));
    } else if !elements.is_empty() {
        note.push_str(&context_tag);
        for element in &elements {
            note.push_str(&format!("\n• {}", element_line(element)));
        }
    } else {
        note.push_str(&context_tag);
    }

    note
}

/// Rasterize a WebKitGTK snapshot surface to PNG. `crop`, when present, is an
/// `[x, y, w, h]` rect in the snapshot's coordinate space; a cropped image is
/// produced by repainting the shifted source into a sized image surface.
#[cfg(target_os = "linux")]
fn snapshot_to_png(
    surface: &cairo::Surface,
    crop: Option<[f64; 4]>,
) -> Result<(Vec<u8>, u32, u32), String> {
    let image = match crop {
        Some([x, y, w, h]) => {
            let width = w.round().max(1.) as i32;
            let height = h.round().max(1.) as i32;
            let image = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
                .map_err(|error| format!("Snapshot crop surface failed: {error}"))?;
            let context = cairo::Context::new(&image)
                .map_err(|error| format!("Snapshot crop context failed: {error}"))?;
            context
                .set_source_surface(surface, -x, -y)
                .map_err(|error| format!("Snapshot crop failed: {error}"))?;
            context
                .paint()
                .map_err(|error| format!("Snapshot crop paint failed: {error}"))?;
            drop(context);
            image
        }
        None => surface
            .map_to_image(None)
            .map_err(|error| format!("Snapshot rasterization failed: {error}"))?
            .to_owned(),
    };
    let width = image.width().max(1) as u32;
    let height = image.height().max(1) as u32;
    let mut bytes = Vec::new();
    image
        .write_to_png(&mut bytes)
        .map_err(|error| format!("Snapshot PNG encode failed: {error}"))?;
    Ok((bytes, width, height))
}

/// Decode the base64 PNG inside a `Page.captureScreenshot` JSON reply.
/// `crop` pairs the rect with the CSS-pixel viewport it was measured in; the
/// rect is scaled into the screenshot's pixel space, which differs under
/// display scaling or page zoom. Uncropped shots skip the PNG decode
/// entirely: the dimensions come straight from the IHDR header.
#[cfg(target_os = "windows")]
fn decode_capture_response(
    json: &str,
    crop: Option<([f64; 4], f64, f64)>,
) -> Result<(Vec<u8>, u32, u32), String> {
    let data = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| {
            value
                .get("data")
                .and_then(|data| data.as_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| "WebView2 screenshot returned no image data.".to_string())?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data)
        .map_err(|error| format!("WebView2 screenshot data is not base64: {error}"))?;
    match crop {
        None => png_dimensions(&bytes).map(|(width, height)| (bytes, width, height)),
        Some((rect, viewport_w, viewport_h)) => {
            encode_cropped_png(&bytes, rect, viewport_w, viewport_h)
        }
    }
}

/// Read `width`/`height` from a PNG's IHDR chunk without decoding pixels.
#[cfg(target_os = "windows")]
fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let ok = bytes.len() >= 24
        && bytes.starts_with(&[137, b'P', b'N', b'G', 13, 10, 26, 10])
        && bytes[12..16] == *b"IHDR";
    if !ok {
        return Err("WebView2 screenshot is not a PNG.".to_string());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap_or_default());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap_or_default());
    Ok((width, height))
}

/// Re-encode screenshot `bytes` as PNG, cropped to `rect` (`[x, y, w, h]` in
/// CSS pixels inside a `viewport_w`x`viewport_h` viewport — scaled to the
/// screenshot's pixel space). Out-of-bounds rects are clamped.
#[cfg(target_os = "windows")]
fn encode_cropped_png(
    bytes: &[u8],
    rect: [f64; 4],
    viewport_w: f64,
    viewport_h: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|error| format!("WebView2 screenshot is not a PNG: {error}"))?;
    let scale_x = image.width() as f64 / viewport_w.max(1.);
    let scale_y = image.height() as f64 / viewport_h.max(1.);
    let [x, y, w, h] = rect;
    let left = (x * scale_x).round().max(0.).min(image.width() as f64 - 1.) as u32;
    let top = (y * scale_y).round().max(0.).min(image.height() as f64 - 1.) as u32;
    let width = (w * scale_x)
        .round()
        .max(1.)
        .min(image.width() as f64 - left as f64) as u32;
    let height = (h * scale_y)
        .round()
        .max(1.)
        .min(image.height() as f64 - top as f64) as u32;
    let cropped = image.crop_imm(left, top, width, height);
    let width = cropped.width().max(1);
    let height = cropped.height().max(1);
    let mut out = Vec::new();
    cropped
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Png,
        )
        .map_err(|error| format!("Snapshot PNG encode failed: {error}"))?;
    Ok((out, width, height))
}

#[cfg(test)]
mod browser_tabs_tests {
    // Narrow import: `use super::*` pulls GPUI macros into test scope and
    // blows the recursion limit (same hazard as threadlane-ui-mirror).
    use super::{tab_title, BrowserTab, BrowserView};
    use gpui::{
        div, px, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
        ParentElement, Render, Styled, TestAppContext, Window,
    };
    use gpui_component::input::InputState;
    use threadlane_ui_state::AppState;

    gpui::actions!(browser_tests, [FallbackEscape]);
    struct BrowserHost {
        browser: Entity<BrowserView>,
        width: f32,
        fallback_escapes: usize,
    }

    impl Render for BrowserHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .on_action(cx.listener(|this, _: &FallbackEscape, _, _| {
                    this.fallback_escapes += 1;
                }))
                .w(px(self.width))
                .h(px(480.0))
                .child(self.browser.clone())
        }
    }

    #[gpui::test]
    fn overflowing_tabs_preserve_new_tab_control(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| cx.bind_keys([gpui::KeyBinding::new("escape", FallbackEscape, None)]));
        let model = cx.new(|_| AppState::default());
        let (host, cx) = cx.add_window_view(move |window, cx| {
            let browser = cx.new(|cx| BrowserView {
                focus_handle: cx.focus_handle(),
                model,
                address_input: cx.new(|cx| {
                    InputState::new(window, cx).default_value(format!(
                        "https://example.com/{}?query=long-address",
                        "deeply-nested-path/".repeat(40)
                    ))
                }),
                tabs: Vec::new(),
                active_tab: 0,
                tab_scroll: gpui::ScrollHandle::new(),
                revealed_tab: None,
                next_tab_id: 12,
                annotating: false,
                annotate_task: None,
                url_watch_task: None,
                visible: false,
                _annotation_escape: BrowserView::annotation_escape_subscription(cx),
            });
            browser.update(cx, |browser, _| {
                // Exercise the real toolbar without constructing native webviews.
                browser.tabs = (0..12)
                    .map(|id| BrowserTab {
                        id,
                        url: format!("https://long-subdomain-{id}.example.com"),
                        pending_url: None,
                        webview: None,
                    })
                    .collect();
            });
            BrowserHost {
                browser,
                width: 320.0,
                fallback_escapes: 0,
            }
        });
        for annotating in [false, true] {
            for width in [320.0, 480.0, 640.0] {
                host.update(cx, |host, cx| {
                    host.width = width;
                    host.browser.update(cx, |browser, cx| {
                        browser.annotating = annotating;
                        cx.notify();
                    });
                    cx.notify();
                });
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let strip = cx
                    .debug_bounds("browser-tab-strip")
                    .expect("tab strip rendered");
                let add = cx
                    .debug_bounds("browser-new-tab-control")
                    .expect("new tab rendered");
                assert!(
                    strip.size.width >= px(80.0),
                    "tab strip collapsed at {width}, annotating={annotating}: {strip:?}"
                );
                assert!(strip.right() <= add.left(), "tabs overlap New tab");
                assert!(add.size.width >= px(16.0), "New tab collapsed");
                assert!(add.right() <= px(width), "New tab overflows panel");
                let address = cx
                    .debug_bounds("browser-address-field")
                    .expect("address rendered");
                assert!(
                    address.size.width >= px(120.0),
                    "address field collapsed at {width}"
                );
                assert!(
                    address.right() <= px(width - 8.0),
                    "address field overflows at {width}"
                );
                let browser = host.read_with(cx, |host, _| host.browser.clone());
                for index in [11, 0] {
                    browser.update(cx, |browser, cx| {
                        browser.active_tab = index;
                        cx.notify();
                    });
                    cx.update(|window, cx| window.draw(cx).clear(cx));
                    browser.read_with(cx, |browser, _| {
                        let viewport = browser.tab_scroll.bounds();
                        let tab = browser.tab_scroll.bounds_for_item(index).unwrap();
                        let offset = browser.tab_scroll.offset().x;
                        assert!(
                            tab.left() + offset >= viewport.left(),
                            "selected tab hidden on left"
                        );
                        assert!(
                            tab.right() + offset <= viewport.right(),
                            "selected tab hidden on right"
                        );
                    });
                }
                // An unrelated render must not snap manual scrolling back to selection.
                browser.update(cx, |browser, cx| {
                    browser
                        .tab_scroll
                        .set_offset(gpui::point(px(-100.0), px(0.0)));
                    cx.notify();
                });
                cx.update(|window, cx| window.draw(cx).clear(cx));
                browser.read_with(cx, |browser, _| {
                    assert_eq!(browser.tab_scroll.offset().x, px(-100.0));
                });
            }
        }
        let browser = host.read_with(cx, |host, _| host.browser.clone());
        cx.update(|window, cx| {
            let focus = browser.read(cx).focus_handle.clone();
            focus.focus(window, cx);
            assert!(browser.read(cx).focus_handle.contains_focused(window, cx));
            // Back, Forward, Reload, Annotate, then the address input.
            for _ in 0..5 {
                window.focus_next(cx);
            }
            assert!(browser
                .read(cx)
                .address_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
            assert!(browser.read(cx).focus_handle.contains_focused(window, cx));
        });
        cx.update(|window, cx| {
            browser.update(cx, |browser, cx| {
                let before = browser.tabs(cx);
                let active = browser.active_tab_id();
                let next_id = browser.next_tab_id;
                // Test windows have no native handle: failure is recoverable and atomic.
                assert!(browser
                    .try_open_tab("http://localhost:3000/", window, cx)
                    .is_err());
                assert_eq!(browser.tabs(cx), before);
                assert_eq!(browser.active_tab_id(), active);
                assert_eq!(browser.next_tab_id, next_id);
                browser.focus_address(window, cx);
                assert!(browser
                    .address_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window));
            })
        });
        browser.update(cx, |browser, cx| {
            browser.annotating = true;
            cx.notify();
        });
        cx.simulate_keystrokes("shift-escape");
        browser.read_with(cx, |browser, _| assert!(browser.annotating));
        cx.simulate_keystrokes("escape");
        browser.read_with(cx, |browser, _| assert!(!browser.annotating));
        host.read_with(cx, |host, _| assert_eq!(host.fallback_escapes, 0));
        // With no annotation to dismiss, the surrounding UI keeps its Escape action.
        cx.simulate_keystrokes("escape");
        host.read_with(cx, |host, _| assert_eq!(host.fallback_escapes, 1));
        browser.update(cx, |browser, cx| {
            browser.annotating = true;
            cx.notify();
        });
        cx.update(|window, cx| window.blur(cx));
        cx.simulate_keystrokes("escape");
        browser.read_with(cx, |browser, _| assert!(browser.annotating));
    }

    #[test]
    fn tab_titles_show_hosts_compactly() {
        assert_eq!(
            tab_title("https://example.com/some/long/path"),
            "example.com"
        );
        assert_eq!(tab_title("https://gpui-kit.com"), "gpui-kit.com");
        assert_eq!(
            tab_title("https://very-long-subdomain-name.example.com/x"),
            "very-long-subdomain-name…"
        );
    }

    #[test]
    fn format_annotation_note_places_comment_first() {
        let pick = serde_json::json!({
            "title": "Example Domain",
            "url": "https://example.com",
            "comment": "Fix this button color",
            "elements": [{
                "tag": "button",
                "selector": "#submit-btn",
                "name": "Submit"
            }]
        });
        let note = super::format_annotation_note(&pick);
        assert_eq!(
            note,
            "Fix this button color\n\n[Browser: Example Domain](https://example.com) — <button> `#submit-btn` \"Submit\""
        );
    }

    #[test]
    fn format_annotation_note_without_comment() {
        let pick = serde_json::json!({
            "title": "Example Domain",
            "url": "https://example.com",
            "comment": "",
            "elements": [{
                "tag": "a",
                "selector": "a.nav-link",
                "text": "Home",
                "href": "/home"
            }]
        });
        let note = super::format_annotation_note(&pick);
        assert_eq!(
            note,
            "[Browser: Example Domain](https://example.com) — <a> `a.nav-link` \"Home\" → /home"
        );
    }
}
