//! The Timers capability of [`ClientCore`] (docs/milestone-8.md §2): your
//! routines, and the follow-ups and routines agents set.

use threadlane_core::client::{ApiError, TimerReply, TimerRequest};
use threadlane_core::ids::TimerId;
use threadlane_core::model::{Recurrence, Timer};

use crate::{ClientCore, ClientError, api_error, wrong_reply};

impl ClientCore {
    /// The timers of a project that have not ended, active or paused,
    /// oldest first. `space: None` means the only paired hub; `project:
    /// None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn routines(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<Timer>, ClientError> {
        let request = TimerRequest::List {
            project: project.map(str::to_owned),
        };
        match self.timers(space, request).await? {
            (_, TimerReply::Timers { timers }) => Ok(timers),
            (space, _) => Err(wrong_reply(&space, "timers")),
        }
    }

    /// Creates a routine of yours in a project's lead lane, with a fresh
    /// request id. `space: None` means the only paired hub; `project: None`
    /// means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn add_routine(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        instruction: &str,
        recurrence: Recurrence,
    ) -> Result<Timer, ClientError> {
        let request = TimerRequest::CreateRoutine {
            project: project.map(str::to_owned),
            instruction: instruction.to_owned(),
            recurrence,
            client_request_id: uuid::Uuid::new_v4().to_string(),
        };
        self.one_timer(space, request).await
    }

    /// Runs a saved routine immediately while preserving its cadence.
    pub async fn run_routine_once(
        &self,
        space: Option<&str>,
        timer: TimerId,
        client_request_id: String,
    ) -> Result<Timer, ClientError> {
        self.one_timer(
            space,
            TimerRequest::RunOnce {
                timer,
                client_request_id,
            },
        )
        .await
    }

    /// Pauses timer `timer` (its number, as `threadlane routines` lists it).
    ///
    /// # Errors
    ///
    /// No such hub or timer, several hubs and none named, or the hub's error.
    pub async fn pause_timer(
        &self,
        space: Option<&str>,
        timer: &str,
    ) -> Result<Timer, ClientError> {
        let timer = self.timer_id(space, timer)?;
        self.one_timer(space, TimerRequest::Pause { timer }).await
    }

    /// Resumes a paused timer.
    ///
    /// # Errors
    ///
    /// No such hub or timer, several hubs and none named, or the hub's error.
    pub async fn resume_timer(
        &self,
        space: Option<&str>,
        timer: &str,
    ) -> Result<Timer, ClientError> {
        let timer = self.timer_id(space, timer)?;
        self.one_timer(space, TimerRequest::Resume { timer }).await
    }

    /// Cancels a timer for good.
    ///
    /// # Errors
    ///
    /// No such hub or timer, several hubs and none named, or the hub's error.
    pub async fn cancel_timer(
        &self,
        space: Option<&str>,
        timer: &str,
    ) -> Result<Timer, ClientError> {
        let timer = self.timer_id(space, timer)?;
        self.one_timer(space, TimerRequest::Cancel { timer }).await
    }

    fn timer_id(&self, space: Option<&str>, timer: &str) -> Result<TimerId, ClientError> {
        let (name, _api) = self.target(space)?;
        timer
            .trim()
            .parse()
            .ok()
            .filter(|id| *id > 0)
            .map(TimerId)
            .ok_or_else(|| {
                api_error(
                    &name,
                    ApiError::Invalid(format!("{timer:?} is not a timer number")),
                )
            })
    }

    async fn one_timer(
        &self,
        space: Option<&str>,
        request: TimerRequest,
    ) -> Result<Timer, ClientError> {
        match self.timers(space, request).await? {
            (_, TimerReply::Timer { timer }) => Ok(timer),
            (space, _) => Err(wrong_reply(&space, "timer")),
        }
    }

    async fn timers(
        &self,
        space: Option<&str>,
        request: TimerRequest,
    ) -> Result<(String, TimerReply), ClientError> {
        let (space, api) = self.target(space)?;
        let reply = api
            .timers(request)
            .await
            .map_err(|e| api_error(&space, e))?;
        Ok((space, reply))
    }
}
