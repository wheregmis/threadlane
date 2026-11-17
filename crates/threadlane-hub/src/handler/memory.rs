//! The Memory capability (Plan.md §8.3, §11.1 "Memory"): a project's memory
//! as its lead sees it, and your own changes to it. Paths map to stored
//! ones through [`crate::memory::scope`] with the lead's reach, so no path
//! names another project (invariant 12).

use threadlane_core::client::{ApiError, MemoryReply, MemoryRequest};
use threadlane_core::model::{LaneKind, MemoryFile};
use threadlane_core::store::{MemoryStore, StoreError};

use super::ClientApi;
use crate::error::api_error;
use crate::memory::{scope, visible};

/// Answers a [`MemoryRequest`].
pub(super) async fn handle(
    api: &ClientApi,
    request: MemoryRequest,
) -> Result<MemoryReply, ApiError> {
    let inner = &api.core.inner;
    let db = &inner.db;
    let (project, path) = match &request {
        MemoryRequest::List { project } => (project.clone(), None),
        MemoryRequest::Read { project, path }
        | MemoryRequest::History { project, path }
        | MemoryRequest::Write { project, path, .. }
        | MemoryRequest::Delete { project, path } => (project.clone(), Some(path.clone())),
        other => return Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    };
    let id = api.project_named(project.as_deref()).await?.id;
    // A stored file as you name it; `None` for one this project cannot see.
    let named = |mut file: MemoryFile| {
        file.path = visible(id, LaneKind::Lead, &file.path)?;
        Some(file)
    };
    let path = path.as_deref();
    let stored = match path {
        Some(path) => Some(
            scope(id, LaneKind::Lead, path)
                .map_err(|error| ApiError::Invalid(error.to_string()))?,
        ),
        None => None,
    };
    let stored = stored.as_deref().unwrap_or_default();
    // A refusal with its paths as you name them.
    let refused = |error: StoreError| {
        let own = format!("projects/{}/", id.get());
        let named = |message: &str| message.replace(&own, "project/");
        match error {
            StoreError::Invalid(message) | StoreError::Conflict(message) => {
                ApiError::Invalid(named(&message))
            }
            StoreError::NotFound { .. } => {
                ApiError::NotFound(format!("there is no file at {}", path.unwrap_or_default()))
            }
            other => api_error(other),
        }
    };
    let file = |file: MemoryFile| {
        named(file)
            .map(|file| MemoryReply::File { file })
            .ok_or_else(|| ApiError::Internal("a file outside this project".to_owned()))
    };
    match request {
        MemoryRequest::List { .. } => {
            let mut files = db.list("space/").await.map_err(api_error)?;
            files.extend(
                db.list(&format!("projects/{}/", id.get()))
                    .await
                    .map_err(api_error)?,
            );
            Ok(MemoryReply::Files {
                files: files.into_iter().filter_map(named).collect(),
            })
        }
        MemoryRequest::Read { .. } => file(
            db.read(stored)
                .await
                .map_err(refused)?
                .ok_or_else(|| refused(StoreError::not_found("memory file", stored)))?,
        ),
        MemoryRequest::History { .. } => Ok(MemoryReply::History {
            versions: db
                .history(stored)
                .await
                .map_err(refused)?
                .into_iter()
                .filter_map(named)
                .collect(),
        }),
        MemoryRequest::Write { content, .. } => file(
            db.write_by_owner(stored, &content, inner.clock.now())
                .await
                .map_err(refused)?,
        ),
        MemoryRequest::Delete { .. } => file(
            db.delete_by_owner(stored, inner.clock.now())
                .await
                .map_err(refused)?,
        ),
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}
