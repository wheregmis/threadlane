use gpui::*;
use gpui_component::Root;
use std::path::PathBuf;
use threadlane_gpui::assets::Assets;
use threadlane_gpui::screens::workspace::WorkspaceView;
use threadlane_gpui::theme;
use threadlane_protocol::HarnessCompositionSnapshot;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn dump_config(args: &[String]) -> Result<(), String> {
    let project_index = args
        .iter()
        .position(|arg| arg == "--project")
        .ok_or_else(|| "--dump-config requires --project <path>".to_string())?;
    let project = args
        .get(project_index + 1)
        .ok_or_else(|| "--project requires a path".to_string())?;
    let _project = PathBuf::from(project)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let session_file = args
        .iter()
        .position(|arg| arg == "--session")
        .and_then(|index| args.get(index + 1))
        .map(|s| PathBuf::from(s).to_string_lossy().to_string());
    let model = args
        .iter()
        .position(|arg| arg == "--model")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| "gpt-4o".into());

    let snapshot = HarnessCompositionSnapshot {
        active_lane: "main".into(),
        session_file,
        model: model.clone(),
        provider: if model.starts_with("antigravity/") {
            "antigravity".into()
        } else if model.starts_with("opencode-go/") {
            "opencode".into()
        } else {
            "openai".into()
        },
        skills: Vec::new(),
        extensions: Vec::new(),
        sandbox_policy: "workspace-write".into(),
    };

    println!("active_lane={}", snapshot.active_lane);
    println!(
        "session_file={}",
        snapshot.session_file.unwrap_or_else(|| "<none>".into())
    );
    println!("model={}", snapshot.model);
    println!("provider={}", snapshot.provider);
    println!("skills={}", snapshot.skills.join(","));
    println!("extensions={}", snapshot.extensions.join(","));
    println!("sandbox={}", snapshot.sandbox_policy);
    Ok(())
}

#[hotpath::main]
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--dump-config") {
        if let Err(error) = dump_config(&args) {
            eprintln!("--dump-config: {error}");
            std::process::exit(2);
        }
        return;
    }
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
