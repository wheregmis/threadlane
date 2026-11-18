//! Owner operations over the hub's durable records.

use threadlane_core::client::{ApiError, LaneTarget, OperationsReply, OperationsRequest};
use threadlane_core::model::LaneKind;
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{StoreError, TaskStore};

use super::ClientApi;
use crate::error::api_error;
use crate::memory::scope;

pub(super) async fn handle(
    api: &ClientApi,
    request: OperationsRequest,
) -> Result<OperationsReply, ApiError> {
    let inner = &api.core.inner;
    let db = &inner.db;
    match request {
        OperationsRequest::Metrics { project } => {
            let project = match project {
                Some(name) => Some(api.project_named(Some(&name)).await?.id),
                None => None,
            };
            let metrics = db.operations_metrics(project).await.map_err(api_error)?;
            Ok(OperationsReply::Metrics {
                metrics: Box::new(metrics),
            })
        }
        OperationsRequest::Lane { target } => {
            let id = match target {
                LaneTarget::Task { task } => {
                    db.task(task)
                        .await
                        .map_err(api_error)?
                        .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?
                        .lane_id
                }
                target => api.target_lane(target).await?,
            };
            let lane = db.inspect_lane(id).await.map_err(operator_error)?;
            Ok(OperationsReply::Lane {
                lane: Box::new(lane),
            })
        }
        OperationsRequest::Replay { event } => {
            let event = db.replay_event(event).await.map_err(operator_error)?;
            inner.wake.wake(WakeTarget::Lane(event.lane_id));
            Ok(OperationsReply::Replayed { event })
        }
        OperationsRequest::Rollback {
            project,
            path,
            version,
        } => {
            let project = api.project_named(project.as_deref()).await?.id;
            let stored = scope(project, LaneKind::Lead, &path)
                .map_err(|error| ApiError::Invalid(error.to_string()))?;
            let mut file = db
                .rollback_memory(&stored, version, inner.clock.now())
                .await
                .map_err(|error| match error {
                    StoreError::NotFound { .. } => {
                        ApiError::NotFound(format!("version {version} of {path}"))
                    }
                    StoreError::Invalid(message) | StoreError::Conflict(message) => {
                        ApiError::Invalid(message.replace(&stored, &path))
                    }
                    other => api_error(other),
                })?;
            file.path = path;
            Ok(OperationsReply::RolledBack { file })
        }
        OperationsRequest::Trace { id } => {
            let trace = db.inspect_trace(&id).await.map_err(operator_error)?;
            Ok(OperationsReply::Trace { trace })
        }
        OperationsRequest::CheckMemory { project } => {
            let id = api.project_named(project.as_deref()).await?.id;
            let findings = crate::memory_check::check(db, id, inner.workspaces.as_deref())
                .await
                .map_err(api_error)?;
            Ok(OperationsReply::MemoryCheck { findings })
        }
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}

fn operator_error(error: StoreError) -> ApiError {
    match error {
        StoreError::NotFound { .. } => ApiError::NotFound(error.to_string()),
        StoreError::Conflict(message) => ApiError::Invalid(message),
        other => api_error(other),
    }
}
