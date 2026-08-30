use gpui::*;
use gpui_component::Root;
use threadlane_gpui::assets::Assets;
use threadlane_gpui::screens::workspace::WorkspaceView;
use threadlane_gpui::theme;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[hotpath::main]
fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,gpui_component::text::format::markdown=error")
    });
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true);
    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);
    if std::env::var_os("THREADLANE_TRACE_JSON").is_some() {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.init();
    }

    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);
        threadlane_gpui::screens::chat::init(cx);
        threadlane_gpui::screens::workspace::init(cx);
        theme::init(cx);

        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1100.0), px(720.0)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Threadlane (GPUI)".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(12.0))),
            }),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                #[cfg(feature = "gpui-profiler")]
                if std::env::var_os("THREADLANE_GPUI_PROFILE").is_some() {
                    window.set_debug_frame_overlay_mode(DebugFrameOverlayMode::Full);
                }
                let view = WorkspaceView::build(window, cx);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open GPUI window");
        })
        .detach();
    });
}
