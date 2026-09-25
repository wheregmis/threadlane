use super::*;

impl CodingSessionHarness {
    /// A Fusion audit fact has a unique key, so replay keeps every decision
    /// alongside the existing per-run usage and provider trace records.
    pub(crate) fn record_fusion_audit(
        &mut self,
        lane: &str,
        run_id: Option<&str>,
        event: serde_json::Value,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        let seq = harness_next_seq(self.store.store());
        let record = HarnessRecord::FactSet {
            id: format!("fusion-audit-{seq}"),
            seq,
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.map(str::to_owned),
            key: format!("fusion_audit:{seq}"),
            value: serde_json::to_string(&event).map_err(|error| error.to_string())?,
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Usage ─────────────────────────────────────────────────────────

    /// Record provider token usage for a run.
    pub fn record_provider_usage(&mut self, run_id: &str, usage: TokenUsage) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .record_provider_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Record discarded (non-terminal) token usage.
    pub fn record_discarded_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .record_discarded_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Retry ─────────────────────────────────────────────────────────

    /// Schedule a retry for a failed run.
    pub(crate) fn schedule_retry(&mut self, run_id: &str, reason: &str) -> Result<u32, String> {
        self.ensure_fresh()?;
        let attempt = self
            .store
            .schedule_retry(
                run_id,
                reason,
                RetryPolicy {
                    max_attempts: 3,
                    base_delay: 1_000,
                    max_delay: 8_000,
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    /// Begin a previously scheduled retry attempt.
    pub fn begin_retry(&mut self, run_id: &str) -> Result<u32, String> {
        self.ensure_fresh()?;
        let attempt = self
            .store
            .begin_retry(run_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    // ── Deferred ──────────────────────────────────────────────────────

    /// Redeem a deferred operation and optionally finish the run.
    pub fn redeem_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, String> {
        self.ensure_fresh()?;
        let terminal = self
            .store
            .redeem_deferred(run_id, resolution)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        if terminal {
            self.finish_run(run_id, OperationOutcome::Completed, None)?;
        }
        Ok(terminal)
    }

    // ── Compaction ────────────────────────────────────────────────────

    /// Accept a compaction summary.
    pub fn accept_compaction(&mut self, run_id: &str, summary: &str) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .accept_compaction(run_id, summary, &[])
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Facts ─────────────────────────────────────────────────────────

    /// Set a session-level fact.
    pub fn set_fact(&mut self, lane: &str, key: &str, value: String) -> Result<(), String> {
        self.ensure_fresh()?;
        self.store
            .set_fact(lane, key, value, None)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
}
