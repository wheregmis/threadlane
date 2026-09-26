use gpui::*;
use gpui_component::Root;
use threadlane_coding_agent::config_dump::dump_config;
use threadlane_ui_chat::init as init_chat;
use threadlane_ui_theme::{init as init_theme, Assets};
use threadlane_ui_workspace::{init as init_workspace, StartupView};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod diagnostics;
mod process_environment;

#[hotpath::main]
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    process_environment::initialize_child_process_path();
    if args.iter().any(|arg| arg == "--dump-config") {
        if let Err(error) = dump_config(&args) {
            eprintln!("--dump-config: {error}");
            std::process::exit(2);
        }
        return;
    }
    let diagnostics = diagnostics::ProcessLog::open()
        .map(Some)
        .unwrap_or_else(|error| {
            eprintln!("Threadlane could not open persistent diagnostics: {error}");
            None
        });
    let file_layer = diagnostics.as_ref().map(|log| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_writer(log.writer())
    });
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "info,gpui_component::text::format::markdown=error,gpui_base::text::format::markdown=error",
        )
    });
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true);
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(file_layer);
    if std::env::var_os("THREADLANE_TRACE_JSON").is_some() {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.init();
    }

    let app = gpui_platform::application().with_assets(Assets);

    let shutdown_log = diagnostics.clone();
    app.run(move |cx| {
        if let Some(log) = shutdown_log {
            cx.on_app_quit(move |_| {
                if let Err(error) = log.shutdown_requested() {
                    eprintln!("Threadlane could not record shutdown request: {error}");
                }
                async {}
            })
            .detach();
        }
        gpui_component::init(cx);
        init_chat(cx);
        init_workspace(cx);
        init_theme(cx);

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
            if let Err(error) = cx
                .open_window(options, |window, cx| {
                    #[cfg(feature = "gpui-profiler")]
                    if std::env::var_os("THREADLANE_GPUI_PROFILE").is_some() {
                        window.set_debug_frame_overlay_mode(DebugFrameOverlayMode::Full);
                    }
                    let view = StartupView::build(window, cx);
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .map(|_| ())
            {
                // A window failure must report, not panic the spawn task:
                // without a window there is nothing to render into.
                tracing::error!(?error, "Threadlane could not open its window");
            }
        })
        .detach();
    });
    if let Some(log) = diagnostics {
        if let Err(error) = log.finish() {
            eprintln!("Threadlane could not record normal shutdown: {error}");
        }
    }
}
