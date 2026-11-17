//! A `wry` webview hosted in a window-composition surface.
//!
//! `Window::enable_window_composition` (longbridge/gpui-fast#30) layers a
//! transparent GPUI overlay above native views: the native webview sits in a
//! composition surface's container, so deferred content (dialogs, sheets,
//! tooltips, menus, notifications) paints above the page instead of being
//! occluded by it. When composition setup fails the webview stays a direct
//! child of the window, keeping the old stacking behavior.
//!
//! Platform notes:
//! - macOS: the `WKWebView` is reparented into the surface's container view.
//! - Linux/X11: `lb-wry` builds the webview as a child of the surface's own
//!   child window (an X window wrapped in a `GdkWindow` foreign window), so
//!   the surface must exist before the webview is built. Wayland windows
//!   cannot host a GTK view — callers get a clean error. GTK has no main loop
//!   here, so a pump task drains `gtk::main_iteration_do` on a timer; without
//!   it the webview never draws or answers IPC.
//! - Windows: `lb-wry` builds the WebView2 controller as a child `HWND` of
//!   the GPUI window. GPUI's composition surfaces are `IDCompositionVisual`s,
//!   which an `HWND`-hosted webview cannot join, so the browser stays in the
//!   direct-child fallback: GPUI overlays clip against the page's bounds
//!   instead of painting above it.

use std::rc::Rc;

use gpui::{
    App, Bounds, Context, DispatchPhase, Element, ElementId, FocusHandle, Focusable,
    GlobalElementId, InteractiveElement, IntoElement, LayoutId, MouseDownEvent, ParentElement,
    Pixels, Render, Size, Style, Styled as _, Window, WindowCompositionSurface, div,
};
#[cfg(target_os = "macos")]
use gpui_component::ActiveTheme as _;
use wry::{
    Rect, dpi,
    dpi::{LogicalPosition, LogicalSize},
};

/// A `wry` webview whose native view sits in a GPUI composition surface.
///
/// Mirrors the `gpui_wry::WebView` API surface the browser view uses. On drop
/// the surface's container view leaves the window, taking the native view
/// with it.
pub struct ComposedWebView {
    focus_handle: FocusHandle,
    /// Declared before `surface`: wry releases the native webview on drop, so
    /// the native view is gone before the container it lived in.
    webview: Rc<wry::WebView>,
    /// The composition surface hosting the webview; `None` in the
    /// direct-child fallback.
    surface: Option<WindowCompositionSurface>,
    /// The GPUI window's own X11 window, wrapped for GDK so keyboard focus
    /// can be returned to it — `wry`'s `focus_parent` would focus the
    /// webview's container, which keeps keys inside the page.
    #[cfg(target_os = "linux")]
    gpui_gdk_window: Option<gtk::gdk::Window>,
}

impl Drop for ComposedWebView {
    fn drop(&mut self) {
        self.hide();
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use gpui::App;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Once;

    /// `HasWindowHandle` over a bare X11 window id: wry's X11 backend only
    /// accepts the `Xlib` variant of `RawWindowHandle`, while GPUI hands out
    /// `Xcb` — both name the same XID.
    struct XlibParent(u64);

    impl HasWindowHandle for XlibParent {
        fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
            // SAFETY: the XID names a live child window of a toplevel owned by
            // this process; wry only reparents/manages it, which is valid for
            // any X window in the same connection.
            Ok(unsafe {
                raw_window_handle::WindowHandle::borrow_raw(
                    RawWindowHandle::Xlib(raw_window_handle::XlibWindowHandle::new(self.0)),
                )
            })
        }
    }

    /// Extracts the X11 window id from a `RawWindowHandle`, or `None` on
    /// Wayland (and any non-X11 backend).
    pub fn raw_xid(handle: RawWindowHandle) -> Option<u64> {
        match handle {
            RawWindowHandle::Xcb(xcb) => Some(xcb.window.get() as u64),
            RawWindowHandle::Xlib(xlib) => Some(xlib.window),
            _ => None,
        }
    }

    /// The XID a new webview should be a child of: the composition surface's
    /// child window, or the GPUI window itself in the fallback.
    pub fn surface_xid(surface: &WindowCompositionSurface) -> Option<u64> {
        surface
            .platform_surface()
            .and_then(|platform| platform.platform_handle())
            .ok()
            .and_then(|handle| handle.downcast::<RawWindowHandle>().ok())
            .and_then(|handle| raw_xid(*handle))
    }

    /// The GPUI window's own X11 id — the fallback parent when no
    /// composition surface is available.
    pub fn window_xid(window: &Window) -> Option<u64> {
        // UFCS: `Window::window_handle()` is an inherent method returning
        // `AnyWindowHandle`; the raw XID lives behind the `HasWindowHandle`
        // trait impl instead.
        let handle = HasWindowHandle::window_handle(window).ok()?;
        raw_xid(handle.as_raw())
    }

    /// Builds the wry webview as a child of `surface`'s X window (preferred:
    /// GPUI overlays keep painting above the page), or as a direct child of
    /// the GPUI window when composition is unavailable.
    pub fn build_child(
        builder: wry::WebViewBuilder<'_>,
        surface: Option<&WindowCompositionSurface>,
        window: &Window,
    ) -> Result<wry::WebView, String> {
        let parent = surface
            .and_then(surface_xid)
            .or_else(|| window_xid(window))
            .ok_or_else(|| {
                "The embedded browser needs an X11 window; Wayland is not supported yet."
                    .to_string()
            })?;
        builder.build_as_child(&XlibParent(parent)).map_err(|error| {
            format!("Browser could not start. Retry Open link… or choose Open in default browser. ({error})")
        })
    }

    /// A `gdk::Window` wrapping an arbitrary XID on GTK's display connection.
    /// Used to hand the GPUI window back its keyboard focus and to give the
    /// webview's own window an X11 focus target on click-in.
    pub fn foreign_window(xid: u64) -> Option<gtk::gdk::Window> {
        use gtk::glib::object::ObjectType as _;
        use gtk::glib::translate::FromGlibPtrFull as _;

        let display = gtk::gdk::Display::default()?;
        let raw = display.as_ptr() as *mut gdkx11::ffi::GdkX11Display;
        let window = unsafe { gdkx11::ffi::gdk_x11_window_foreign_new_for_display(raw, xid) };
        if window.is_null() {
            return None;
        }
        unsafe { Some(gtk::gdk::Window::from_glib_full(window)) }
    }

    /// The GPUI window wrapped as a foreign GDK window so `focus()` returns
    /// keyboard input to GPUI.
    pub fn gpui_focus_window(window: &Window) -> Option<gtk::gdk::Window> {
        window_xid(window).and_then(foreign_window)
    }

    /// GTK initialisation, once per process. `set_allowed_backends` forces
    /// GTK to open the X11 display even inside a Wayland session, which is
    /// what lets wry wrap the XID of the XWayland-backed composition surface.
    pub fn ensure_gtk_init() -> Result<(), String> {
        static INIT: Once = Once::new();
        static READY: AtomicBool = AtomicBool::new(false);
        INIT.call_once(|| {
            // `set_var` here would race `getenv` callers on other threads; an
            // explicit GDK_BACKEND still overrides this, as before.
            if std::env::var_os("GDK_BACKEND").is_none() {
                gtk::gdk::set_allowed_backends("x11");
            }
            READY.store(gtk::init().is_ok(), Ordering::SeqCst);
        });
        if READY.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("GTK could not connect to an X11 display.".to_string())
        }
    }

    /// No GTK main loop runs in this process, so WebKit/GTK only advance when
    /// something drains `gtk::main_iteration_do`. A timer task on the main
    /// thread keeps page rendering, IPC replies, and signal delivery moving.
    pub fn ensure_gtk_pump(cx: &App) {
        static STARTED: AtomicBool = AtomicBool::new(false);
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(8))
                    .await;
                // Bound the drain: a permanently-ready WebKit source would
                // otherwise loop forever inside `update`, starving the UI.
                cx.update(|_| {
                    for _ in 0..16 {
                        if !gtk::main_iteration_do(false) {
                            break;
                        }
                    }
                });
            }
        })
        .detach();
    }
}

#[cfg(target_os = "linux")]
pub use linux::{build_child, ensure_gtk_init, ensure_gtk_pump, surface_xid};

impl ComposedWebView {
    /// Reparents `webview`'s native view into a fresh composition surface.
    /// On any failure the webview stays a direct child of the window.
    #[cfg(target_os = "macos")]
    pub fn new(webview: wry::WebView, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let surface = Self::attach_surface(&webview, window, cx);
        if surface.is_none() {
            // Direct-child fallback: start with an empty frame like gpui-wry.
            let _ = webview.set_bounds(Rect::default());
        }
        Self {
            focus_handle: cx.focus_handle(),
            webview: Rc::new(webview),
            surface,
        }
    }

    /// Wraps a WebView2 controller already built as a child `HWND` of the
    /// GPUI window. `wry`'s `focus_parent` `SetFocus`es that `HWND`, so
    /// keyboard input returns to GPUI without extra bookkeeping.
    #[cfg(target_os = "windows")]
    pub fn new(webview: wry::WebView, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            webview: Rc::new(webview),
            surface: None,
        }
    }

    /// Wraps a webview already built as a child of `surface`'s X window (or
    /// of the GPUI window when `surface` is `None`). Focus is returned to the
    /// GPUI window through `gpui_gdk_window`.
    #[cfg(target_os = "linux")]
    pub fn new(
        webview: wry::WebView,
        surface: Option<WindowCompositionSurface>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use gtk::prelude::WidgetExt as _;
        use wry::WebViewExtUnix as _;

        // X focus never lands on the reparented container by itself: without a
        // grab the page would paint and click but never receive keystrokes.
        let widget = webview.webview();
        widget.connect_button_press_event(|widget, _| {
            widget.grab_focus();
            if let Some(window) = widget.window() {
                window.focus(0);
            }
            gtk::glib::Propagation::Proceed
        });
        Self {
            focus_handle: cx.focus_handle(),
            webview: Rc::new(webview),
            surface,
            gpui_gdk_window: linux::gpui_focus_window(window),
        }
    }

    #[cfg(target_os = "macos")]
    fn attach_surface(
        webview: &wry::WebView,
        window: &Window,
        cx: &App,
    ) -> Option<WindowCompositionSurface> {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use wry::WebViewExtMacOS as _;

        let surface = match window
            .enable_window_composition()
            .and_then(|composition| composition.create_native_surface())
        {
            Ok(surface) => surface,
            Err(error) => {
                tracing::warn!(
                    "window composition unavailable; webview stays a direct child: {error:#}"
                );
                return None;
            }
        };
        let handle = match surface
            .platform_surface()
            .and_then(|platform| platform.platform_handle())
        {
            Ok(handle) => handle,
            Err(error) => {
                tracing::warn!("composition surface has no platform handle: {error:#}");
                return None;
            }
        };
        let container = match handle.downcast::<usize>() {
            Ok(container) => *container as *mut AnyObject,
            Err(_) => {
                tracing::warn!("composition surface did not provide an AppKit view");
                return None;
            }
        };
        if container.is_null() {
            tracing::warn!("composition surface returned a null AppKit view");
            return None;
        }

        unsafe {
            let wk: *mut AnyObject = &*webview.webview() as *const _ as *mut AnyObject;
            // wry keeps the window's own view for `focus_parent`; only the
            // WKWebView moves into the container.
            let _: () = msg_send![wk, removeFromSuperview];
            let _: () = msg_send![container, addSubview: wk];
            // NSViewWidthSizable | NSViewHeightSizable: the container's frame
            // is authoritative; the webview fills it on every resize.
            let _: () = msg_send![wk, setAutoresizingMask: 18usize];
            // Clip the page to the same radius the GPUI frame draws.
            let layer: *mut AnyObject = msg_send![container, layer];
            if !layer.is_null() {
                let radius: f64 = f32::from(cx.theme().radius) as f64;
                let _: () = msg_send![layer, setMasksToBounds: true];
                let _: () = msg_send![layer, setCornerRadius: radius];
            }
        }
        Some(surface)
    }

    /// Show the webview. Callers must `cx.notify()` the entity afterwards so
    /// the element re-paints and re-applies the surface bounds; retained
    /// views otherwise replay the frame painted while it was hidden.
    pub fn show(&mut self) {
        let _ = self.webview.set_visible(true);
        if let Some(surface) = &self.surface {
            let _ = surface
                .platform_surface()
                .and_then(|platform| platform.set_visible(true));
        }
    }

    /// Hide the webview, returning keyboard focus to the window first. See
    /// [`Self::show`] for the required `cx.notify()`.
    pub fn hide(&mut self) {
        self.focus_parent();
        let _ = self.webview.set_visible(false);
        if let Some(surface) = &self.surface {
            let _ = surface
                .platform_surface()
                .and_then(|platform| platform.set_visible(false));
        }
    }

    /// Return keyboard focus to the GPUI window. On macOS wry knows the
    /// parent `NSWindow`; on X11 we must `XSetInputFocus` the GPUI window
    /// explicitly (via a foreign `GdkWindow`), because the webview's own
    /// parent window is its embedded container.
    pub fn focus_parent(&self) {
        #[cfg(target_os = "linux")]
        {
            if let Some(window) = &self.gpui_gdk_window {
                window.focus(0);
                return;
            }
        }
        let _ = self.webview.focus_parent();
    }

    /// Go back in the webview history.
    pub fn back(&self) -> wry::Result<()> {
        self.webview.evaluate_script("history.back();")
    }

    /// Load a URL in the webview.
    pub fn load_url(&mut self, url: &str) {
        let _ = self.webview.load_url(url);
    }

    /// Get the raw wry webview.
    pub fn raw(&self) -> &wry::WebView {
        &self.webview
    }
}

impl Focusable for ComposedWebView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ComposedWebView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(ComposedWebViewElement {
                view: self.webview.clone(),
                surface: self.surface.clone(),
                #[cfg(target_os = "linux")]
                gpui_window: self.gpui_gdk_window.clone(),
            })
    }
}

/// Element that keeps the composition surface (or, in the fallback, the
/// webview itself) glued to its layout bounds.
pub struct ComposedWebViewElement {
    view: Rc<wry::WebView>,
    surface: Option<WindowCompositionSurface>,
    #[cfg(target_os = "linux")]
    gpui_window: Option<gtk::gdk::Window>,
}

impl IntoElement for ComposedWebViewElement {
    type Element = ComposedWebViewElement;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ComposedWebViewElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size::full(),
            flex_shrink: 1.,
            ..Default::default()
        };
        let id = window.request_layout(style, [], cx);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        if let Some(surface) = &self.surface {
            let device_bounds = bounds.to_device_pixels(window.scale_factor());
            if let Err(error) = surface
                .platform_surface()
                .and_then(|platform| platform.set_bounds(device_bounds))
            {
                tracing::warn!("failed to update webview surface bounds: {error:#}");
            }
            // The surface carries the window-space offset; inside the
            // container the webview fills it from the origin.
            let _ = self.view.set_bounds(Rect {
                size: dpi::Size::Logical(LogicalSize {
                    width: bounds.size.width.into(),
                    height: bounds.size.height.into(),
                }),
                position: dpi::Position::Logical(LogicalPosition::new(0., 0.)),
            });
        } else {
            let _ = self.view.set_bounds(Rect {
                size: dpi::Size::Logical(LogicalSize {
                    width: bounds.size.width.into(),
                    height: bounds.size.height.into(),
                }),
                position: dpi::Position::Logical(LogicalPosition::new(
                    bounds.origin.x.into(),
                    bounds.origin.y.into(),
                )),
            });
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        // A click GPUI dispatches never reached the native view — either it
        // landed outside the surface or on an overlay drawn above it. Either
        // way the page is not the click target, so hand keyboard focus back
        // to the window: overlay dismissal, the composer, and keybindings all
        // depend on the GPUI view being first responder again.
        let webview = self.view.clone();
        #[cfg(target_os = "linux")]
        let gpui_window = self.gpui_window.clone();
        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| {
            if phase == DispatchPhase::Bubble {
                #[cfg(target_os = "linux")]
                if let Some(window) = &gpui_window {
                    window.focus(0);
                    return;
                }
                let _ = webview.focus_parent();
            }
        });
    }
}
