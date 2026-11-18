//! One process-wide notification bridge; routing metadata never leaves the process.
use super::{
    session_switcher::{resolve, VisitKey},
    WorkspaceView,
};
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Global, SystemNotification,
    SystemNotificationResponse, WeakEntity, Window,
};
use gpui_component::WindowExt;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use threadlane_client::DaemonClient;
use threadlane_protocol::daemon::SessionProjectionKey;
use threadlane_ui_state::{
    actions::AppAction, completion_alerts::CompletionAlertIdentity, controller, AppState,
};

const MAX_ROUTES: usize = 128;
const TAG_PREFIX: &str = "threadlane-alert-";
const UNAVAILABLE: &str = "This session is no longer available";

#[derive(Clone)]
struct Owner {
    view: WeakEntity<WorkspaceView>,
    window: AnyWindowHandle,
}

#[derive(Clone)]
pub(super) struct Activation {
    identity: Option<CompletionAlertIdentity>,
    client: Arc<dyn DaemonClient>,
    epoch: u64,
    generation: u64,
}
impl Activation {
    fn current(&self, state: &AppState) -> bool {
        state.completion_alerts.is_enabled()
            && self.generation == state.completion_alerts.generation()
            && self.identity.as_ref().is_none_or(|identity| {
                !state
                    .automations
                    .snapshot
                    .runs
                    .iter()
                    .any(|run| run.session_file.as_ref() == Some(&identity.session_file))
            })
            && (self.identity.is_none()
                || (Arc::ptr_eq(&self.client, &state.daemon_client)
                    && self.epoch == state.daemon_client.file_search_connection_epoch()
                    && state.daemon_client.is_connected()
                    && !state.daemon_remote))
    }

    fn key(&self) -> Option<VisitKey> {
        let identity = self.identity.as_ref()?;
        Some(VisitKey {
            work_dir: identity.project.clone(),
            session: SessionProjectionKey {
                session_id: identity.session_id.clone(),
                session_file: identity.session_file.clone(),
            },
        })
    }
}

struct Route {
    tag: String,
    owner: Owner,
    activation: Activation,
}
#[derive(Default)]
struct NotificationRouter {
    owners: Vec<Owner>,
    routes: VecDeque<Route>,
    next_tag: u64,
    #[cfg(test)]
    delivered: Vec<SystemNotification>,
}
impl Global for NotificationRouter {}

fn notification(tag: String, test: bool) -> SystemNotification {
    SystemNotification {
        tag: tag.into(),
        title: if test {
            "Threadlane test alert"
        } else {
            "Session completed"
        }
        .into(),
        body: if test {
            "This is a test of background completion alerts"
        } else {
            "Open Threadlane to review the result"
        }
        .into(),
        actions: Vec::new(),
    }
}

fn show(notification: SystemNotification, cx: &mut App) {
    #[cfg(not(test))]
    cx.show_system_notification(notification);
    #[cfg(test)]
    cx.global_mut::<NotificationRouter>()
        .delivered
        .push(notification);
}

fn register(owner: Owner, cx: &mut App) {
    if !cx.has_global::<NotificationRouter>() {
        cx.set_global(NotificationRouter::default());
        cx.on_system_notification_response(respond);
    }
    let router = cx.global_mut::<NotificationRouter>();
    router.owners.retain(|owner| owner.view.upgrade().is_some());
    router.owners.push(owner);
}

fn respond(response: SystemNotificationResponse, cx: &mut App) {
    if !response.tag.starts_with(TAG_PREFIX) || response.action_id.is_some() {
        return;
    }
    let windows = cx.windows();
    let router = cx.global_mut::<NotificationRouter>();
    router
        .owners
        .retain(|owner| windows.contains(&owner.window) && owner.view.upgrade().is_some());
    let route = router
        .routes
        .iter()
        .position(|route| route.tag == response.tag)
        .and_then(|index| router.routes.remove(index));
    let fallback = router
        .owners
        .iter()
        .find(|owner| owner.view.upgrade().is_some())
        .cloned();
    cx.dismiss_system_notification(&response.tag);
    if let Some(route) = route.filter(|route| {
        windows.contains(&route.owner.window) && route.owner.view.upgrade().is_some()
    }) {
        activate(route.owner, Some(route.activation), cx);
    } else if let Some(owner) = fallback {
        activate(owner, None, cx);
    }
}

fn activate(owner: Owner, activation: Option<Activation>, cx: &mut App) {
    cx.defer(move |cx| {
        let _ = owner.window.update(cx, |_, window, cx| {
            window.activate_window();
            let _ = owner.view.update(cx, |view, cx| {
                view.pending_completion_activation = activation;
                if view.pending_completion_activation.is_none() {
                    window.push_notification(UNAVAILABLE, cx);
                    return;
                }
                let file = view
                    .pending_completion_activation
                    .as_ref()
                    .and_then(|activation| activation.identity.as_ref())
                    .map(|identity| identity.session_file.clone());
                let probe = cx.background_spawn(async move {
                    file.is_none_or(|path| {
                        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
                    })
                });
                view.completion_activation_task =
                    Some(cx.spawn_in(window, async move |this, cx| {
                        if !probe.await {
                            let _ = this.update_in(cx, |view, window, cx| {
                                view.pending_completion_activation = None;
                                window.push_notification(UNAVAILABLE, cx);
                            });
                            return;
                        }
                        loop {
                            let done = this.update_in(cx, |view, window, cx| {
                                view.try_completion_activation(window, cx)
                            });
                            if !matches!(done, Ok(false)) {
                                break;
                            }
                            cx.background_executor()
                                .timer(Duration::from_millis(100))
                                .await;
                        }
                    }));
            });
        });
    });
}

impl WorkspaceView {
    pub(super) fn initialize_completion_alerts(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        register(
            Owner {
                view: cx.entity().downgrade(),
                window: window.window_handle(),
            },
            cx,
        );
        let model = self.model.downgrade();
        let load = cx.background_spawn(async {
            threadlane_ui_state::completion_alert_preferences::load(
                &threadlane_ui_state::completion_alert_preferences::path(),
            )
        });
        cx.spawn(async move |_, cx| {
            let result = load.await;
            let _ = model.update(cx, |state, cx| {
                state.completion_alerts_loaded = true;
                match result {
                    Ok(enabled) => state.set_completion_alerts_enabled(enabled),
                    Err(error) => state.completion_alerts_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn dispatch_completion_alerts(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (intents, test, client, epoch, generation, enabled) =
            self.model.update(cx, |state, _| {
                state.sync_completion_alert_context();
                (
                    state.completion_alerts.drain(),
                    std::mem::take(&mut state.completion_alert_test_requested),
                    state.daemon_client.clone(),
                    state.daemon_client.file_search_connection_epoch(),
                    state.completion_alerts.generation(),
                    state.completion_alerts.is_enabled(),
                )
            });
        if self
            .pending_completion_activation
            .as_ref()
            .is_some_and(|activation| !activation.current(self.model.read(cx)))
        {
            self.pending_completion_activation = None;
            self.completion_activation_task = None;
        }
        // Prune stale targets even when there is nothing to deliver (disable/reconnect/removal).
        let stale: Vec<_> = cx
            .global::<NotificationRouter>()
            .routes
            .iter()
            .filter(|route| {
                let Some(view) = route.owner.view.upgrade() else {
                    return true;
                };
                let model = if view.entity_id() == cx.entity_id() {
                    self.model.clone()
                } else {
                    view.read(cx).model.clone()
                };
                let state = model.read(cx);
                !route.activation.current(state)
                    || route
                        .activation
                        .key()
                        .is_some_and(|key| resolve(state, &key).is_none())
            })
            .map(|route| route.tag.clone())
            .collect();
        cx.global_mut::<NotificationRouter>()
            .routes
            .retain(|route| !stale.contains(&route.tag));
        for tag in stale {
            cx.dismiss_system_notification(&tag);
        }

        let windows = cx.windows();
        cx.global_mut::<NotificationRouter>()
            .owners
            .retain(|owner| windows.contains(&owner.window) && owner.view.upgrade().is_some());
        let owners = cx.global::<NotificationRouter>().owners.clone();
        let foreground = window.is_window_active()
            || owners.iter().any(|owner| {
                owner.window != window.window_handle()
                    && owner
                        .window
                        .update(cx, |_, window, _| window.is_window_active())
                        .unwrap_or(true)
            });
        let state = self.model.read(cx);
        let eligible = enabled && !state.daemon_remote && client.is_connected();
        let identities: Vec<_> = intents
            .into_iter()
            .filter(|intent| {
                intent.generation == generation && {
                    let activation = Activation {
                        identity: Some(intent.identity.clone()),
                        client: client.clone(),
                        epoch,
                        generation,
                    };
                    activation.current(state)
                        && activation
                            .key()
                            .is_some_and(|key| resolve(state, &key).is_some())
                }
            })
            .map(|intent| intent.identity)
            .collect();
        let mut alerts = Vec::new();
        if eligible && !foreground {
            alerts.extend(identities.into_iter().map(Some));
        }
        if enabled && test {
            alerts.push(None);
        }
        for identity in alerts {
            let activation = Activation {
                identity,
                client: client.clone(),
                epoch,
                generation,
            };
            let owner = Owner {
                view: cx.entity().downgrade(),
                window: window.window_handle(),
            };
            let router = cx.global_mut::<NotificationRouter>();
            let mut dismissed = Vec::new();
            router.routes.retain(|route| {
                let same = route.owner.view.entity_id() == owner.view.entity_id()
                    && route.activation.identity == activation.identity;
                if same {
                    dismissed.push(route.tag.clone());
                }
                !same
            });
            if router.routes.len() >= MAX_ROUTES {
                if let Some(route) = router.routes.pop_front() {
                    dismissed.push(route.tag);
                }
            }
            router.next_tag = router.next_tag.wrapping_add(1);
            let tag = format!(
                "{TAG_PREFIX}{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                router.next_tag
            );
            let test = activation.identity.is_none();
            router.routes.push_back(Route {
                tag: tag.clone(),
                owner,
                activation,
            });
            for old in dismissed {
                cx.dismiss_system_notification(&old);
            }
            show(notification(tag, test), cx);
        }
    }

    fn try_completion_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pending_completion_activation.is_none() {
            return true;
        }
        if window.has_active_dialog(cx) || window.has_active_sheet(cx) || window.has_active_prompt()
        {
            return false;
        }
        let activation = self.pending_completion_activation.take().unwrap();
        self.model
            .update(cx, |state, _| state.sync_completion_alert_context());
        let state = self.model.read(cx);
        if !activation.current(state) {
            window.push_notification(UNAVAILABLE, cx);
            return true;
        }
        let Some(key) = activation.key() else {
            self.open_settings_action(&super::OpenSettings, window, cx);
            self.settings.update(cx, |settings, cx| {
                settings.open_search_destination("general", cx)
            });
            return true;
        };
        let Some(_) = resolve(state, &key) else {
            window.push_notification(UNAVAILABLE, cx);
            return true;
        };
        self.model.update(cx, |state, cx| {
            controller::dispatch(
                state,
                AppAction::SelectSession {
                    work_dir: key.work_dir,
                    session_id: key.session.session_id,
                },
            );
            state.workspace_page = threadlane_ui_state::WorkspacePage::Chat;
            cx.notify();
        });
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(threadlane_ui_chat::CentralTab::Chat, cx)
        });
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::{session_switcher, WorkspaceView};
    use super::{notification, register, respond, NotificationRouter, Owner, MAX_ROUTES};
    use gpui::{
        AppContext, Context, Entity, IntoElement, Render, SystemNotificationResponse,
        TestAppContext, VisualTestContext, Window,
    };
    use gpui_component::{Root, WindowExt};
    use std::{collections::HashSet, path::Path, time::Duration};
    use threadlane_protocol::daemon::{RunCompletionToken, SessionCompletionSummary as Summary};
    use threadlane_ui_state::{completion_alerts::CompletionAlertIdentity, AppState};

    struct EmptyHost;
    impl Render for EmptyHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            gpui::div()
        }
    }
    fn mount(
        cx: &mut TestAppContext,
        mut state: AppState,
    ) -> (Entity<WorkspaceView>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_theme::init_bundled);
        state.set_completion_alerts_enabled(true);
        for project in &state.client.projects {
            state
                .completion_alerts
                .observe(&project.work_dir, &project.sessions, &HashSet::new());
        }
        let (_, cx) = cx.add_window_view(|window, cx| Root::new(cx.new(|_| EmptyHost), window, cx));
        let view = cx
            .update(|window, cx| session_switcher::interaction_tests::workspace(state, window, cx));
        cx.update(|window, cx| {
            register(
                Owner {
                    view: view.downgrade(),
                    window: window.window_handle(),
                },
                cx,
            )
        });
        (view, cx)
    }
    fn state() -> AppState {
        let mut state = session_switcher::tests::state();
        for session in &mut state.projects[0].sessions {
            session.completion_summary = Summary::None;
        }
        state.active_session_id = Some("a".into());
        state
    }
    fn complete(view: &Entity<WorkspaceView>, id: &str, seq: u64, cx: &mut VisualTestContext) {
        let model = view.read_with(cx, |view, _| view.model.clone());
        model.update(cx, |state, _| {
            let project = &mut state.client.projects[0];
            let session = project
                .sessions
                .iter_mut()
                .find(|session| session.id == id)
                .unwrap();
            session.completion_summary = Summary::Latest(RunCompletionToken {
                seq,
                record_id: format!("finish-{seq}"),
                run_id: format!("run-{seq}"),
            });
            state
                .completion_alerts
                .observe(&project.work_dir, &project.sessions, &HashSet::new());
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.dispatch_completion_alerts(window, cx))
        });
    }
    #[gpui::test]
    fn completion_alert_delivery_foreground_discard_background_once_and_test_bypass(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = mount(cx, state());
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        complete(&view, "b", 1, cx);
        cx.update(|_, cx| assert!(cx.global::<NotificationRouter>().delivered.is_empty()));
        cx.deactivate_window();
        complete(&view, "b", 1, cx);
        cx.update(|_, cx| assert!(cx.global::<NotificationRouter>().delivered.is_empty()));
        complete(&view, "b", 2, cx);
        cx.update(|_, cx| {
            let alerts = &cx.global::<NotificationRouter>().delivered;
            assert_eq!(alerts.len(), 1);
            assert_eq!(alerts[0].title.as_ref(), "Session completed");
            assert_eq!(
                alerts[0].body.as_ref(),
                "Open Threadlane to review the result"
            );
            assert!(!alerts[0].tag.contains("/project"));
        });
        cx.update(|window, cx| {
            window.activate_window();
            view.update(cx, |view, cx| {
                view.model
                    .update(cx, |state, _| state.completion_alert_test_requested = true);
                view.dispatch_completion_alerts(window, cx);
            });
            assert_eq!(
                cx.global::<NotificationRouter>()
                    .delivered
                    .last()
                    .unwrap()
                    .title
                    .as_ref(),
                "Threadlane test alert"
            );
        });
        let model = view.read_with(cx, |view, _| view.model.clone());
        model.update(cx, |state, _| state.set_completion_alerts_enabled(false));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.dispatch_completion_alerts(window, cx));
            assert!(cx.global::<NotificationRouter>().routes.is_empty());
        });
    }
    #[gpui::test]
    fn completion_alert_click_defers_for_dialog_and_uses_exact_selection_without_ack(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut state = state();
        let project = root.path().to_path_buf();
        state.projects[0].work_dir = project.clone();
        state.active_work_dir = Some(project.clone());
        state.workspace_page = threadlane_ui_state::WorkspacePage::Settings;
        state.client.composer_drafts.insert(
            (Some(project.clone()), Some("c".into())),
            threadlane_client::ComposerDraft {
                text: "preserved draft".into(),
                images: vec![],
            },
        );
        for session in &mut state.projects[0].sessions {
            session.work_dir = project.clone();
            session.runtime_work_dir = project.clone();
            session.session_file = project.join(format!("{}.jsonl", session.id));
            std::fs::write(&session.session_file, "").unwrap();
        }
        state.register_session_seen(&project, "b");
        let (view, cx) = mount(cx, state);
        cx.deactivate_window();
        complete(&view, "b", 1, cx);
        let model = view.read_with(cx, |view, _| view.model.clone());
        let tag = cx.update(|_, cx| cx.global::<NotificationRouter>().delivered[0].tag.clone());
        cx.update(|window, cx| {
            window.open_alert_dialog(cx, |dialog, _, _| {
                dialog.title("Keep this decision").confirm()
            });
            respond(
                SystemNotificationResponse {
                    tag,
                    action_id: None,
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(
            model
                .read_with(cx, |state, _| state.active_session_id.clone())
                .as_deref(),
            Some("a")
        );
        cx.update(|window, cx| {
            assert!(window.has_active_dialog(cx));
            window.close_dialog(cx);
        });
        cx.background_executor
            .advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert_eq!(
            model
                .read_with(cx, |state, _| state.active_session_id.clone())
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            model.read_with(cx, |state, _| state.workspace_page),
            threadlane_ui_state::WorkspacePage::Chat
        );
        assert_eq!(
            model.read_with(cx, |state, _| state.pending_hydrations.len()),
            1
        );
        assert_eq!(
            model.read_with(cx, |state, _| state
                .client
                .composer_drafts
                .get(&(Some(project.clone()), Some("c".into())))
                .unwrap()
                .text
                .clone()),
            "preserved draft"
        );
        assert!(model.read_with(cx, |state, _| state
            .session_has_unseen_result(&state.projects[0].sessions[1])));
    }
    #[gpui::test]
    fn completion_alert_routes_are_bounded_and_stale_targets_do_not_select(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = mount(cx, state());
        cx.deactivate_window();
        let model = view.read_with(cx, |view, _| view.model.clone());
        for index in 0..MAX_ROUTES + 3 {
            let id = format!("opaque-{index}");
            model.update(cx, |state, _| {
                let mut session = state.projects[0].sessions[0].clone();
                session.id = id.clone();
                session.session_file = Path::new("/project").join(format!("{id}.jsonl"));
                state
                    .completion_alerts
                    .register(CompletionAlertIdentity::new(
                        Path::new("/project"),
                        &session,
                    ));
                state.projects[0].sessions.push(session);
            });
            complete(&view, &id, 1, cx);
        }
        cx.update(|_, cx| assert_eq!(cx.global::<NotificationRouter>().routes.len(), MAX_ROUTES));
        let mut target = cx.update(|_, cx| {
            cx.global::<NotificationRouter>()
                .routes
                .back()
                .unwrap()
                .activation
                .clone()
        });
        model.update(cx, |state, _| {
            let id = target.identity.as_ref().unwrap().session_id.clone();
            let session = state.projects[0]
                .sessions
                .iter_mut()
                .find(|session| session.id == id)
                .unwrap();
            session.session_file = "/different/file.jsonl".into();
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.pending_completion_activation = Some(target.clone());
                assert!(view.try_completion_activation(window, cx));
            })
        });
        assert_eq!(
            model
                .read_with(cx, |state, _| state.active_session_id.clone())
                .as_deref(),
            Some("a")
        );
        model.update(cx, |state, _| state.set_completion_alerts_enabled(false));
        target.identity = None;
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.pending_completion_activation = Some(target);
                assert!(view.try_completion_activation(window, cx));
                view.dispatch_completion_alerts(window, cx);
            })
        });
        cx.update(|_, cx| assert!(cx.global::<NotificationRouter>().routes.is_empty()));
    }
    #[test]
    fn completion_alert_payload_is_generic_and_test_is_explicit() {
        let alert = notification("threadlane-alert-opaque".into(), false);
        assert_eq!(alert.actions.len(), 0);
        assert_eq!(alert.title.as_ref(), "Session completed");
        assert!(notification("threadlane-alert-test".into(), true)
            .title
            .contains("test alert"));
    }
}
