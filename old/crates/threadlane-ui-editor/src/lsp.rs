use std::{
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use gpui::*;
use gpui_component::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, EditorState, HoverProvider,
    InputEvent, Rope,
};
use lsp_types::{
    CodeAction, CodeActionOrCommand, CompletionContext, CompletionResponse, GotoDefinitionResponse,
    Hover, LocationLink, ShowDocumentParams,
};
use threadlane_client::DaemonClient;
use threadlane_protocol::{
    daemon::{CommandResponse, SessionCommand},
    editor_lsp::{
        EditorLspOperation, EditorLspPosition, EditorLspRange, EditorLspRequest, EditorLspResponse,
        MAX_EDITOR_LSP_BYTES,
    },
};
use threadlane_ui_kit::{EditorLanguageRefresh, EditorWorkbench};
use threadlane_ui_state::{AppState, chat, project_io};

use super::EditorView;
use crate::lsp_mapping::{
    action_text, completions, map_diagnostics, relative_uri, scalar_range, wire_position,
};

const SETTLED_DIAGNOSTICS_BACKOFF: Duration = Duration::from_secs(30);

static NEXT_DOCUMENT: LazyLock<AtomicU64> = LazyLock::new(|| {
    AtomicU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    )
});

#[derive(Clone)]
struct Snapshot {
    client: Arc<dyn DaemonClient>,
    epoch: u64,
    session: String,
    root: PathBuf,
    path: String,
    revision: u64,
    load_generation: u64,
    activation: u64,
    text: SharedString,
    version: u64,
}

impl Snapshot {
    fn same_scope(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.client, &other.client)
            && self.epoch == other.epoch
            && self.session == other.session
            && self.root == other.root
            && self.path == other.path
    }

    fn same_buffer(&self, other: &Self) -> bool {
        self.same_scope(other)
            && self.revision == other.revision
            && self.load_generation == other.load_generation
            && self.activation == other.activation
            && self.text == other.text
    }
}

pub(super) struct LanguageService {
    owner: WeakEntity<EditorView>,
    editor: WeakEntity<EditorState>,
    workbench: WeakEntity<EditorWorkbench>,
    document_id: u64,
    version: u64,
    runtime: Option<u64>,
    snapshot: Option<Snapshot>,
    generations: [u64; 5],
    blocked: Option<String>,
    last_edit: Instant,
    settled_diagnostics: Option<(Snapshot, Instant)>,
    definition: Option<Snapshot>,
    actions: Vec<(CodeAction, Snapshot, Option<i32>)>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

pub(super) fn attach(
    owner: WeakEntity<EditorView>,
    model: Entity<AppState>,
    editor: &Entity<EditorState>,
    workbench: &Entity<EditorWorkbench>,
    path: &str,
    cx: &mut App,
) -> Option<Entity<LanguageService>> {
    if !matches!(
        path.rsplit('.').next(),
        Some("rs" | "js" | "jsx" | "ts" | "tsx" | "go" | "py" | "pyi")
    ) {
        return None;
    }
    let service = cx.new(|cx| {
        let changed = cx.subscribe(
            editor,
            |this: &mut LanguageService, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.last_edit = Instant::now();
                    this.clear_presentation(cx);
                }
            },
        );
        let scope = cx.observe(&model, |this: &mut LanguageService, model, cx| {
            let model = model.read(cx);
            if this.snapshot.as_ref().is_some_and(|old| {
                !Arc::ptr_eq(&old.client, &model.daemon_client)
                    || old.epoch != model.daemon_client.file_search_connection_epoch()
                    || !model.daemon_client.is_connected()
                    || model.active_session_id.as_ref() != Some(&old.session)
                    || model.active_git_work_dir().as_ref() != Some(&old.root)
            }) {
                this.reset(cx);
                this.cancel_pending_presentation(cx);
            }
        });
        let refresh = cx.subscribe(
            workbench,
            |this: &mut LanguageService, _, _: &EditorLanguageRefresh, cx| {
                this.reset(cx);
                this.cancel_pending_presentation(cx);
            },
        );
        cx.on_release(|this: &mut LanguageService, _| {
            let Some(snapshot) = this.snapshot.take() else {
                return;
            };
            let request = this.request(
                &snapshot,
                EditorLspOperation::Close,
                EditorLspPosition::default(),
            );
            if let Ok(runtime) = chat::executor() {
                runtime.spawn(async move {
                    let _ = snapshot
                        .client
                        .request(SessionCommand::EditorLsp { request })
                        .await;
                });
            }
        })
        .detach();
        let poll = cx.spawn(async move |this: WeakEntity<LanguageService>, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let next = this.update(cx, |this, cx| {
                    let snapshot = this.poll_snapshot(cx)?;
                    Some(this.query(
                        snapshot.text.to_string(),
                        0,
                        EditorLspOperation::Diagnostics,
                        cx,
                    ))
                });
                match next {
                    Ok(Some(task)) => {
                        let _ = task.await;
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
        LanguageService {
            owner,
            editor: editor.downgrade(),
            workbench: workbench.downgrade(),
            document_id: NEXT_DOCUMENT.fetch_add(1, Ordering::Relaxed),
            version: 0,
            runtime: None,
            snapshot: None,
            generations: [0; 5],
            blocked: None,
            last_edit: Instant::now(),
            settled_diagnostics: None,
            definition: None,
            actions: vec![],
            _subscriptions: vec![changed, scope, refresh],
            _poll: poll,
        }
    });
    let provider = Rc::new(Provider(service.downgrade()));
    let weak = service.downgrade();
    editor.update(cx, |editor, cx| {
        let lsp = editor.lsp_mut();
        lsp.hover_provider = Some(provider.clone());
        lsp.definition_provider = Some(provider.clone());
        lsp.code_action_providers = vec![provider.clone()];
        lsp.show_document = Some(Rc::new(move |params, window, cx| {
            let params = params.clone();
            // Never fall through: the library otherwise jumps in the source buffer.
            let _ = weak.update(cx, |service, cx| service.show_document(params, window, cx));
            true
        }));
        cx.notify();
    });
    workbench.update(cx, |workbench, cx| {
        workbench.set_language_completion(provider, cx);
        workbench.set_language_status(
            "LSP: waiting",
            "Language services require an active session in this checkout. Click to retry.",
            cx,
        );
    });
    Some(service)
}

impl LanguageService {
    fn poll_snapshot(&mut self, cx: &mut Context<Self>) -> Option<Snapshot> {
        if self.blocked.is_some() || self.last_edit.elapsed() < Duration::from_millis(450) {
            return None;
        }
        match self.capture(cx) {
            Ok(snapshot) => {
                let diagnostics_are_settled = self.settled_diagnostics.as_ref().is_some_and(
                    |(settled, completed_at)| {
                        settled.same_buffer(&snapshot)
                            && settled.version == snapshot.version
                            && completed_at.elapsed() < SETTLED_DIAGNOSTICS_BACKOFF
                    },
                );
                (!diagnostics_are_settled).then_some(snapshot)
            }
            Err(error) => {
                self.status(
                    "LSP: unavailable",
                    format!("{error}. Current-file words remain available."),
                    cx,
                );
                None
            }
        }
    }

    fn capture(&self, cx: &App) -> Result<Snapshot> {
        let owner = self
            .owner
            .upgrade()
            .ok_or_else(|| anyhow!("Editor closed"))?;
        let owner = owner.read(cx);
        let tab = owner
            .active_tab_index
            .and_then(|index| owner.tabs.get(index))
            .ok_or_else(|| anyhow!("No active buffer"))?;
        ensure!(
            tab.editor_state.as_ref().map(Entity::entity_id) == Some(self.editor.entity_id()),
            "Inactive buffer"
        );
        ensure!(
            !tab.loading
                && tab.pending_content.is_none()
                && tab.baseline_loaded
                && !tab.client_invalidated
                && tab.open_error.is_none(),
            "File is not ready for language services"
        );
        let model = owner.model.read(cx);
        let client = model.daemon_client.clone();
        ensure!(
            client.is_connected(),
            "Language services: daemon disconnected"
        );
        ensure!(
            client.supports_editor_lsp(),
            "Language services require daemon protocol v8"
        );
        ensure!(
            model.active_git_work_dir().as_ref() == Some(&tab.project_dir),
            "Activate a session in this checkout for language services"
        );
        ensure!(
            tab.client_origin
                .as_ref()
                .is_some_and(|origin| Arc::ptr_eq(origin, &client)),
            "This buffer belongs to another daemon"
        );
        let session = model
            .active_session_id
            .clone()
            .ok_or_else(|| anyhow!("Activate a session in this checkout for language services"))?;
        let editor = self
            .editor
            .upgrade()
            .ok_or_else(|| anyhow!("Editor closed"))?;
        Ok(Snapshot {
            epoch: client.file_search_connection_epoch(),
            client,
            session,
            root: tab.project_dir.clone(),
            path: tab.relative_path.clone(),
            revision: tab.buffer_revision,
            load_generation: tab.request_generation,
            activation: owner.language_activation,
            text: editor.read(cx).value(),
            version: self.version,
        })
    }

    fn current(&self, snapshot: &Snapshot, cx: &App) -> bool {
        self.capture(cx)
            .is_ok_and(|current| current.same_buffer(snapshot))
            && self.version == snapshot.version
    }

    fn status(&self, label: &str, detail: impl Into<SharedString>, cx: &mut App) {
        let _ = self.workbench.update(cx, |workbench, cx| {
            workbench.set_language_status(label.to_owned(), detail, cx)
        });
    }

    fn clear_presentation(&mut self, cx: &mut App) {
        self.definition = None;
        self.actions.clear();
        let _ = self.editor.update(cx, |editor, cx| {
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
            }
            editor.dismiss_completion_overlay(cx);
            editor.dismiss_code_action_overlay(cx);
            cx.notify();
        });
    }

    fn cancel_pending_presentation(&mut self, cx: &mut App) {
        let _ = self
            .editor
            .update(cx, |editor, cx| editor.dismiss_lsp_overlays(cx));
    }

    pub(super) fn deactivate(&mut self, cx: &mut App) {
        for generation in &mut self.generations {
            *generation = generation.wrapping_add(1);
        }
        self.settled_diagnostics = None;
        self.cancel_pending_presentation(cx);
        self.clear_presentation(cx);
    }

    fn reset(&mut self, cx: &mut App) {
        if let Some(snapshot) = self.snapshot.take() {
            let request = self.request(
                &snapshot,
                EditorLspOperation::Close,
                EditorLspPosition::default(),
            );
            if let Ok(runtime) = chat::executor() {
                runtime.spawn(async move {
                    let _ = snapshot
                        .client
                        .request(SessionCommand::EditorLsp { request })
                        .await;
                });
            }
        }
        // A delayed close from the old scope must not close a reopened document.
        self.document_id = NEXT_DOCUMENT.fetch_add(1, Ordering::Relaxed);
        self.blocked = None;
        self.runtime = None;
        self.snapshot = None;
        self.version = self.version.wrapping_add(1);
        self.settled_diagnostics = None;
        self.clear_presentation(cx);
        self.status(
            "LSP: waiting",
            "Language services will refresh for the active session and buffer.",
            cx,
        );
    }

    fn request(
        &self,
        snapshot: &Snapshot,
        operation: EditorLspOperation,
        position: EditorLspPosition,
    ) -> EditorLspRequest {
        EditorLspRequest {
            session_id: snapshot.session.clone(),
            work_dir: snapshot.root.clone(),
            path: snapshot.path.clone(),
            document_id: self.document_id,
            version: snapshot.version,
            expected_runtime_id: self.runtime,
            text: if matches!(operation, EditorLspOperation::Close) {
                String::new()
            } else {
                snapshot.text.to_string()
            },
            position,
            operation,
        }
    }

    fn query(
        &mut self,
        text: String,
        offset: usize,
        operation: EditorLspOperation,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<(Snapshot, EditorLspResponse)>>> {
        let slot = match operation {
            EditorLspOperation::Completion { .. } => 0,
            EditorLspOperation::Hover => 1,
            EditorLspOperation::Definition => 2,
            EditorLspOperation::Diagnostics => 3,
            _ => 4,
        };
        self.generations[slot] = self.generations[slot].wrapping_add(1);
        let generation = self.generations[slot];
        cx.spawn(async move |this, cx| {
            if slot == 0 { cx.background_executor().timer(Duration::from_millis(100)).await; }
            let prepared = this.update(cx, |this, cx| -> Result<Option<_>> {
                if this.generations[slot] != generation { return Ok(None); }
                let mut snapshot = match this.capture(cx) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        this.status("LSP: unavailable", format!("{error}. Current-file words remain available."), cx);
                        return Err(error);
                    }
                };
                if snapshot.text.as_str() != text { return Ok(None); }
                ensure!(text.len() <= MAX_EDITOR_LSP_BYTES, "LSP buffer exceeds the 1 MiB limit");
                if this.snapshot.as_ref().is_some_and(|old| !old.same_scope(&snapshot)) { this.reset(cx); }
                if this.snapshot.as_ref().is_none_or(|old| !old.same_buffer(&snapshot)) {
                    this.version = this.version.wrapping_add(1);
                    this.clear_presentation(cx);
                }
                snapshot.version = this.version;
                this.snapshot = Some(snapshot.clone());
                if let Some(error) = &this.blocked { return Err(anyhow!(error.clone())); }
                let position = wire_position(&text, offset)?;
                let request = this.request(&snapshot, operation, EditorLspPosition { line: position.line, character: position.character });
                if slot != 3 || this.runtime.is_none() {
                    this.status("LSP: working", "Waiting for the daemon language server. Permission requests are handled by the active session.", cx);
                }
                Ok(Some((snapshot, request)))
            })??;
            let Some((snapshot, request)) = prepared else { return Ok(None) };
            let client = snapshot.client.clone();
            let response = chat::executor().map_err(|error| anyhow!(error))?.spawn(async move {
                match client.request(SessionCommand::EditorLsp { request }).await? {
                    CommandResponse::EditorLsp { result } => result,
                    _ => Err("Daemon returned an unexpected language-service reply".into()),
                }
            }).await.map_err(|error| anyhow!(error))?;
            let diagnostics = if let Some(items) = response.as_ref().ok().and_then(|response| response.diagnostics.clone()) {
                let source = snapshot.text.clone();
                Some(cx.background_executor().spawn(async move {
                    map_diagnostics(source.as_str(), items)
                }).await)
            } else { None };
            this.update(cx, |this, cx| -> Result<Option<_>> {
                if this.generations[slot] != generation || !this.current(&snapshot, cx) { return Ok(None); }
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        let runtime_changed = error.contains("stale session runtime") || error.contains("Session runtime changed");
                        if runtime_changed { this.runtime = None; }
                        let transient = runtime_changed || error.to_ascii_lowercase().contains("busy")
                            || error.contains("activate a session");
                        if !transient {
                            this.blocked = Some(error.clone());
                            this.clear_presentation(cx);
                        }
                        this.status(if transient { "LSP: busy" } else { "LSP: unavailable" }, format!("{}. Current-file words remain available. Click to retry.", error.chars().take(512).collect::<String>()), cx);
                        return Err(anyhow!(error));
                    }
                };
                ensure!(response.document_id == this.document_id && response.version == snapshot.version, "Mismatched language-service snapshot");
                ensure!(this.runtime.is_none_or(|runtime| runtime == response.runtime_id), "Language-service runtime changed");
                this.runtime = Some(response.runtime_id);
                match diagnostics.as_ref() {
                    Some((items, ignored)) if !items.is_empty() || *ignored == 0 => {
                        this.settled_diagnostics =
                            Some((snapshot.clone(), Instant::now()));
                    }
                    Some(_) | None => this.settled_diagnostics = None,
                }
                let diagnostic_summary = diagnostics
                    .as_ref()
                    .map(|(items, ignored)| (items.len(), *ignored));
                let detail = if let Some(diagnostics) = diagnostics {
                    let (diagnostics, ignored) = diagnostics;
                    let count = diagnostics.len();
                    let _ = this.editor.update(cx, |editor, cx| {
                        let text = editor.text().clone();
                        let mut diagnostics = diagnostics;
                        diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.range.end));
                        if let Some(set) = editor.diagnostics_mut() {
                            set.reset(&text);
                            set.extend(diagnostics);
                        }
                        cx.notify();
                    });
                    format!(
                        "{} · {count} valid diagnostics · {ignored} ignored · unsaved buffer v{}",
                        response.server, snapshot.version
                    )
                } else {
                    format!("{} · diagnostics pending or unsupported · unsaved buffer v{}", response.server, snapshot.version)
                };
                let label = match diagnostic_summary {
                    Some((0, ignored)) if ignored > 0 => "LSP: diagnostics invalid".into(),
                    Some((count, _)) => format!("LSP: {count} diagnostics"),
                    None => "LSP: ready".into(),
                };
                this.status(&label, detail, cx);
                Ok(Some((snapshot, response)))
            })?
        })
    }

    fn show_document(
        &mut self,
        params: ShowDocumentParams,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.definition.clone() else {
            return;
        };
        let path = match relative_uri(&params.uri, &snapshot.root) {
            Ok(path) => path,
            Err(error) => {
                self.status("LSP: navigation unavailable", error.to_string(), cx);
                return;
            }
        };
        let owner = self.owner.clone();
        cx.spawn_in(window, async move |this, cx| {
            if !this
                .update(cx, |this, cx| this.current(&snapshot, cx))
                .unwrap_or(false)
            {
                return;
            }
            let root = snapshot.root.clone();
            let target_path = path.clone();
            let client = snapshot.client.clone();
            let result = match chat::executor() {
                Ok(runtime) => {
                    runtime
                        .spawn(async move {
                            project_io::validate_search_target(&client, &root, target_path).await
                        })
                        .await
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.status("LSP: navigation unavailable", error, cx)
                    });
                    return;
                }
            };
            let valid = this
                .update(cx, |this, cx| this.current(&snapshot, cx))
                .unwrap_or(false);
            if !valid {
                return;
            }
            match result {
                Ok(Ok(_)) => {
                    let _ = owner.update_in(cx, |owner, window, cx| {
                        owner.open_file_internal(&snapshot.root, &path, window, cx);
                        if let Some(tab) = owner
                            .active_tab_index
                            .and_then(|index| owner.tabs.get_mut(index))
                        {
                            if tab.project_dir == snapshot.root
                                && tab.relative_path == path
                                && (!tab.is_dirty || path == snapshot.path)
                            {
                                tab.pending_lsp_position =
                                    params.selection.map(|range| range.start);
                            }
                        }
                        cx.notify();
                    });
                }
                other => {
                    let _ = this.update(cx, |this, cx| {
                        this.status(
                            "LSP: navigation unavailable",
                            format!("Could not validate definition target: {other:?}"),
                            cx,
                        )
                    });
                }
            }
        })
        .detach();
    }
}

struct Provider(WeakEntity<LanguageService>);

impl Provider {
    fn query(
        &self,
        text: String,
        offset: usize,
        operation: EditorLspOperation,
        cx: &mut App,
    ) -> Task<Result<Option<(Snapshot, EditorLspResponse)>>> {
        self.0
            .update(cx, |service, cx| service.query(text, offset, operation, cx))
            .unwrap_or_else(|_| Task::ready(Ok(None)))
    }
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let text = text.to_string();
        let service = self.0.clone();
        // Trigger characters vary by server; an invoked completion is always valid.
        let task = self.query(
            text.clone(),
            offset,
            EditorLspOperation::Completion {
                trigger_character: None,
            },
            cx,
        );
        cx.spawn(async move |cx| {
            let Some((snapshot, response)) = task.await? else {
                return Ok(CompletionResponse::Array(vec![]));
            };
            let mapped = cx
                .background_executor()
                .spawn(async move { completions(response.result, &text, offset) })
                .await?;
            if service
                .update(cx, |service, cx| service.current(&snapshot, cx))
                .unwrap_or(false)
            {
                Ok(mapped)
            } else {
                Ok(CompletionResponse::Array(vec![]))
            }
        })
    }

    fn is_completion_trigger(&self, _: usize, text: &str, _: &mut App) -> bool {
        text.chars().count() == 1
            && text
                .chars()
                .any(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | ':'))
    }
}

impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let text = text.to_string();
        let service = self.0.clone();
        let task = self.query(text.clone(), offset, EditorLspOperation::Hover, cx);
        cx.spawn(async move |cx| {
            let Some((snapshot, response)) = task.await? else {
                return Ok(None);
            };
            if !service
                .update(cx, |service, cx| service.current(&snapshot, cx))
                .unwrap_or(false)
            {
                return Ok(None);
            }
            let mut hover: Option<Hover> = serde_json::from_value(response.result)?;
            if let Some(hover) = &mut hover {
                hover.range = hover
                    .range
                    .map(|range| scalar_range(&text, range))
                    .transpose()?;
            }
            Ok(hover)
        })
    }
}

impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let text = text.to_string();
        let service = self.0.clone();
        let task = self.query(text.clone(), offset, EditorLspOperation::Definition, cx);
        cx.spawn(async move |cx| {
            let Some((snapshot, response)) = task.await? else {
                return Ok(vec![]);
            };
            let Some(definition): Option<GotoDefinitionResponse> =
                serde_json::from_value(response.result)?
            else {
                return Ok(vec![]);
            };
            let mut links = match definition {
                GotoDefinitionResponse::Scalar(location) => vec![location_link(location)],
                GotoDefinitionResponse::Array(locations) => {
                    locations.into_iter().map(location_link).collect()
                }
                GotoDefinitionResponse::Link(links) => links,
            };
            for link in &mut links {
                link.origin_selection_range = link
                    .origin_selection_range
                    .map(|range| scalar_range(&text, range))
                    .transpose()?;
            }
            let current = service.update(cx, |service, cx| {
                if !service.current(&snapshot, cx) {
                    return false;
                }
                service.definition = Some(snapshot);
                true
            })?;
            if !current {
                return Ok(vec![]);
            }
            Ok(links)
        })
    }
}

fn location_link(location: lsp_types::Location) -> LocationLink {
    LocationLink {
        origin_selection_range: None,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: location.range,
    }
}

impl CodeActionProvider for Provider {
    fn id(&self) -> SharedString {
        "threadlane-daemon".into()
    }

    fn code_actions(
        &self,
        editor: Entity<EditorState>,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let text = editor.read(cx).value().to_string();
        let position = |offset| {
            wire_position(&text, offset).map(|position| EditorLspPosition {
                line: position.line,
                character: position.character,
            })
        };
        let (start, end) = match (position(range.start), position(range.end)) {
            (Ok(start), Ok(end)) => (start, end),
            _ => return Task::ready(Err(anyhow!("Invalid code action selection"))),
        };
        let service = self.0.clone();
        let task = self.query(
            text,
            range.start,
            EditorLspOperation::CodeActions {
                range: EditorLspRange { start, end },
            },
            cx,
        );
        cx.spawn(async move |cx| {
            let Some((snapshot, response)) = task.await? else { return Ok(vec![]) };
            let source = snapshot.clone();
            let (offered, cached) = cx.background_executor().spawn(async move {
                let actions: Option<Vec<CodeActionOrCommand>> = serde_json::from_value(response.result)?;
                let mut offered = vec![];
                let mut cached = vec![];
                for action in actions.unwrap_or_default().into_iter().take(100) {
                    let CodeActionOrCommand::CodeAction(action) = action else { continue };
                    let Ok(replacement) = action_text(&action, &source.text, &source.root, &source.path, response.server_document_version) else { continue };
                    if replacement.len() > MAX_EDITOR_LSP_BYTES { continue; }
                    cached.push((action.clone(), source.clone(), response.server_document_version));
                    offered.push(action);
                }
                Ok::<_, anyhow::Error>((offered, cached))
            }).await?;
            let current = service.update(cx, |service, cx| {
                if !service.current(&snapshot, cx) { return false; }
                if offered.is_empty() {
                    service.status("LSP: no safe actions", "No immediately available current-buffer actions. Server commands, unresolved actions and workspace-wide changes are not applied.", cx);
                }
                service.actions = cached;
                true
            })?;
            Ok(if current { offered } else { vec![] })
        })
    }

    fn perform_code_action(
        &self,
        editor: Entity<EditorState>,
        action: CodeAction,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        self.0
            .update(cx, |_, cx| {
                cx.spawn_in(window, async move |service, cx| {
                    service.update_in(cx, |service, window, cx| -> Result<()> {
                        ensure!(
                            editor.entity_id() == service.editor.entity_id(),
                            "Wrong code action buffer"
                        );
                        let (_, snapshot, server_version) = service
                            .actions
                            .iter()
                            .find(|(offered, _, _)| offered == &action)
                            .cloned()
                            .ok_or_else(|| anyhow!("Code action expired; request actions again"))?;
                        ensure!(
                            service.current(&snapshot, cx),
                            "Buffer changed; request actions again"
                        );
                        let replacement = action_text(
                            &action,
                            &snapshot.text,
                            &snapshot.root,
                            &snapshot.path,
                            server_version,
                        )?;
                        ensure!(
                            replacement.len() <= MAX_EDITOR_LSP_BYTES,
                            "Code action result exceeds the 1 MiB limit"
                        );
                        editor.update(cx, |editor, cx| editor.replace_all(replacement, window, cx));
                        service.actions.clear();
                        Ok(())
                    })?
                })
            })
            .unwrap_or_else(|error| Task::ready(Err(anyhow!(error))))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{LanguageService, Snapshot};
    use crate::view::EditorView;
    use gpui::{AppContext as _, Entity, Task, TestAppContext, VisualTestContext};
    use gpui_component::input::EditorState;
    use lsp_types::{CodeAction, Position, Range, TextEdit, WorkspaceEdit};
    use std::collections::HashMap;
    use threadlane_protocol::daemon::{ProjectInfo, SessionInfo};
    use threadlane_ui_kit::EditorLanguageRefresh;
    use threadlane_ui_state::AppState;

    fn fixture(
        cx: &mut TestAppContext,
    ) -> (
        Entity<EditorView>,
        Entity<AppState>,
        tempfile::TempDir,
        &mut VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_kit::init_editor);
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("sample.rs"), "fn original() {}\n").unwrap();
        let project = directory.path().to_path_buf();
        let model = cx.new(|_| {
            let mut model = AppState::for_tests();
            model.active_work_dir = Some(project.clone());
            model.active_session_id = Some("editor-test".into());
            model.projects = vec![ProjectInfo {
                name: "Fixture".into(),
                work_dir: project.clone(),
                is_expanded: true,
                sessions: vec![SessionInfo {
                    id: "editor-test".into(),
                    work_dir: project.clone(),
                    runtime_work_dir: project.clone(),
                    worktree_available: true,
                    ..Default::default()
                }],
            }];
            model
        });
        let retained_model = model.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|cx| {
                let mut view = EditorView::new(model, window, cx);
                view.open_file_internal(&project, "sample.rs", window, cx);
                view
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<EditorView>().unwrap()
        });
        let service = view.read_with(cx, |view, _| view.tabs[0].language_service.clone().unwrap());
        service.update(cx, |service, _| {
            // These interaction tests drive immutable reply snapshots directly;
            // they must not launch real language servers on the Tokio reactor.
            service._poll = Task::ready(());
            service.blocked = Some("fixture".into());
        });
        for _ in 0..4 {
            cx.run_until_parked();
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        (view, retained_model, directory, cx)
    }

    fn entities(
        view: &Entity<EditorView>,
        cx: &mut VisualTestContext,
    ) -> (Entity<LanguageService>, Entity<EditorState>) {
        view.read_with(cx, |view, _| {
            (
                view.tabs[0].language_service.clone().unwrap(),
                view.tabs[0].editor_state.clone().unwrap(),
            )
        })
    }

    fn capture(service: &Entity<LanguageService>, cx: &mut VisualTestContext) -> Snapshot {
        service.update(cx, |service, cx| service.capture(cx).unwrap())
    }

    #[gpui::test]
    fn editor_lsp_installs_providers_and_rejects_changed_snapshot(cx: &mut TestAppContext) {
        let (view, model, _directory, cx) = fixture(cx);
        let (service, buffer) = entities(&view, cx);
        buffer.read_with(cx, |buffer, _| {
            assert!(buffer.lsp().completion_provider.is_some());
            assert!(buffer.lsp().hover_provider.is_some());
            assert!(buffer.lsp().definition_provider.is_some());
            assert!(buffer.lsp().show_document.is_some());
            assert_eq!(buffer.lsp().code_action_providers.len(), 1);
        });
        let snapshot = capture(&service, cx);
        assert!(service.read_with(cx, |service, cx| service.current(&snapshot, cx)));
        cx.update(|window, cx| {
            buffer.update(cx, |buffer, cx| buffer.set_value("unsaved 😀", window, cx))
        });
        assert!(!service.read_with(cx, |service, cx| service.current(&snapshot, cx)));
        cx.update(|window, cx| {
            buffer.update(cx, |buffer, cx| {
                buffer.set_value(snapshot.text.clone(), window, cx)
            })
        });
        view.update(cx, |view, _| view.tabs[0].buffer_revision += 1);
        assert!(
            !service.read_with(cx, |service, cx| service.current(&snapshot, cx)),
            "edit then undo must not resurrect a stale request"
        );
        let fresh = capture(&service, cx);
        view.update(cx, |view, _| view.language_activation += 1);
        assert!(
            !service.read_with(cx, |service, cx| service.current(&fresh, cx)),
            "tab activation is part of request identity"
        );
        let fresh = capture(&service, cx);
        model.update(cx, |model, _| {
            model.active_session_id = Some("other-session".into())
        });
        assert!(!service.read_with(cx, |service, cx| service.current(&fresh, cx)));
    }

    #[gpui::test]
    fn editor_lsp_unavailable_completion_falls_back_and_accepts(cx: &mut TestAppContext) {
        let (view, model, _directory, cx) = fixture(cx);
        let (service, buffer) = entities(&view, cx);
        model.update(cx, |model, _| model.active_session_id = None);
        cx.update(|window, cx| {
            buffer.update(cx, |buffer, cx| {
                buffer.set_value("alpha_value\nalp_suffix", window, cx);
                buffer.set_cursor_position(Position::new(1, 3), window, cx);
                buffer.focus(window, cx);
            });
        });

        cx.simulate_keystrokes("h");
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(101));
        cx.run_until_parked();
        buffer.read_with(cx, |buffer, _| {
            assert_eq!(buffer.value().as_str(), "alpha_value\nalph_suffix");
            let menu = buffer.completion_menu_state();
            assert!(menu.open);
            assert_eq!(menu.items[0].label, "alpha_value");
        });

        service.update(cx, |service, _| {
            service.blocked = None;
            service.last_edit = Instant::now() - Duration::from_secs(1);
        });
        assert!(service
            .update(cx, |service, cx| service.poll_snapshot(cx))
            .is_none());
        buffer.read_with(cx, |buffer, _| {
            assert!(buffer.completion_menu_state().open);
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.value().to_string()),
            "alpha_value\nalpha_value"
        );
    }

    #[gpui::test]
    fn editor_lsp_settled_diagnostics_backoff_reopens_on_pending_edit_refresh_and_timeout(
        cx: &mut TestAppContext,
    ) {
        let (view, _model, _directory, cx) = fixture(cx);
        let (service, _) = entities(&view, cx);
        let settled = capture(&service, cx);
        service.update(cx, |service, cx| {
            service.blocked = None;
            service.last_edit = Instant::now() - Duration::from_secs(1);
            service.settled_diagnostics = Some((settled.clone(), Instant::now()));
            assert!(service.poll_snapshot(cx).is_none());

            service.settled_diagnostics = Some((
                settled.clone(),
                Instant::now() - Duration::from_secs(30),
            ));
            assert!(service.poll_snapshot(cx).is_some());

            service.settled_diagnostics = None;
            assert!(
                service.poll_snapshot(cx).is_some(),
                "pending diagnostics must retain the fast polling cadence"
            );
            service.settled_diagnostics = Some((settled.clone(), Instant::now()));
        });

        view.update(cx, |view, _| view.tabs[0].buffer_revision += 1);
        service.update(cx, |service, cx| {
            assert!(
                service.poll_snapshot(cx).is_some(),
                "a changed buffer must reopen diagnostics polling"
            );
        });

        let workbench = service.read_with(cx, |service, _| {
            service.workbench.upgrade().unwrap()
        });
        service.update(cx, |service, _| {
            service.settled_diagnostics = Some((settled, Instant::now()));
        });
        workbench.update(cx, |_, cx| cx.emit(EditorLanguageRefresh));
        service.read_with(cx, |service, _| assert!(service.settled_diagnostics.is_none()));
        service.update(cx, |service, cx| {
            assert!(
                service.poll_snapshot(cx).is_some(),
                "manual refresh must clear settled-diagnostics backoff"
            );
        });
    }

    #[gpui::test]
    fn editor_lsp_code_action_is_undoable_and_rechecks_revision(cx: &mut TestAppContext) {
        let (view, _model, directory, cx) = fixture(cx);
        let (service, buffer) = entities(&view, cx);
        let snapshot = capture(&service, cx);
        let uri = url::Url::from_file_path(directory.path().join("sample.rs"))
            .unwrap()
            .to_string()
            .parse()
            .unwrap();
        let action = CodeAction {
            title: "Rename local symbol".into(),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(
                    uri,
                    vec![TextEdit {
                        range: Range::new(Position::new(0, 3), Position::new(0, 11)),
                        new_text: "changed".into(),
                    }],
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };
        service.update(cx, |service, _| {
            service.actions = vec![(action.clone(), snapshot.clone(), Some(2))]
        });
        let provider = buffer.read_with(cx, |buffer, _| {
            buffer.lsp().code_action_providers[0].clone()
        });
        cx.update(|window, cx| {
            // GPUI invokes this hook while EditorState is already borrowed.
            buffer.update(cx, |buffer_state, cx| {
                buffer_state.focus(window, cx);
                provider
                    .perform_code_action(buffer.clone(), action.clone(), true, window, cx)
                    .detach();
            });
        });
        cx.run_until_parked();
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.value().to_string()),
            "fn changed() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("sample.rs")).unwrap(),
            "fn original() {}\n"
        );
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-z");
        cx.run_until_parked();
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.value().to_string()),
            snapshot.text.as_str()
        );
        service.update(cx, |service, _| {
            service.actions = vec![(action.clone(), snapshot, Some(2))]
        });
        cx.update(|window, cx| {
            provider
                .perform_code_action(buffer.clone(), action.clone(), true, window, cx)
                .detach()
        });
        cx.run_until_parked();
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.value().to_string()),
            "fn original() {}\n",
            "stale cached actions cannot apply after undo"
        );
    }

    #[gpui::test]
    fn editor_lsp_pending_definition_converts_utf16_target_columns(cx: &mut TestAppContext) {
        let (view, _model, _directory, cx) = fixture(cx);
        let (_, buffer) = entities(&view, cx);
        cx.update(|window, cx| {
            buffer.update(cx, |buffer, cx| buffer.set_value("😀 target", window, cx));
            view.update(cx, |view, _| {
                view.tabs[0].is_dirty = true;
                view.tabs[0].pending_lsp_position = Some(Position::new(0, 3));
            });
        });
        for _ in 0..3 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.update(|window, cx| window.simulate_next_frame(cx));
            cx.run_until_parked();
        }
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.cursor_position()),
            Position::new(0, 2)
        );
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.value().to_string()),
            "😀 target"
        );
    }
}
