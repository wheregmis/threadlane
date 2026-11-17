use crate::trajectory_cache::{
    build_trajectory_rows, contains_case_insensitive, extend_trajectory_facets,
    extend_trajectory_previews, extend_trajectory_rows, format_trajectory_raw_json,
    reconcile_trajectory_entries, reconcile_trajectory_entries_by_epoch, TrajectoryCacheKey,
    TrajectoryInspectorTab, TrajectoryMode, TrajectoryRenderCache, TrajectoryRow,
};
use threadlane_protocol::daemon::{TrajectoryDiagnostics, TrajectoryEntry};

struct EmptySource;
impl crate::TrajectorySource for EmptySource {
    fn session_key(&self, _: &gpui::App) -> Option<(std::path::PathBuf, String)> {
        None
    }
    fn revision(&self, _: TrajectoryMode, _: &gpui::App) -> (u64, u64) {
        (0, 0)
    }
    fn entries<'a>(
        &'a self,
        _: TrajectoryMode,
        _: &'a gpui::App,
    ) -> std::borrow::Cow<'a, [TrajectoryEntry]> {
        std::borrow::Cow::Borrowed(&[])
    }
}

#[test]
fn selected_trajectory_entry_formats_as_raw_json() {
    let entry = TrajectoryEntry {
        seq: Some(1),
        run_id: None,
        turn: None,
        request: None,
        category: "Tool".into(),
        summary: "Read file".into(),
        detail: "src/main.rs".into(),
        lane: None,
        correlation_id: None,
        diagnostics: TrajectoryDiagnostics::default(),
    };

    let raw = format_trajectory_raw_json(&entry);

    assert!(raw.contains("\"category\": \"Tool\""));
    assert!(raw.contains("\"summary\": \"Read file\""));
}

fn trajectory_entry(category: &str, request: Option<u32>, turn: Option<u32>) -> TrajectoryEntry {
    TrajectoryEntry {
        seq: None,
        run_id: None,
        turn,
        request,
        category: category.into(),
        summary: category.into(),
        detail: String::new(),
        lane: None,
        correlation_id: None,
        diagnostics: TrajectoryDiagnostics::default(),
    }
}

#[test]
fn trajectory_cache_reuses_entries_for_append_only_updates() {
    let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
    let source = vec![
        cached[0].clone(),
        trajectory_entry("Tool", Some(1), Some(1)),
    ];

    assert_eq!(reconcile_trajectory_entries(cached, &source), source);
}

#[test]
fn trajectory_cache_replaces_entries_when_existing_data_changes() {
    let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
    let source = vec![trajectory_entry("Assistant", Some(1), Some(1))];

    assert_eq!(reconcile_trajectory_entries(cached, &source), source);
}

#[test]
fn trajectory_epoch_distinguishes_append_from_replacement() {
    let cached = vec![trajectory_entry("Input", Some(1), Some(1))];
    let appended_source = vec![
        cached[0].clone(),
        trajectory_entry("Tool", Some(1), Some(1)),
    ];
    let (entries, appended) =
        reconcile_trajectory_entries_by_epoch(cached.clone(), &appended_source, 7, 7);
    assert!(appended);
    assert_eq!(entries, appended_source);

    let replacement = vec![trajectory_entry("Assistant", Some(1), Some(1))];
    let (entries, appended) = reconcile_trajectory_entries_by_epoch(cached, &replacement, 7, 8);
    assert!(!appended);
    assert_eq!(entries, replacement);
}

#[test]
fn trajectory_incremental_facets_match_full_rebuild() {
    let mut input = trajectory_entry("Input", Some(1), Some(1));
    input.lane = Some("main".into());
    let mut tool = trajectory_entry("Tool", Some(1), Some(1));
    tool.lane = Some("main".into());
    let mut anomaly = trajectory_entry("Anomaly", Some(1), Some(2));
    anomaly.lane = Some("child".into());
    let appended_tool = trajectory_entry("Tool", Some(1), Some(2));
    let entries = vec![input, tool, anomaly, appended_tool];
    let key = TrajectoryCacheKey {
        revision: 4,
        epoch: 1,
        mode: TrajectoryMode::Execution,
        query: "tool".into(),
        category: None,
        lane: None,
    };

    let mut incremental = (Vec::new(), std::collections::BTreeMap::new(), Vec::new());
    extend_trajectory_facets(
        &mut incremental.0,
        &mut incremental.1,
        &mut incremental.2,
        &entries[..2],
        0,
        &key,
    );
    extend_trajectory_facets(
        &mut incremental.0,
        &mut incremental.1,
        &mut incremental.2,
        &entries,
        2,
        &key,
    );

    let mut rebuilt = (Vec::new(), std::collections::BTreeMap::new(), Vec::new());
    extend_trajectory_facets(
        &mut rebuilt.0,
        &mut rebuilt.1,
        &mut rebuilt.2,
        &entries,
        0,
        &key,
    );
    assert_eq!(incremental, rebuilt);
    assert_eq!(incremental.0, vec!["Anomaly", "Input", "Tool"]);
    assert_eq!(incremental.2, vec![1, 3]);
}

#[test]
fn trajectory_previews_extend_without_reformatting_existing_entries() {
    let mut input = trajectory_entry("Input", Some(1), Some(1));
    input.summary = "Prompt".into();
    input.detail = "first\nsecond".into();
    let mut tool = trajectory_entry("Tool", Some(1), Some(1));
    tool.summary = "read_file".into();
    tool.detail.clear();
    let entries = vec![input, tool];
    let mut previews = Vec::new();

    extend_trajectory_previews(&mut previews, &entries[..1], 0);
    extend_trajectory_previews(&mut previews, &entries, 1);

    assert_eq!(previews, ["Prompt  first second", "read_file"]);
}

#[test]
fn trajectory_rows_preserve_request_headers_and_setup_boundaries() {
    let entries = vec![
        trajectory_entry("Provider", Some(1), Some(1)),
        trajectory_entry("Input", Some(1), Some(1)),
        trajectory_entry("Input", Some(2), Some(2)),
    ];

    assert_eq!(
        build_trajectory_rows(&entries, &[0, 1, 2], TrajectoryMode::Requests),
        vec![
            TrajectoryRow::RequestHeader(1),
            TrajectoryRow::Setup,
            TrajectoryRow::Entry(0),
            TrajectoryRow::Entry(1),
            TrajectoryRow::RequestHeader(2),
            TrajectoryRow::Entry(2),
        ]
    );
}

#[test]
fn trajectory_incremental_rows_match_full_rebuild() {
    let entries = vec![
        trajectory_entry("Provider", Some(1), Some(1)),
        trajectory_entry("Input", Some(1), Some(1)),
        trajectory_entry("Input", Some(2), Some(2)),
    ];
    let indices = [0, 1, 2];
    let mut incremental = Vec::new();
    extend_trajectory_rows(
        &mut incremental,
        &entries,
        &indices[..2],
        0,
        TrajectoryMode::Requests,
    );
    extend_trajectory_rows(
        &mut incremental,
        &entries,
        &indices,
        2,
        TrajectoryMode::Requests,
    );

    assert_eq!(
        incremental,
        build_trajectory_rows(&entries, &indices, TrajectoryMode::Requests)
    );
}

#[test]
fn trajectory_cache_key_changes_with_data_or_filter() {
    let base = TrajectoryCacheKey {
        revision: 7,
        epoch: 2,
        mode: TrajectoryMode::Execution,
        query: "tool".into(),
        category: None,
        lane: None,
    };
    let mut changed = base.clone();
    changed.revision += 1;
    assert_ne!(base, changed);

    let mut changed = base.clone();
    changed.query = "provider".into();
    assert_ne!(base, changed);
}

#[test]
fn trajectory_search_matches_ascii_without_case_sensitivity() {
    assert!(contains_case_insensitive("Read File", "read"));
    assert!(contains_case_insensitive("TOOL-CALL-42", "call-42"));
    assert!(!contains_case_insensitive("Write File", "read"));
}

#[test]
fn trajectory_search_preserves_unicode_lowercase_matching() {
    assert!(contains_case_insensitive("CAFÉ output", "café"));
    assert!(contains_case_insensitive("Kelvin", "kelvin"));
    assert!(!contains_case_insensitive("CAFÉ output", "résumé"));
}

#[gpui::test]
fn trajectory_inspector_tabs_switch_content(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    cx.update(gpui_component::init);
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| crate::TrajectoryView::new(EmptySource, window, cx));
        *capture.borrow_mut() = Some(chat.clone());
        gpui_component::Root::new(chat, window, cx)
    });
    let chat = captured.borrow_mut().take().unwrap();
    chat.update(cx, |chat, cx| {
        chat.trajectory_cache = Some(TrajectoryRenderCache {
            key: TrajectoryCacheKey {
                revision: 0,
                epoch: 0,
                mode: TrajectoryMode::Execution,
                query: String::new(),
                category: None,
                lane: None,
            },
            all_entries: vec![trajectory_entry("Tool", Some(1), Some(1))],
            categories: Arc::new(vec!["Tool".to_string()]),
            lanes: Arc::new(Vec::new()),
            lane_latest: Arc::new(BTreeMap::new()),
            filtered_indices: vec![0],
            previews: vec!["Tool".into()],
            rows: vec![TrajectoryRow::Entry(0)],
            summary: crate::TrajectorySummary::default(),
        });
        chat.selected_trajectory_index = Some(0);
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "trajectory-inspector-Overview",
        "trajectory-inspector-Preview",
        "trajectory-inspector-Raw",
        "trajectory-inspector-Source",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} inspector tab is reachable"
        );
    }
    chat.read_with(cx, |chat, _| {
        assert_eq!(
            chat.trajectory_inspector_tab,
            TrajectoryInspectorTab::Overview
        );
    });
    let raw = cx.debug_bounds("trajectory-inspector-Raw").unwrap();
    cx.simulate_click(raw.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.trajectory_inspector_tab, TrajectoryInspectorTab::Raw);
    });
}

#[gpui::test]
fn trajectory_toolbar_filters_are_reachable(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    cx.update(gpui_component::init);
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let chat = cx.new(|cx| crate::TrajectoryView::new(EmptySource, window, cx));
        *capture.borrow_mut() = Some(chat.clone());
        gpui_component::Root::new(chat, window, cx)
    });
    let chat = captured.borrow_mut().take().unwrap();
    chat.update(cx, |chat, cx| {
        chat.trajectory_cache = Some(TrajectoryRenderCache {
            key: TrajectoryCacheKey {
                revision: 0,
                epoch: 0,
                mode: TrajectoryMode::Execution,
                query: String::new(),
                category: None,
                lane: None,
            },
            all_entries: vec![trajectory_entry("Tool", Some(1), Some(1))],
            categories: Arc::new(vec!["Tool".to_string()]),
            lanes: Arc::new(vec!["main".to_string(), "worker".to_string()]),
            lane_latest: Arc::new(BTreeMap::from([
                ("main".to_string(), "Read file".to_string()),
                ("worker".to_string(), "Write file".to_string()),
            ])),
            filtered_indices: vec![0],
            previews: vec!["Tool".into()],
            rows: vec![TrajectoryRow::Entry(0)],
            summary: crate::TrajectorySummary::default(),
        });
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "trajectory-mode-filter",
        "trajectory-category-filter",
        "trajectory-lane-filter",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} is reachable in the trajectory toolbar"
        );
    }
}

#[gpui::test]
fn trajectory_selection_tracks_event_across_replacement(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    use std::{borrow::Cow, cell::RefCell, rc::Rc};
    struct Source(Rc<RefCell<(u64, Vec<TrajectoryEntry>)>>);
    impl crate::TrajectorySource for Source {
        fn session_key(&self, _: &gpui::App) -> Option<(std::path::PathBuf, String)> {
            None
        }
        fn revision(&self, _: TrajectoryMode, _: &gpui::App) -> (u64, u64) {
            let rev = self.0.borrow().0;
            (rev, rev)
        }
        fn entries<'a>(
            &'a self,
            _: TrajectoryMode,
            _: &'a gpui::App,
        ) -> Cow<'a, [TrajectoryEntry]> {
            Cow::Owned(self.0.borrow().1.clone())
        }
    }
    cx.update(gpui_component::init);
    let mut first = trajectory_entry("Input", Some(1), Some(1));
    first.seq = Some(1);
    let mut selected = trajectory_entry("Tool", Some(1), Some(1));
    selected.seq = Some(2);
    let data = Rc::new(RefCell::new((0, vec![first.clone(), selected.clone()])));
    let source = Source(data.clone());
    let captured = Rc::new(RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| crate::TrajectoryView::new(source, window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.update(cx, |view, cx| {
        view.selected_trajectory_index = Some(1);
        cx.notify();
    });
    *data.borrow_mut() = (1, vec![selected, first.clone()]);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.read_with(cx, |view, _| {
        assert_eq!(view.selected_trajectory_index, Some(0))
    });
    *data.borrow_mut() = (2, vec![first]);
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.read_with(cx, |view, _| {
        assert_eq!(view.selected_trajectory_index, None)
    });
}

#[gpui::test]
fn trajectory_inspector_fits_narrow_and_wide_panels(cx: &mut gpui::TestAppContext) {
    use gpui::{AppContext as _, IntoElement as _, ParentElement as _, Styled as _};
    struct Source(Vec<TrajectoryEntry>);
    impl crate::TrajectorySource for Source {
        fn session_key(&self, _: &gpui::App) -> Option<(std::path::PathBuf, String)> {
            None
        }
        fn revision(&self, _: TrajectoryMode, _: &gpui::App) -> (u64, u64) {
            (0, 0)
        }
        fn entries<'a>(
            &'a self,
            _: TrajectoryMode,
            _: &'a gpui::App,
        ) -> std::borrow::Cow<'a, [TrajectoryEntry]> {
            std::borrow::Cow::Borrowed(&self.0)
        }
    }
    struct Host {
        width: f32,
        view: gpui::Entity<crate::TrajectoryView<Source>>,
    }
    impl gpui::Render for Host {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
                .w(gpui::px(self.width))
                .h_full()
                .flex()
                .flex_col()
                .child(self.view.clone())
                .into_any_element()
        }
    }
    cx.update(gpui_component::init);
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| {
            crate::TrajectoryView::new(
                Source(vec![trajectory_entry("Input", Some(1), Some(1))]),
                window,
                cx,
            )
        });
        let host = cx.new(|_| Host {
            width: 350.,
            view: view.clone(),
        });
        *capture.borrow_mut() = Some((host.clone(), view));
        gpui_component::Root::new(host, window, cx)
    });
    let (host, view) = captured.borrow_mut().take().unwrap();
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.update(cx, |view, cx| {
        view.selected_trajectory_index = Some(0);
        cx.notify();
    });
    for width in [350., 960., 350.] {
        host.update(cx, |host, cx| {
            host.width = width;
            cx.notify();
        });
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        let inspector = cx.debug_bounds("trajectory-inspector").unwrap();
        assert!(inspector.right() <= gpui::px(width));
        for tab in [
            "trajectory-inspector-Overview",
            "trajectory-inspector-Preview",
            "trajectory-inspector-Raw",
            "trajectory-inspector-Source",
        ] {
            let bounds = cx.debug_bounds(tab).unwrap();
            assert!(
                bounds.left() >= inspector.left() && bounds.right() <= inspector.right(),
                "{tab} fits at {width}"
            );
        }
        view.read_with(cx, |view, _| assert_eq!(view.split_inspector, width > 700.));
        let expected = if width > 700. { width * 0.55 } else { width };
        assert!((f32::from(inspector.size.width) - expected).abs() < 1.0);
    }
}

#[gpui::test]
fn trajectory_mode_switch_never_reuses_another_projection(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext as _;
    struct Source;
    impl crate::TrajectorySource for Source {
        fn session_key(&self, _: &gpui::App) -> Option<(std::path::PathBuf, String)> {
            None
        }
        fn revision(&self, _: TrajectoryMode, _: &gpui::App) -> (u64, u64) {
            (0, 0)
        }
        fn entries<'a>(
            &'a self,
            mode: TrajectoryMode,
            _: &'a gpui::App,
        ) -> std::borrow::Cow<'a, [TrajectoryEntry]> {
            std::borrow::Cow::Owned(vec![trajectory_entry(
                match mode {
                    TrajectoryMode::Execution | TrajectoryMode::Requests => "Operation",
                    TrajectoryMode::Recovery => "Decision",
                    TrajectoryMode::ModelContext => "Model Context",
                    TrajectoryMode::DurableEvents => "Record",
                },
                Some(1),
                Some(1),
            )])
        }
    }
    cx.update(gpui_component::init);
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| crate::TrajectoryView::new(Source, window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().unwrap();
    for (mode, category) in [
        (TrajectoryMode::Execution, "Operation"),
        (TrajectoryMode::Recovery, "Decision"),
        (TrajectoryMode::Execution, "Operation"),
        (TrajectoryMode::ModelContext, "Model Context"),
        (TrajectoryMode::DurableEvents, "Record"),
        (TrajectoryMode::Requests, "Operation"),
    ] {
        view.update(cx, |view, cx| {
            view.trajectory_mode = mode;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(cx, |view, _| {
            let entries = &view.trajectory_cache.as_ref().unwrap().all_entries;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].category, category);
        });
    }
}
