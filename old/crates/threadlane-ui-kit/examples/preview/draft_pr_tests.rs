use super::{open, DraftPrPreview, DraftPrSample};
use gpui::{
    prelude::*, AppContext, Context, Entity, IntoElement, Render, TestAppContext,
    VisualTestContext, Window,
};
use gpui_component::WindowExt;
use std::{cell::RefCell, rc::Rc, time::Duration};
use threadlane_protocol::repo::GitStatus;
use threadlane_ui_kit::{ReviewDraftPrAction as Action, ReviewDraftPrPhase as Phase};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect(selector);
    cx.simulate_mouse_move(bounds.center(), None, Default::default());
    cx.simulate_click(bounds.center(), Default::default());
    draw(cx);
}
struct Harness(Entity<DraftPrPreview>);
impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::div().size_full().p_3().child(self.0.clone())
    }
}
fn mount(cx: &mut TestAppContext) -> (Entity<DraftPrPreview>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(threadlane_ui_theme::init_bundled);
    let saved = Rc::new(RefCell::new(None));
    let capture = saved.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let status = GitStatus {
            branch: Some("feature/a-very-long-branch-name-with-many-layout-segments".repeat(2)),
            ..Default::default()
        };
        let view = cx.new(|cx| DraftPrPreview::new(Some(&status), window, cx));
        *capture.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(cx.new(|_| Harness(view)), window, cx)
    });
    let view = saved.borrow_mut().take().unwrap();
    (view, cx)
}
#[gpui::test]
fn shared_draft_pr_preview_preserves_newer_edits_and_requires_readback(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx);
    cx.update(|window, cx| open(view.clone(), window, cx));
    draw(cx);
    let before = view.read_with(cx, |view, cx| view.fields(cx));
    click(cx, "submit-draft-pr");
    assert_eq!(view.read_with(cx, |view, _| view.phase), Phase::Creating);
    cx.simulate_keystrokes("escape enter");
    draw(cx);
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.title
                .update(cx, |input, cx| input.set_value("Newer edit", window, cx))
        })
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.phase), Phase::Created);
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields(cx).title),
        "Newer edit"
    );
    click(cx, "cancel-draft-pr");
    view.update(cx, |view, cx| view.set_sample(DraftPrSample::Uncertain, cx));
    cx.update(|window, cx| open(view.clone(), window, cx));
    draw(cx);
    assert!(cx.debug_bounds("submit-draft-pr").is_none());
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.phase), Phase::Uncertain);
    click(cx, "check-draft-pr");
    assert_eq!(view.read_with(cx, |view, _| view.phase), Phase::Checking);
    cx.executor().advance_clock(Duration::from_secs(1));
    draw(cx);
    assert_eq!(view.read_with(cx, |view, _| view.phase), Phase::Idle);
    assert!(cx.debug_bounds("submit-draft-pr").is_some());
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields(cx).body),
        before.body
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields(cx).base),
        before.base
    );
    click(cx, "regenerate-pr-title");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.title
                .update(cx, |input, cx| input.set_value("Keep my title", window, cx))
        })
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields(cx).title),
        "Keep my title"
    );
    assert!(view.read_with(cx, |view, _| view
        .error
        .as_deref()
        .unwrap()
        .contains("was not applied")));
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.update(|window, cx| open(view.clone(), window, cx));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields(cx).title),
        "Keep my title"
    );
    let body_before = view.read_with(cx, |view, cx| view.fields(cx).body);
    click(cx, "regenerate-pr-description");
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.executor().advance_clock(Duration::from_secs(1));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.fields(cx).body),
        body_before
    );
    assert!(!view.read_with(cx, |view, _| view.generating));
}
#[gpui::test]
fn shared_draft_pr_form_fits_zoom_and_blocks_unavailable_actions(cx: &mut TestAppContext) {
    let (view, cx) = mount(cx);
    for font in [13.0, 20.0] {
        cx.update(|window, _| window.set_rem_size(gpui::px(font)));
        for width in [280.0, 420.0] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(1200.0)));
            for phase in [
                Phase::Creating,
                Phase::Checking,
                Phase::Uncertain,
                Phase::Created,
            ] {
                view.update(cx, |view, cx| {
                    view.phase = phase;
                    cx.notify();
                });
                draw(cx);
                let form = cx.debug_bounds("review-draft-pr-form").unwrap();
                for selector in [
                    "review-draft-pr-context",
                    "draft-pr-base",
                    "draft-pr-title",
                    "draft-pr-body",
                    "regenerate-pr-title",
                    "regenerate-pr-description",
                    "cancel-draft-pr",
                ] {
                    let bounds = cx.debug_bounds(selector).unwrap();
                    assert!(
                        bounds.left() >= form.left() && bounds.right() <= form.right(),
                        "{selector} overflows at {font}, {width}: {bounds:?}, {form:?}"
                    );
                }
                let before = view.read_with(cx, |view, cx| view.fields(cx));
                for action in [
                    Action::Create,
                    Action::GenerateTitle,
                    Action::GenerateDescription,
                ] {
                    cx.update(|window, cx| {
                        view.update(cx, |view, cx| view.action(&action, window, cx))
                    });
                }
                assert_eq!(view.read_with(cx, |view, _| view.phase), phase);
                assert!(!view.read_with(cx, |view, _| view.generating));
                assert_eq!(view.read_with(cx, |view, cx| view.fields(cx)), before);
            }
            view.update(cx, |view, cx| {
                view.set_sample(DraftPrSample::ChangedCheckout, cx)
            });
            draw(cx);
            assert!(cx.debug_bounds("draft-pr-context-error").is_some());
            click(cx, "submit-draft-pr");
            click(cx, "regenerate-pr-title");
            assert_eq!(view.read_with(cx, |view, _| view.phase), Phase::Idle);
            assert!(!view.read_with(cx, |view, _| view.generating));
            view.update(cx, |view, cx| view.set_sample(DraftPrSample::Ready, cx));
        }
    }
}
