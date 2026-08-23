use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    time::MissedTickBehavior,
};
use tracing::{error, warn};
use uuid::Uuid;

use crate::{
    VERSION,
    app_server::{
        AppServerClient, AppServerConfig as ClientConfig, AppServerError, AppServerErrorKind,
        AppServerLimits, BackendStatus, QueueAddOutcome, QueueDeleteOutcome, QueueOwnership,
        WriteDisposition, adapter_config_for_thread, text_queue_input,
    },
    config::{Config, ConsentMode},
    control::{
        CONTROL_PROTOCOL_VERSION, ControlError, ControlRequest, ControlResponse,
        MAX_CONTROL_FRAME_BYTES, OriginContext,
    },
    model::{
        AppServerDoctorSummary, AuthorityState, AuthoritySummary, AutomationDoctorArgs,
        AutomationDoctorResult, AutomationGetArgs, AutomationGetResult, AutomationKind,
        AutomationListArgs, AutomationListResult, AutomationRunArgs, AutomationRunOutcome,
        AutomationRunResult, AutomationRunsArgs, AutomationRunsResult, AutomationState,
        AutomationTarget, AutomationUpdateArgs, AutomationUpdateOutcome, AutomationUpdateResult,
        AutomationView, BackendMode, BackendSummary, BoundedError, ClockDoctorSummary, ClockState,
        DaemonDoctorSummary, DatabaseDoctorSummary, DoctorCheck, DoctorOverall, EndpointKind,
        NominalOccurrence, OccurrenceIdentitySeed, OccurrencePreview, PromptSummary, RunState,
        RunView, SchedulerDoctorSummary, StrictQueueCapability, TargetSource, UserAction,
        UserActionCode, Warning,
    },
    schedule::{compute_catch_up, next_occurrence, normalize_catch_up, normalize_schedule},
    store::{Store, StoredAutomation, StoredRun},
};

const IDEMPOTENCY_TTL_DAYS: i64 = 30;
const PREVIEW_COUNT: usize = 5;
const SCHEDULER_BATCH: usize = 64;
const CONTROL_IO_TIMEOUT: StdDuration = StdDuration::from_secs(5);

pub async fn run(config: Config) -> Result<()> {
    config.validate()?;
    secure_directory(&config.state_dir)?;
    secure_directory(&config.runtime_dir)?;
    let lock = acquire_lock(&config.lock_path())?;
    let store = Store::open(config.database_path()).await?;
    let socket_path = config.control_socket_path();
    prepare_socket_path(&socket_path)?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    secure_file(&socket_path)?;
    let _socket_guard = SocketGuard(socket_path);

    let mut daemon = Daemon::new(config, store, lock);
    let mut ticker = tokio::time::interval(daemon.config.poll_interval());
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("failed to accept control connection")?;
                if let Err(error) = daemon.serve_one(stream).await {
                    warn!(%error, "control request failed before a response could be sent");
                }
            }
            _ = ticker.tick() => {
                daemon.observe_clock();
                if daemon.clock_trusted() {
                    if let Err(error) = daemon.scheduler_tick().await {
                        error!(%error, "scheduler tick failed");
                    }
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to install shutdown signal handler")?;
                break;
            }
        }
    }
    Ok(())
}

struct Daemon {
    config: Config,
    store: Store,
    _lock: File,
    started: Instant,
    clock: ClockMonitor,
}

impl Daemon {
    fn new(config: Config, store: Store, lock: File) -> Self {
        let threshold = config.scheduler.clock_jump_quarantine_seconds;
        Self {
            config,
            store,
            _lock: lock,
            started: Instant::now(),
            clock: ClockMonitor::new(threshold),
        }
    }

    async fn serve_one(&mut self, mut stream: UnixStream) -> Result<()> {
        let request =
            match tokio::time::timeout(CONTROL_IO_TIMEOUT, read_one_request(&mut stream)).await {
                Ok(request) => request,
                Err(_) => Err(api_error(
                    "CONTROL_TIMEOUT",
                    "control request timed out",
                    true,
                )),
            };
        let response = match request {
            Ok(request) => self.dispatch(request).await,
            Err(error) => ControlResponse::failure(Uuid::nil(), error),
        };
        let mut encoded = serde_json::to_vec(&response).context("failed to encode response")?;
        if encoded.len() > MAX_CONTROL_FRAME_BYTES {
            let fallback = ControlResponse::failure(
                response.request_id,
                api_error(
                    "RESPONSE_TOO_LARGE",
                    "daemon response exceeded its bound",
                    false,
                ),
            );
            encoded = serde_json::to_vec(&fallback)?;
        }
        encoded.push(b'\n');
        tokio::time::timeout(CONTROL_IO_TIMEOUT, stream.write_all(&encoded))
            .await
            .context("timed out writing control response")??;
        tokio::time::timeout(CONTROL_IO_TIMEOUT, stream.shutdown())
            .await
            .context("timed out closing control response")??;
        Ok(())
    }

    async fn dispatch(&mut self, request: ControlRequest) -> ControlResponse {
        let request_id = request.request_id;
        let result = self.dispatch_inner(request).await;
        match result {
            Ok(value) => ControlResponse::success(request_id, value),
            Err(error) => ControlResponse::failure(request_id, error),
        }
    }

    async fn dispatch_inner(
        &mut self,
        request: ControlRequest,
    ) -> std::result::Result<Value, ControlError> {
        if request.protocol_version != CONTROL_PROTOCOL_VERSION {
            return Err(api_error(
                "CONTROL_PROTOCOL_MISMATCH",
                "control protocol version is not supported",
                false,
            ));
        }
        validate_origin(&request.origin)?;
        match request.tool.as_str() {
            "automation_update" => {
                let arguments: AutomationUpdateArgs = decode_arguments(request.arguments.clone())?;
                let key = update_request_key(&arguments).to_owned();
                let token = self
                    .idempotency_begin(
                        &request.origin,
                        request.tool.as_str(),
                        &key,
                        &request.arguments,
                    )
                    .await?;
                if let Some(cached) = token.cached {
                    return Ok(cached);
                }
                let result = self
                    .automation_update(
                        &request.origin,
                        arguments,
                        token.started_at,
                        &token.argument_digest,
                    )
                    .await;
                self.idempotency_finish(token, result).await
            }
            "automation_run" => {
                let arguments: AutomationRunArgs = decode_arguments(request.arguments.clone())?;
                let key = run_request_key(&arguments).to_owned();
                let token = self
                    .idempotency_begin(
                        &request.origin,
                        request.tool.as_str(),
                        &key,
                        &request.arguments,
                    )
                    .await?;
                if let Some(cached) = token.cached {
                    return Ok(cached);
                }
                let result = self
                    .automation_run(
                        &request.origin,
                        arguments,
                        token.started_at,
                        &token.argument_digest,
                    )
                    .await;
                self.idempotency_finish(token, result).await
            }
            "automation_get" => {
                let arguments = decode_arguments(request.arguments)?;
                encode_result(self.automation_get(&request.origin, arguments).await?)
            }
            "automation_list" => {
                let arguments = decode_arguments(request.arguments)?;
                encode_result(self.automation_list(&request.origin, arguments).await?)
            }
            "automation_runs" => {
                let arguments = decode_arguments(request.arguments)?;
                encode_result(self.automation_runs(&request.origin, arguments).await?)
            }
            "automation_doctor" => {
                let arguments = decode_arguments(request.arguments)?;
                encode_result(self.automation_doctor(&request.origin, arguments).await?)
            }
            "_admin_authorize_task" => {
                if request.origin.call_id.is_some() {
                    return Err(api_error(
                        "ADMIN_TOOL_FORBIDDEN",
                        "private administration is unavailable to MCP callers",
                        false,
                    ));
                }
                let arguments: AdminAuthorizeArgs = decode_arguments(request.arguments)?;
                self.admin_authorize(&request.origin, arguments).await
            }
            _ => Err(api_error("UNKNOWN_TOOL", "unknown daemon tool", false)),
        }
    }

    async fn idempotency_begin(
        &self,
        origin: &OriginContext,
        tool: &str,
        request_key: &str,
        arguments: &Value,
    ) -> std::result::Result<IdempotencyToken, ControlError> {
        validate_request_key(request_key)?;
        let scope = digest_parts(&["request-v1", &origin.thread_id, tool, request_key]);
        let argument_digest = digest_json(arguments)?;
        let started_at = Utc::now();
        let owner_nonce = Uuid::new_v4().to_string();
        let pending = IdempotencyEnvelope {
            version: 1,
            argument_digest: argument_digest.clone(),
            started_at,
            owner_nonce,
            outcome: IdempotencyOutcome::Pending,
        };
        let pending_value = serde_json::to_value(&pending).map_err(internal_json_error)?;
        if let Some(cached) = self
            .store
            .idempotency_reserve(
                scope.clone(),
                pending_value.clone(),
                Utc::now() + Duration::days(IDEMPOTENCY_TTL_DAYS),
            )
            .await
            .map_err(storage_error)?
        {
            let envelope: IdempotencyEnvelope = serde_json::from_value(cached).map_err(|_| {
                api_error(
                    "IDEMPOTENCY_RECORD_INVALID",
                    "stored request-key record is invalid",
                    false,
                )
            })?;
            if envelope.argument_digest != argument_digest {
                return Err(api_error(
                    "REQUEST_KEY_CONFLICT",
                    "requestKey was already used with different arguments",
                    false,
                ));
            }
            return match envelope.outcome {
                IdempotencyOutcome::Succeeded { result } => Ok(IdempotencyToken {
                    scope,
                    argument_digest,
                    started_at: envelope.started_at,
                    owner_nonce: envelope.owner_nonce,
                    pending_value,
                    cached: Some(result),
                }),
                IdempotencyOutcome::Failed { error } => Err(error),
                IdempotencyOutcome::Pending => Err(ControlError {
                    code: "REQUEST_STATUS_UNCERTAIN".to_owned(),
                    message: "a prior attempt was interrupted; inspect task state before using a new requestKey"
                        .to_owned(),
                    retryable: false,
                    details: Some(json!({
                        "recovery": "read_task_state_before_new_request"
                    })),
                }),
            };
        }
        Ok(IdempotencyToken {
            scope,
            argument_digest,
            started_at,
            owner_nonce: pending.owner_nonce,
            pending_value,
            cached: None,
        })
    }

    async fn idempotency_finish(
        &self,
        token: IdempotencyToken,
        result: std::result::Result<Value, ControlError>,
    ) -> std::result::Result<Value, ControlError> {
        let outcome = match &result {
            Ok(result) => IdempotencyOutcome::Succeeded {
                result: result.clone(),
            },
            Err(error) => IdempotencyOutcome::Failed {
                error: error.clone(),
            },
        };
        let envelope = serde_json::to_value(IdempotencyEnvelope {
            version: 1,
            argument_digest: token.argument_digest,
            started_at: token.started_at,
            owner_nonce: token.owner_nonce,
            outcome,
        })
        .map_err(internal_json_error)?;
        let completed = self
            .store
            .idempotency_complete(
                token.scope,
                token.pending_value,
                envelope,
                Utc::now() + Duration::days(IDEMPOTENCY_TTL_DAYS),
            )
            .await
            .map_err(storage_error)?;
        if !completed {
            return Err(api_error(
                "IDEMPOTENCY_COMPLETION_CONFLICT",
                "request completion could not be committed safely",
                false,
            ));
        }
        result
    }

    async fn automation_update(
        &mut self,
        origin: &OriginContext,
        arguments: AutomationUpdateArgs,
        now: DateTime<Utc>,
        argument_digest: &str,
    ) -> std::result::Result<Value, ControlError> {
        let (outcome, mut record) = match arguments {
            AutomationUpdateArgs::SuggestedCreate {
                request_key,
                name,
                prompt,
                schedule,
                catch_up,
            } => {
                validate_name_prompt(&name, &prompt, self.config.scheduler.max_prompt_bytes)?;
                let schedule = normalize_schedule(&schedule, now).map_err(schedule_error)?;
                let catch_up = normalize_catch_up(catch_up.as_ref()).map_err(schedule_error)?;
                let id = automation_id_for_request(
                    &origin.thread_id,
                    "suggested_create",
                    &request_key,
                    argument_digest,
                );
                let authority = self.authority(origin, now).await?;
                let view = AutomationView {
                    id,
                    revision: 1,
                    name,
                    prompt: prompt_summary(&prompt),
                    kind: AutomationKind::Heartbeat,
                    target: AutomationTarget {
                        thread_id: origin.thread_id.clone(),
                        source: TargetSource::OriginatingTask,
                    },
                    next_nominal_due_at: initial_due(&schedule, now)?,
                    schedule,
                    catch_up,
                    state: AutomationState::Paused,
                    pending_run_count: 0,
                    authority,
                    created_at: now,
                    updated_at: now,
                    blocker: Some(UserAction {
                        code: UserActionCode::ConsentRequired,
                        safe_message: "Authorize this task before resuming the automation"
                            .to_owned(),
                    }),
                };
                (
                    AutomationUpdateOutcome::Suggested,
                    StoredAutomation { view, prompt },
                )
            }
            AutomationUpdateArgs::Create {
                request_key,
                name,
                prompt,
                schedule,
                catch_up,
            } => {
                self.require_delivery_configured()?;
                validate_name_prompt(&name, &prompt, self.config.scheduler.max_prompt_bytes)?;
                let schedule = normalize_schedule(&schedule, now).map_err(schedule_error)?;
                let catch_up = normalize_catch_up(catch_up.as_ref()).map_err(schedule_error)?;
                self.require_authorized(origin, now).await?;
                let id = automation_id_for_request(
                    &origin.thread_id,
                    "create",
                    &request_key,
                    argument_digest,
                );
                let view = AutomationView {
                    id,
                    revision: 1,
                    name,
                    prompt: prompt_summary(&prompt),
                    kind: AutomationKind::Heartbeat,
                    target: AutomationTarget {
                        thread_id: origin.thread_id.clone(),
                        source: TargetSource::OriginatingTask,
                    },
                    next_nominal_due_at: initial_due(&schedule, now)?,
                    schedule,
                    catch_up,
                    state: AutomationState::Active,
                    pending_run_count: 0,
                    authority: self.authority(origin, now).await?,
                    created_at: now,
                    updated_at: now,
                    blocker: None,
                };
                (
                    AutomationUpdateOutcome::Created,
                    StoredAutomation { view, prompt },
                )
            }
            AutomationUpdateArgs::Update {
                automation_id,
                expected_revision,
                patch,
                ..
            } => {
                let mut record = self.get_owned(origin, &automation_id).await?;
                require_revision(record.view.revision, expected_revision)?;

                if let Some(name) = patch.name.as_deref() {
                    validate_name(name)?;
                }
                if let Some(prompt) = patch.prompt.as_deref() {
                    validate_prompt(prompt, self.config.scheduler.max_prompt_bytes)?;
                }
                let normalized_schedule = patch
                    .schedule
                    .as_ref()
                    .map(|schedule| normalize_schedule(schedule, now).map_err(schedule_error))
                    .transpose()?;
                let normalized_catch_up = patch
                    .catch_up
                    .as_ref()
                    .map(|catch_up| normalize_catch_up(Some(catch_up)).map_err(schedule_error))
                    .transpose()?;
                if record.view.state == AutomationState::Active
                    && (patch.prompt.is_some()
                        || patch.schedule.is_some()
                        || patch.catch_up.is_some())
                {
                    self.require_delivery_configured()?;
                    self.require_authorized(origin, now).await?;
                }
                if let Some(name) = patch.name {
                    record.view.name = name;
                }
                if let Some(prompt) = patch.prompt {
                    record.view.prompt = prompt_summary(&prompt);
                    record.prompt = prompt;
                }
                if let Some(schedule) = normalized_schedule {
                    record.view.schedule = schedule;
                    record.view.next_nominal_due_at = initial_due(&record.view.schedule, now)?;
                }
                if let Some(catch_up) = normalized_catch_up {
                    record.view.catch_up = catch_up;
                }
                record.view.revision = record.view.revision.saturating_add(1);
                record.view.updated_at = now;
                self.replace_owned(origin, expected_revision, &record)
                    .await?;
                (AutomationUpdateOutcome::Updated, record)
            }
            AutomationUpdateArgs::Pause {
                automation_id,
                expected_revision,
                ..
            } => {
                let mut record = self.get_owned(origin, &automation_id).await?;
                require_revision(record.view.revision, expected_revision)?;
                record.view.revision = record.view.revision.saturating_add(1);
                record.view.state = AutomationState::Paused;
                record.view.updated_at = now;
                record.view.blocker = None;
                self.replace_owned(origin, expected_revision, &record)
                    .await?;
                (AutomationUpdateOutcome::Paused, record)
            }
            AutomationUpdateArgs::Resume {
                automation_id,
                expected_revision,
                ..
            } => {
                self.require_delivery_configured()?;
                let mut record = self.get_owned(origin, &automation_id).await?;
                require_revision(record.view.revision, expected_revision)?;
                self.require_authorized(origin, now).await?;
                record.view.revision = record.view.revision.saturating_add(1);
                record.view.state = AutomationState::Active;
                record.view.updated_at = now;
                record.view.authority = self.authority(origin, now).await?;
                record.view.blocker = None;
                self.replace_owned(origin, expected_revision, &record)
                    .await?;
                (AutomationUpdateOutcome::Resumed, record)
            }
            AutomationUpdateArgs::SoftDelete {
                automation_id,
                expected_revision,
                cancel_pending,
                ..
            } => {
                if cancel_pending {
                    return Err(api_error(
                        "CANCEL_PENDING_REQUIRES_RUN_TOOL",
                        "cancel pending runs individually with automation_run",
                        false,
                    ));
                }
                let mut record = self.get_owned(origin, &automation_id).await?;
                require_revision(record.view.revision, expected_revision)?;
                record.view.revision = record.view.revision.saturating_add(1);
                record.view.state = AutomationState::Deleted;
                record.view.next_nominal_due_at = None;
                record.view.updated_at = now;
                self.replace_owned(origin, expected_revision, &record)
                    .await?;
                (AutomationUpdateOutcome::Deleted, record)
            }
        };

        if matches!(
            outcome,
            AutomationUpdateOutcome::Suggested | AutomationUpdateOutcome::Created
        ) {
            let inserted = self
                .store
                .create_automation_if_absent(record.clone())
                .await
                .map_err(storage_error)?;
            if !inserted {
                record = self
                    .store
                    .get_automation(origin.thread_id.clone(), record.view.id.clone())
                    .await
                    .map_err(storage_error)?
                    .ok_or_else(|| {
                        api_error(
                            "IDEMPOTENCY_RECORD_INVALID",
                            "deterministic automation identity is not task-scoped",
                            false,
                        )
                    })?;
            }
        }
        let previews =
            upcoming_previews(&record.view.id, &record.view.schedule, now, PREVIEW_COUNT)?;
        let authority = record.view.authority.clone();
        let requires_user_action = (outcome == AutomationUpdateOutcome::Suggested)
            .then(|| record.view.blocker.clone())
            .flatten();
        let result = AutomationUpdateResult {
            outcome,
            automation: record.view,
            next_occurrences: previews,
            authority,
            backend: self.backend_summary(),
            warnings: self.backend_warnings(),
            requires_user_action,
        };
        encode_result(result)
    }

    async fn automation_get(
        &self,
        origin: &OriginContext,
        arguments: AutomationGetArgs,
    ) -> std::result::Result<AutomationGetResult, ControlError> {
        let record = self.get_owned(origin, &arguments.automation_id).await?;
        let recent_run = self
            .store
            .latest_run_for_automation(origin.thread_id.clone(), arguments.automation_id)
            .await
            .map_err(storage_error)?;
        Ok(AutomationGetResult {
            automation: record.view,
            recent_run,
            warnings: self.backend_warnings(),
        })
    }

    async fn automation_list(
        &self,
        origin: &OriginContext,
        arguments: AutomationListArgs,
    ) -> std::result::Result<AutomationListResult, ControlError> {
        let (automations, next_cursor) = self
            .store
            .list_automations(origin.thread_id.clone(), arguments)
            .await
            .map_err(storage_error)?;
        Ok(AutomationListResult {
            automations,
            next_cursor,
        })
    }

    async fn automation_runs(
        &self,
        origin: &OriginContext,
        arguments: AutomationRunsArgs,
    ) -> std::result::Result<AutomationRunsResult, ControlError> {
        let (runs, next_cursor) = self
            .store
            .list_runs(origin.thread_id.clone(), arguments)
            .await
            .map_err(storage_error)?;
        Ok(AutomationRunsResult { runs, next_cursor })
    }

    async fn automation_run(
        &mut self,
        origin: &OriginContext,
        arguments: AutomationRunArgs,
        now: DateTime<Utc>,
        argument_digest: &str,
    ) -> std::result::Result<Value, ControlError> {
        let result = match arguments {
            AutomationRunArgs::RunNow {
                request_key,
                automation_id,
                expected_revision,
            } => {
                self.require_delivery_configured()?;
                let automation = self.get_owned(origin, &automation_id).await?;
                require_revision(automation.view.revision, expected_revision)?;
                if self
                    .store
                    .task_has_nonterminal_run(origin.thread_id.clone())
                    .await
                    .map_err(storage_error)?
                {
                    return Err(api_error(
                        "RUN_OVERLAP_FORBIDDEN",
                        "the task already has nonterminal automation work",
                        true,
                    ));
                }
                self.require_authorized(origin, now).await?;
                let logical_id = digest_parts(&[
                    "manual-occurrence-v2",
                    &origin.thread_id,
                    &automation_id,
                    &request_key,
                    argument_digest,
                ]);
                let candidate = make_run(&automation, now, logical_id, None)?;
                let run = self
                    .materialize_and_deliver(&automation, candidate)
                    .await
                    .map_err(storage_error)?;
                AutomationRunResult {
                    outcome: AutomationRunOutcome::RunCreated,
                    run: run.view,
                    shared_queue: None,
                    authority: automation.view.authority,
                    warnings: self.backend_warnings(),
                    requires_user_action: None,
                }
            }
            AutomationRunArgs::Cancel {
                run_id,
                expected_run_revision,
                ..
            } => {
                let mut run = self.get_owned_run(origin, &run_id).await?;
                require_revision(run.view.revision, expected_run_revision)?;
                let mut requires_user_action = None;
                let outcome = match run.view.state {
                    RunState::Scheduled | RunState::Due => {
                        run.view.state = RunState::Cancelled;
                        run.view.revision = run.view.revision.saturating_add(1);
                        run.view.completed_at = Some(now);
                        self.replace_run(origin, expected_run_revision, &run)
                            .await?;
                        AutomationRunOutcome::Cancelled
                    }
                    RunState::ThreadQueued => {
                        self.require_delivery_configured()?;
                        self.require_authorized(origin, now).await?;
                        let automation = self.get_owned(origin, &run.view.automation_id).await?;
                        let client_config = self
                            .client_config(&origin.thread_id)
                            .map_err(|_| app_configuration_error())?;
                        let mut client = AppServerClient::connect(client_config)
                            .await
                            .map_err(app_control_error)?;
                        let ownership = ownership_for_run(&automation, &run, client.limits())?;
                        match client.delete_exact(&ownership).await {
                            Ok(QueueDeleteOutcome::Deleted) => {
                                run.view.state = RunState::Cancelled;
                                run.view.revision = run.view.revision.saturating_add(1);
                                run.view.completed_at = Some(now);
                                run.view.phase = Some("remote_queue_deleted".to_owned());
                                self.replace_run(origin, expected_run_revision, &run)
                                    .await?;
                                AutomationRunOutcome::Cancelled
                            }
                            Ok(QueueDeleteOutcome::AbsentBeforeMutation) => {
                                AutomationRunOutcome::TooLate
                            }
                            Ok(QueueDeleteOutcome::NotDeleted) => {
                                mark_remote_uncertain(
                                    &mut run,
                                    "CANCEL_RESULT_UNCERTAIN",
                                    "the exact queue row existed but deletion was not confirmed",
                                );
                                self.replace_run(origin, expected_run_revision, &run)
                                    .await?;
                                requires_user_action = Some(UserAction {
                                    code: UserActionCode::DeliveryUncertain,
                                    safe_message:
                                        "Verify task queue/history before retrying cancellation"
                                            .to_owned(),
                                });
                                AutomationRunOutcome::CancelUncertain
                            }
                            Err(error)
                                if error.write_disposition()
                                    == WriteDisposition::MayHaveCommitted =>
                            {
                                mark_remote_uncertain(
                                    &mut run,
                                    app_error_code(error.kind()),
                                    "remote cancellation may have committed",
                                );
                                self.replace_run(origin, expected_run_revision, &run)
                                    .await?;
                                requires_user_action = Some(UserAction {
                                    code: UserActionCode::DeliveryUncertain,
                                    safe_message:
                                        "Verify task queue/history before retrying cancellation"
                                            .to_owned(),
                                });
                                AutomationRunOutcome::CancelUncertain
                            }
                            Err(error)
                                if error.kind() == AppServerErrorKind::OwnershipViolation =>
                            {
                                mark_ownership_violation(&mut run);
                                self.replace_run(origin, expected_run_revision, &run)
                                    .await?;
                                requires_user_action = Some(UserAction {
                                    code: UserActionCode::RemoteOwnershipViolation,
                                    safe_message: "Remote queue ownership could not be proven"
                                        .to_owned(),
                                });
                                AutomationRunOutcome::CancelUncertain
                            }
                            Err(error) => return Err(app_control_error(error)),
                        }
                    }
                    _ => AutomationRunOutcome::TooLate,
                };
                AutomationRunResult {
                    outcome,
                    run: run.view,
                    shared_queue: None,
                    authority: self.authority(origin, now).await?,
                    warnings: self.backend_warnings(),
                    requires_user_action,
                }
            }
            AutomationRunArgs::Retry {
                request_key,
                run_id,
                expected_run_revision,
            } => {
                self.require_delivery_configured()?;
                let source = self.get_owned_run(origin, &run_id).await?;
                require_revision(source.view.revision, expected_run_revision)?;
                if !matches!(
                    source.view.state,
                    RunState::DeliveryFailed
                        | RunState::TurnFailed
                        | RunState::Interrupted
                        | RunState::DeliveryUncertain
                ) {
                    return Err(api_error(
                        "RUN_NOT_RETRYABLE",
                        "run is not retryable",
                        false,
                    ));
                }
                let automation = self.get_owned(origin, &source.view.automation_id).await?;
                self.require_authorized(origin, now).await?;
                let logical_id = digest_parts(&[
                    "retry-occurrence-v2",
                    &source.view.logical_occurrence_id,
                    &request_key,
                    argument_digest,
                ]);
                let candidate = make_run(&automation, now, logical_id, Some(source.view.id))?;
                let run = self
                    .materialize_and_deliver(&automation, candidate)
                    .await
                    .map_err(storage_error)?;
                AutomationRunResult {
                    outcome: AutomationRunOutcome::RetryCreated,
                    run: run.view,
                    shared_queue: None,
                    authority: automation.view.authority,
                    warnings: self.backend_warnings(),
                    requires_user_action: None,
                }
            }
            AutomationRunArgs::StartBlockedRun {
                run_id,
                expected_run_revision,
                ..
            } => {
                self.require_delivery_configured()?;
                let mut run = self.get_owned_run(origin, &run_id).await?;
                require_revision(run.view.revision, expected_run_revision)?;
                if !matches!(
                    run.view.state,
                    RunState::ThreadQueued | RunState::Interrupted
                ) {
                    return Err(api_error(
                        "RUN_NOT_STARTABLE",
                        "run is not an exact queued or interrupted item",
                        false,
                    ));
                }
                let automation = self.get_owned(origin, &run.view.automation_id).await?;
                self.require_authorized(origin, now).await?;
                let client_config = self
                    .client_config(&origin.thread_id)
                    .map_err(|_| app_configuration_error())?;
                let mut client = AppServerClient::connect(client_config)
                    .await
                    .map_err(app_control_error)?;
                let ownership = ownership_for_run(&automation, &run, client.limits())?;
                let mut requires_user_action = None;
                let outcome = match client.start_exact_head(&ownership).await {
                    Ok(started) => {
                        run.view.revision = run.view.revision.saturating_add(1);
                        run.view.state = RunState::Running;
                        run.view.phase = Some("remote_turn_running".to_owned());
                        run.view.turn_id = Some(started.turn_id);
                        run.view.started_at = Some(now);
                        run.view.start_lag_ms = Some(
                            now.signed_duration_since(run.view.nominal_due_at)
                                .num_milliseconds(),
                        );
                        self.replace_run(origin, expected_run_revision, &run)
                            .await?;
                        AutomationRunOutcome::BlockedRunStarted
                    }
                    Err(error)
                        if error.write_disposition() == WriteDisposition::MayHaveCommitted =>
                    {
                        mark_remote_uncertain(
                            &mut run,
                            app_error_code(error.kind()),
                            "remote queue start may have committed",
                        );
                        self.replace_run(origin, expected_run_revision, &run)
                            .await?;
                        requires_user_action = Some(UserAction {
                            code: UserActionCode::DeliveryUncertain,
                            safe_message: "Verify task history before attempting another start"
                                .to_owned(),
                        });
                        AutomationRunOutcome::StartUncertain
                    }
                    Err(error) if error.kind() == AppServerErrorKind::OwnershipViolation => {
                        // `start_exact_head` intentionally collapses an absent row and a
                        // mismatched row into one safe error, so do not overclaim which occurred.
                        mark_remote_uncertain(
                            &mut run,
                            "START_OWNERSHIP_OR_ABSENCE_UNRESOLVED",
                            "the exact queue row was absent or ownership could not be proven",
                        );
                        self.replace_run(origin, expected_run_revision, &run)
                            .await?;
                        requires_user_action = Some(UserAction {
                            code: UserActionCode::DeliveryUncertain,
                            safe_message: "Verify task queue and history before another start"
                                .to_owned(),
                        });
                        AutomationRunOutcome::StartUncertain
                    }
                    Err(error) => return Err(app_control_error(error)),
                };
                AutomationRunResult {
                    outcome,
                    run: run.view,
                    shared_queue: None,
                    authority: automation.view.authority,
                    warnings: self.backend_warnings(),
                    requires_user_action,
                }
            }
        };
        encode_result(result)
    }

    async fn automation_doctor(
        &self,
        origin: &OriginContext,
        _arguments: AutomationDoctorArgs,
    ) -> std::result::Result<AutomationDoctorResult, ControlError> {
        let stats = self.store.stats().await.map_err(storage_error)?;
        let authority = self.authority(origin, Utc::now()).await?;
        let (app_server, app_check, backend_ready, mut recommended_actions) =
            if self.config.app_server.is_none() {
                (
                    AppServerDoctorSummary {
                        connected: false,
                        endpoint_kind: endpoint_kind(&self.config),
                        user_agent: None,
                        schema_digest: None,
                        tested_build: false,
                        strict_queue_capability: StrictQueueCapability::Unavailable,
                    },
                    DoctorCheck {
                        code: "app_server_strict_queue".to_owned(),
                        healthy: false,
                        safe_message: "app-server is not configured".to_owned(),
                    },
                    false,
                    vec!["configure_and_verify_app_server".to_owned()],
                )
            } else {
                match self.client_config(&origin.thread_id) {
                    Ok(client_config) => match AppServerClient::connect(client_config).await {
                        Ok(client) => {
                            let identity = client.identity().clone();
                            let (tested_build, capability, ready, message, actions) =
                                match client.backend_status() {
                                    BackendStatus::Verified { .. } => (
                                        true,
                                        StrictQueueCapability::Available,
                                        true,
                                        "app-server strict queue probe passed for a verified build",
                                        Vec::new(),
                                    ),
                                    BackendStatus::Unverified => (
                                        false,
                                        StrictQueueCapability::Available,
                                        false,
                                        "queue methods exist but this exact build is not verified",
                                        vec!["verify_app_server_build".to_owned()],
                                    ),
                                    BackendStatus::Unsupported => (
                                        false,
                                        StrictQueueCapability::Unavailable,
                                        false,
                                        "app-server does not expose the required strict queue API",
                                        vec!["upgrade_app_server".to_owned()],
                                    ),
                                };
                            (
                                AppServerDoctorSummary {
                                    connected: true,
                                    endpoint_kind: endpoint_kind(&self.config),
                                    user_agent: Some(identity.user_agent),
                                    schema_digest: None,
                                    tested_build,
                                    strict_queue_capability: capability,
                                },
                                DoctorCheck {
                                    code: "app_server_strict_queue".to_owned(),
                                    healthy: ready,
                                    safe_message: message.to_owned(),
                                },
                                ready,
                                actions,
                            )
                        }
                        Err(error) => (
                            AppServerDoctorSummary {
                                connected: false,
                                endpoint_kind: endpoint_kind(&self.config),
                                user_agent: None,
                                schema_digest: None,
                                tested_build: false,
                                strict_queue_capability: capability_for_error(error.kind()),
                            },
                            DoctorCheck {
                                code: "app_server_strict_queue".to_owned(),
                                healthy: false,
                                safe_message: safe_probe_failure(error.kind()).to_owned(),
                            },
                            false,
                            vec!["check_app_server_connection".to_owned()],
                        ),
                    },
                    Err(_) => (
                        AppServerDoctorSummary {
                            connected: false,
                            endpoint_kind: endpoint_kind(&self.config),
                            user_agent: None,
                            schema_digest: None,
                            tested_build: false,
                            strict_queue_capability: StrictQueueCapability::Unknown,
                        },
                        DoctorCheck {
                            code: "app_server_strict_queue".to_owned(),
                            healthy: false,
                            safe_message: "app-server configuration could not be validated"
                                .to_owned(),
                        },
                        false,
                        vec!["fix_app_server_configuration".to_owned()],
                    ),
                }
            };
        if !self.config.delivery.enabled {
            recommended_actions.push("enable_delivery_after_review".to_owned());
        }
        let core_healthy = stats.healthy && self.clock_trusted();
        let delivery_healthy = backend_ready && self.config.delivery.enabled;
        Ok(AutomationDoctorResult {
            overall: if !core_healthy {
                DoctorOverall::Blocked
            } else if delivery_healthy {
                DoctorOverall::Healthy
            } else {
                DoctorOverall::Degraded
            },
            daemon: DaemonDoctorSummary {
                version: VERSION.to_owned(),
                control_protocol_version: u32::from(CONTROL_PROTOCOL_VERSION),
                uptime_seconds: self.started.elapsed().as_secs(),
                os_lock_held: true,
                control_socket_secure: socket_is_secure(&self.config.control_socket_path()),
            },
            database: DatabaseDoctorSummary {
                healthy: stats.healthy,
                schema_version: stats.schema_version,
                size_bytes: stats.database_bytes,
                wal_bytes: stats.wal_bytes,
                free_bytes: 0,
                effective_retention_days: None,
            },
            clock: ClockDoctorSummary {
                state: if self.clock_trusted() {
                    ClockState::Trusted
                } else {
                    ClockState::Quarantined
                },
                source: Some("wall_clock_checked_against_monotonic".to_owned()),
                warning: (!self.clock_trusted())
                    .then(|| "wall clock jump detected; scheduling is quarantined".to_owned()),
            },
            app_server,
            authority,
            scheduler: SchedulerDoctorSummary {
                active_automations: stats.active_automations,
                oldest_dispatch_lag_ms: None,
                blocked_runs: stats.blocked_runs,
                uncertain_runs: stats.uncertain_runs,
                ownership_violations: stats.ownership_violations,
            },
            checks: vec![
                DoctorCheck {
                    code: "database_integrity".to_owned(),
                    healthy: stats.healthy,
                    safe_message: if stats.healthy {
                        "SQLite quick check passed".to_owned()
                    } else {
                        "SQLite quick check failed".to_owned()
                    },
                },
                DoctorCheck {
                    code: "clock_trust".to_owned(),
                    healthy: self.clock_trusted(),
                    safe_message: if self.clock_trusted() {
                        "wall clock agrees with monotonic elapsed time".to_owned()
                    } else {
                        "wall clock is quarantined after a detected jump".to_owned()
                    },
                },
                app_check,
                DoctorCheck {
                    code: "delivery_enabled".to_owned(),
                    healthy: self.config.delivery.enabled,
                    safe_message: if self.config.delivery.enabled {
                        "delivery is enabled".to_owned()
                    } else {
                        "delivery is disabled".to_owned()
                    },
                },
            ],
            recommended_actions,
        })
    }

    async fn admin_authorize(
        &self,
        origin: &OriginContext,
        arguments: AdminAuthorizeArgs,
    ) -> std::result::Result<Value, ControlError> {
        let max = self.config.consent.grant_ttl_seconds.max(60);
        if arguments.ttl_seconds == 0 || arguments.ttl_seconds > max {
            return Err(api_error(
                "INVALID_GRANT_TTL",
                "authorization TTL is outside the configured bound",
                false,
            ));
        }
        let expires_at = Utc::now()
            .checked_add_signed(Duration::seconds(
                i64::try_from(arguments.ttl_seconds).map_err(|_| {
                    api_error("INVALID_GRANT_TTL", "authorization TTL is too large", false)
                })?,
            ))
            .ok_or_else(|| api_error("INVALID_GRANT_TTL", "authorization TTL overflow", false))?;
        self.store
            .authorize_task(origin.thread_id.clone(), expires_at)
            .await
            .map_err(storage_error)?;
        Ok(json!({
            "authorized": true,
            "threadId": origin.thread_id,
            "expiresAt": expires_at,
        }))
    }

    async fn scheduler_tick(&mut self) -> Result<()> {
        let now = Utc::now();
        let due = self.store.due_automations(now, SCHEDULER_BATCH).await?;
        for mut automation in due {
            let Some(cursor) = automation.view.next_nominal_due_at else {
                continue;
            };
            let after = cursor
                .checked_sub_signed(Duration::nanoseconds(1))
                .context("schedule cursor underflow")?;
            let plan = compute_catch_up(
                &automation.view.schedule,
                after,
                now,
                &automation.view.catch_up,
            )?;
            let selected = plan.runs.first().cloned();
            let next_cursor = match automation.view.catch_up.mode {
                crate::model::CatchUpMode::All => selected
                    .as_ref()
                    .map_or_else(
                        || next_occurrence(&automation.view.schedule, now),
                        |item| next_occurrence(&automation.view.schedule, item.nominal_due_at),
                    )?
                    .map(|item| item.nominal_due_at),
                crate::model::CatchUpMode::Skip | crate::model::CatchUpMode::Coalesce => {
                    next_occurrence(&automation.view.schedule, now)?.map(|item| item.nominal_due_at)
                }
            };

            if let Some(occurrence) = selected {
                let logical_id = logical_occurrence_id(&automation.view.id, &occurrence.identity)
                    .map_err(|error| anyhow::anyhow!(error.message))?;
                let mut run = make_run(&automation, occurrence.nominal_due_at, logical_id, None)
                    .map_err(|error| anyhow::anyhow!(error.message))?;
                apply_occurrence_metadata(&mut run.view, &occurrence);
                run.view.coalesced = plan.coalesced.clone();
                let _ = self.materialize_and_deliver(&automation, run).await?;
            }

            automation.view.next_nominal_due_at = next_cursor;
            automation.view.updated_at = now;
            let _ = self
                .store
                .replace_automation(
                    automation.view.target.thread_id.clone(),
                    automation.view.revision,
                    automation,
                )
                .await?;
        }
        Ok(())
    }

    /// Insert a durable run once, recover a prior partial attempt by its
    /// deterministic run ID, and return the post-delivery persisted record.
    async fn materialize_and_deliver(
        &mut self,
        automation: &StoredAutomation,
        candidate: StoredRun,
    ) -> Result<StoredRun> {
        let thread_id = automation.view.target.thread_id.clone();
        let run_id = candidate.view.id.clone();
        let mut run = if self.store.create_run(candidate.clone()).await? {
            candidate
        } else {
            self.store
                .get_run(thread_id.clone(), run_id.clone())
                .await?
                .context("deterministic run exists but could not be read")?
        };

        match run.view.state {
            RunState::Due => self.deliver(automation, run).await?,
            RunState::Delivering => {
                let expected = run.view.revision;
                run.view.revision = run.view.revision.saturating_add(1);
                run.view.state = RunState::DeliveryUncertain;
                run.view.bounded_error = Some(BoundedError {
                    code: "RECOVERY_DELIVERY_UNCERTAIN".to_owned(),
                    safe_message:
                        "daemon restarted during remote delivery; automatic replay stopped"
                            .to_owned(),
                    retryable: false,
                });
                let _ = self
                    .store
                    .replace_run(thread_id.clone(), expected, run)
                    .await?;
            }
            _ => {}
        }

        self.store
            .get_run(thread_id, run_id)
            .await?
            .context("persisted run disappeared after delivery")
    }

    async fn deliver(&mut self, automation: &StoredAutomation, mut run: StoredRun) -> Result<()> {
        let thread_id = automation.view.target.thread_id.clone();
        let expected = run.view.revision;
        if !self.config.delivery.enabled || self.config.app_server.is_none() {
            run.view.revision = run.view.revision.saturating_add(1);
            run.view.state = RunState::DeliveryFailed;
            run.view.completed_at = Some(Utc::now());
            run.view.bounded_error = Some(BoundedError {
                code: "DELIVERY_DISABLED".to_owned(),
                safe_message: "delivery is disabled or app-server is not configured".to_owned(),
                retryable: true,
            });
            let _ = self.store.replace_run(thread_id, expected, run).await?;
            return Ok(());
        }

        let input = text_queue_input(&automation.prompt)?;
        let client_config = self.client_config(&thread_id)?;
        let mut client = match AppServerClient::connect(client_config).await {
            Ok(client) => client,
            Err(error) => {
                self.finish_delivery_error(thread_id, run, error).await?;
                return Ok(());
            }
        };
        let ownership = QueueOwnership::pending(
            thread_id.clone(),
            run.client_user_message_id.clone(),
            &input,
            client.limits(),
        )?;
        if ownership.input_digest.to_hex() != run.input_digest {
            bail!("persisted input digest does not match configured prompt");
        }

        run.view.revision = run.view.revision.saturating_add(1);
        run.view.state = RunState::Delivering;
        let delivering_revision = run.view.revision;
        if !self
            .store
            .replace_run(thread_id.clone(), expected, run.clone())
            .await?
        {
            return Ok(());
        }
        match client.add_exact(ownership, input).await {
            Ok(QueueAddOutcome::Added(owned) | QueueAddOutcome::AlreadyPresent(owned)) => {
                run.view.revision = run.view.revision.saturating_add(1);
                run.view.state = RunState::ThreadQueued;
                run.view.enqueued_at = Some(Utc::now());
                run.remote_submission_id = owned.queued_submission_id;
                let _ = self
                    .store
                    .replace_run(thread_id, delivering_revision, run)
                    .await?;
            }
            Err(error) => self.finish_delivery_error(thread_id, run, error).await?,
        }
        Ok(())
    }

    async fn finish_delivery_error(
        &self,
        thread_id: String,
        mut run: StoredRun,
        error: AppServerError,
    ) -> Result<()> {
        let expected = run.view.revision;
        run.view.revision = run.view.revision.saturating_add(1);
        run.view.state = if error.write_disposition() == WriteDisposition::MayHaveCommitted {
            RunState::DeliveryUncertain
        } else {
            RunState::DeliveryFailed
        };
        run.view.completed_at = (run.view.state == RunState::DeliveryFailed).then(Utc::now);
        run.view.bounded_error = Some(BoundedError {
            code: app_error_code(error.kind()).to_owned(),
            safe_message: error.to_string(),
            retryable: error.write_disposition() == WriteDisposition::NotStarted,
        });
        let _ = self.store.replace_run(thread_id, expected, run).await?;
        Ok(())
    }

    fn client_config(&self, probe_thread: &str) -> Result<ClientConfig> {
        let configured = self
            .config
            .app_server
            .as_ref()
            .context("app-server is not configured")?;
        let mut client = adapter_config_for_thread(configured, probe_thread)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        client.limits.max_queue_items = self.config.scheduler.max_remote_queue_depth;
        Ok(client)
    }

    async fn get_owned(
        &self,
        origin: &OriginContext,
        id: &str,
    ) -> std::result::Result<StoredAutomation, ControlError> {
        self.store
            .get_automation(origin.thread_id.clone(), id.to_owned())
            .await
            .map_err(storage_error)?
            .ok_or_else(|| api_error("AUTOMATION_NOT_FOUND", "automation was not found", false))
    }

    async fn get_owned_run(
        &self,
        origin: &OriginContext,
        id: &str,
    ) -> std::result::Result<StoredRun, ControlError> {
        self.store
            .get_run(origin.thread_id.clone(), id.to_owned())
            .await
            .map_err(storage_error)?
            .ok_or_else(|| api_error("RUN_NOT_FOUND", "run was not found", false))
    }

    async fn replace_owned(
        &self,
        origin: &OriginContext,
        expected: u64,
        record: &StoredAutomation,
    ) -> std::result::Result<(), ControlError> {
        let changed = self
            .store
            .replace_automation(origin.thread_id.clone(), expected, record.clone())
            .await
            .map_err(storage_error)?;
        if !changed {
            return Err(revision_conflict());
        }
        Ok(())
    }

    async fn replace_run(
        &self,
        origin: &OriginContext,
        expected: u64,
        record: &StoredRun,
    ) -> std::result::Result<(), ControlError> {
        let changed = self
            .store
            .replace_run(origin.thread_id.clone(), expected, record.clone())
            .await
            .map_err(storage_error)?;
        if !changed {
            return Err(revision_conflict());
        }
        Ok(())
    }

    async fn require_authorized(
        &self,
        origin: &OriginContext,
        now: DateTime<Utc>,
    ) -> std::result::Result<(), ControlError> {
        if self.config.consent.mode == ConsentMode::TrustedAgentCurrentTask
            && origin.call_id.is_some()
        {
            return Ok(());
        }
        let consumed = self
            .store
            .consume_task_authorization(origin.thread_id.clone(), now)
            .await
            .map_err(storage_error)?;
        if !consumed {
            Err(api_error(
                "CONSENT_REQUIRED",
                "active automation work requires a current task grant",
                false,
            ))
        } else {
            Ok(())
        }
    }

    fn require_delivery_configured(&self) -> std::result::Result<(), ControlError> {
        if self.config.delivery.enabled && self.config.app_server.is_some() {
            Ok(())
        } else {
            Err(api_error(
                "DELIVERY_NOT_READY",
                "active automation work requires enabled app-server delivery",
                false,
            ))
        }
    }

    async fn is_authorized(
        &self,
        origin: &OriginContext,
        now: DateTime<Utc>,
    ) -> std::result::Result<bool, ControlError> {
        if self.config.consent.mode == ConsentMode::TrustedAgentCurrentTask
            && origin.call_id.is_some()
        {
            return Ok(true);
        }
        self.store
            .task_authorized(origin.thread_id.clone(), now)
            .await
            .map_err(storage_error)
    }

    async fn authority(
        &self,
        origin: &OriginContext,
        now: DateTime<Utc>,
    ) -> std::result::Result<AuthoritySummary, ControlError> {
        let authorized = self.is_authorized(origin, now).await?;
        Ok(AuthoritySummary {
            fingerprint: digest_parts(&["task-authority-v1", &origin.thread_id]),
            state: if authorized {
                AuthorityState::Matched
            } else {
                AuthorityState::NotAuthorized
            },
            auto_review: self.config.consent.mode == ConsentMode::TrustedAgentCurrentTask,
            reauthorization_required: !authorized,
        })
    }

    fn backend_summary(&self) -> BackendSummary {
        BackendSummary {
            mode: if self.config.app_server.is_some() {
                BackendMode::StrictQueue
            } else {
                BackendMode::Unsupported
            },
            tested_build: false,
            experimental: true,
            warning: Some(
                "strict queue mutation requires a semantically verified app-server build"
                    .to_owned(),
            ),
        }
    }

    fn backend_warnings(&self) -> Vec<Warning> {
        (!self.config.delivery.enabled || self.config.app_server.is_none())
            .then(|| Warning {
                code: "DELIVERY_NOT_READY".to_owned(),
                safe_message: "delivery is disabled or app-server is not configured".to_owned(),
            })
            .into_iter()
            .collect()
    }

    fn observe_clock(&mut self) {
        self.clock.observe();
    }

    fn clock_trusted(&self) -> bool {
        self.clock.trusted()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminAuthorizeArgs {
    ttl_seconds: u64,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdempotencyEnvelope {
    version: u8,
    argument_digest: String,
    started_at: DateTime<Utc>,
    owner_nonce: String,
    #[serde(flatten)]
    outcome: IdempotencyOutcome,
}

#[derive(serde::Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum IdempotencyOutcome {
    Pending,
    Succeeded { result: Value },
    Failed { error: ControlError },
}

struct IdempotencyToken {
    scope: String,
    argument_digest: String,
    started_at: DateTime<Utc>,
    owner_nonce: String,
    pending_value: Value,
    cached: Option<Value>,
}

struct ClockMonitor {
    wall: DateTime<Utc>,
    monotonic: Instant,
    threshold: StdDuration,
    quarantined_until: Option<Instant>,
}

impl ClockMonitor {
    fn new(threshold_seconds: u64) -> Self {
        Self {
            wall: Utc::now(),
            monotonic: Instant::now(),
            threshold: StdDuration::from_secs(threshold_seconds.max(1)),
            quarantined_until: None,
        }
    }

    fn observe(&mut self) {
        let now_wall = Utc::now();
        let now_mono = Instant::now();
        let elapsed = now_mono.saturating_duration_since(self.monotonic);
        let expected = self.wall + Duration::from_std(elapsed).unwrap_or(Duration::MAX);
        let drift = now_wall.signed_duration_since(expected).abs();
        if drift > Duration::from_std(self.threshold).unwrap_or(Duration::MAX) {
            self.quarantined_until = now_mono.checked_add(self.threshold);
        }
        self.wall = now_wall;
        self.monotonic = now_mono;
        if self
            .quarantined_until
            .is_some_and(|deadline| now_mono >= deadline)
        {
            self.quarantined_until = None;
        }
    }

    fn trusted(&self) -> bool {
        self.quarantined_until
            .is_none_or(|deadline| Instant::now() >= deadline)
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn read_one_request(
    stream: &mut UnixStream,
) -> std::result::Result<ControlRequest, ControlError> {
    let mut bytes = Vec::new();
    stream
        .take((MAX_CONTROL_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| {
            api_error(
                "CONTROL_READ_FAILED",
                "failed to read control request",
                true,
            )
        })?;
    if bytes.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(api_error(
            "CONTROL_FRAME_TOO_LARGE",
            "control request exceeded its bound",
            false,
        ));
    }
    if bytes.last() != Some(&b'\n') || bytes[..bytes.len().saturating_sub(1)].contains(&b'\n') {
        return Err(api_error(
            "INVALID_CONTROL_FRAME",
            "control connection must contain exactly one newline-terminated request",
            false,
        ));
    }
    bytes.pop();
    serde_json::from_slice(&bytes)
        .map_err(|_| api_error("INVALID_CONTROL_JSON", "control request is invalid", false))
}

fn update_request_key(arguments: &AutomationUpdateArgs) -> &str {
    match arguments {
        AutomationUpdateArgs::SuggestedCreate { request_key, .. }
        | AutomationUpdateArgs::Create { request_key, .. }
        | AutomationUpdateArgs::Update { request_key, .. }
        | AutomationUpdateArgs::Pause { request_key, .. }
        | AutomationUpdateArgs::Resume { request_key, .. }
        | AutomationUpdateArgs::SoftDelete { request_key, .. } => request_key,
    }
}

fn run_request_key(arguments: &AutomationRunArgs) -> &str {
    match arguments {
        AutomationRunArgs::RunNow { request_key, .. }
        | AutomationRunArgs::Cancel { request_key, .. }
        | AutomationRunArgs::Retry { request_key, .. }
        | AutomationRunArgs::StartBlockedRun { request_key, .. } => request_key,
    }
}

fn ownership_for_run(
    automation: &StoredAutomation,
    run: &StoredRun,
    limits: &AppServerLimits,
) -> std::result::Result<QueueOwnership, ControlError> {
    let input = text_queue_input(&automation.prompt).map_err(app_control_error)?;
    let mut ownership = QueueOwnership::pending(
        automation.view.target.thread_id.clone(),
        run.client_user_message_id.clone(),
        &input,
        limits,
    )
    .map_err(app_control_error)?;
    if ownership.input_digest.to_hex() != run.input_digest {
        return Err(api_error(
            "OWNERSHIP_EVIDENCE_UNAVAILABLE",
            "current automation prompt no longer reconstructs the persisted queue digest",
            false,
        ));
    }
    ownership.queued_submission_id = Some(run.remote_submission_id.clone().ok_or_else(|| {
        api_error(
            "OWNERSHIP_EVIDENCE_UNAVAILABLE",
            "run has no exact remote queue submission ID",
            false,
        )
    })?);
    Ok(ownership)
}

fn mark_remote_uncertain(run: &mut StoredRun, code: &str, message: &str) {
    run.view.revision = run.view.revision.saturating_add(1);
    run.view.state = RunState::DeliveryUncertain;
    run.view.phase = Some("remote_mutation_uncertain".to_owned());
    run.view.bounded_error = Some(BoundedError {
        code: code.to_owned(),
        safe_message: message.to_owned(),
        retryable: false,
    });
}

fn mark_ownership_violation(run: &mut StoredRun) {
    run.view.revision = run.view.revision.saturating_add(1);
    run.view.state = RunState::RemoteOwnershipViolation;
    run.view.completed_at = Some(Utc::now());
    run.view.phase = Some("remote_ownership_violation".to_owned());
    run.view.bounded_error = Some(BoundedError {
        code: "REMOTE_OWNERSHIP_VIOLATION".to_owned(),
        safe_message: "remote queue ownership could not be proven".to_owned(),
        retryable: false,
    });
}

fn make_run(
    automation: &StoredAutomation,
    nominal_due_at: DateTime<Utc>,
    logical_occurrence_id: String,
    retry_of_run_id: Option<String>,
) -> std::result::Result<StoredRun, ControlError> {
    let input = text_queue_input(&automation.prompt).map_err(app_control_error)?;
    let limits = AppServerLimits::default();
    let client_user_message_id = Uuid::new_v4().to_string();
    let ownership = QueueOwnership::pending(
        automation.view.target.thread_id.clone(),
        client_user_message_id.clone(),
        &input,
        &limits,
    )
    .map_err(app_control_error)?;
    let id = format!(
        "run_{}",
        &digest_parts(&["run-v1", &logical_occurrence_id])[..32]
    );
    Ok(StoredRun {
        view: RunView {
            id,
            revision: 1,
            automation_id: automation.view.id.clone(),
            automation_revision: automation.view.revision,
            state: RunState::Due,
            phase: Some("local_due".to_owned()),
            logical_occurrence_id,
            nominal_due_at,
            intended_local_time: None,
            stable_slot_id: None,
            resolution_ordinal: None,
            timezone: None,
            resolved_offset: None,
            enqueued_at: None,
            started_at: None,
            completed_at: None,
            dispatch_lag_ms: Some(
                Utc::now()
                    .signed_duration_since(nominal_due_at)
                    .num_milliseconds(),
            ),
            start_lag_ms: None,
            turn_id: None,
            coalesced: None,
            retry_of_run_id,
            bounded_error: None,
            attempts: None,
        },
        client_user_message_id,
        input_digest: ownership.input_digest.to_hex(),
        remote_submission_id: None,
    })
}

fn apply_occurrence_metadata(view: &mut RunView, occurrence: &NominalOccurrence) {
    view.intended_local_time = occurrence.intended_local_time;
    view.stable_slot_id = occurrence.stable_slot_id.clone();
    view.resolution_ordinal = Some(occurrence.resolution_ordinal);
    view.timezone = occurrence.timezone.clone();
    view.resolved_offset = occurrence.resolved_offset.clone();
}

fn initial_due(
    schedule: &crate::model::NormalizedSchedule,
    now: DateTime<Utc>,
) -> std::result::Result<Option<DateTime<Utc>>, ControlError> {
    if let crate::model::NormalizedSchedule::Once { at } = schedule {
        return Ok(Some(*at));
    }
    next_occurrence(schedule, now)
        .map(|occurrence| occurrence.map(|item| item.nominal_due_at))
        .map_err(schedule_error)
}

fn upcoming_previews(
    automation_id: &str,
    schedule: &crate::model::NormalizedSchedule,
    after: DateTime<Utc>,
    count: usize,
) -> std::result::Result<Vec<OccurrencePreview>, ControlError> {
    let mut cursor = after;
    let mut previews = Vec::new();
    for _ in 0..count {
        let Some(occurrence) = next_occurrence(schedule, cursor).map_err(schedule_error)? else {
            break;
        };
        cursor = occurrence.nominal_due_at;
        previews.push(OccurrencePreview {
            logical_occurrence_id: logical_occurrence_id(automation_id, &occurrence.identity)?,
            stable_slot_id: occurrence.stable_slot_id,
            intended_local_time: occurrence.intended_local_time,
            resolved_at: occurrence.nominal_due_at,
            offset: occurrence.resolved_offset,
            resolution_ordinal: Some(occurrence.resolution_ordinal),
            dst_resolution: occurrence.dst_resolution,
        });
    }
    Ok(previews)
}

fn logical_occurrence_id(
    automation_id: &str,
    identity: &OccurrenceIdentitySeed,
) -> std::result::Result<String, ControlError> {
    let encoded = serde_json::to_vec(identity).map_err(internal_json_error)?;
    let mut hasher = Sha256::new();
    hasher.update(b"threadbeat-logical-occurrence-v1\0");
    hasher.update(automation_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(encoded);
    Ok(format!("occ_{}", hex::encode(hasher.finalize())))
}

fn prompt_summary(prompt: &str) -> PromptSummary {
    PromptSummary {
        sha256: hex::encode(Sha256::digest(prompt.as_bytes())),
        byte_length: prompt.len() as u64,
        redacted_preview: format!("[stored automation prompt: {} bytes]", prompt.len()),
    }
}

fn validate_name_prompt(
    name: &str,
    prompt: &str,
    max_prompt: usize,
) -> std::result::Result<(), ControlError> {
    validate_name(name)?;
    validate_prompt(prompt, max_prompt)
}

fn validate_name(name: &str) -> std::result::Result<(), ControlError> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(api_error("INVALID_NAME", "name is invalid", false));
    }
    Ok(())
}

fn validate_prompt(prompt: &str, max_prompt: usize) -> std::result::Result<(), ControlError> {
    if prompt.trim().is_empty() || prompt.len() > max_prompt {
        return Err(api_error(
            "INVALID_PROMPT",
            "prompt is empty or too large",
            false,
        ));
    }
    Ok(())
}

fn validate_request_key(key: &str) -> std::result::Result<(), ControlError> {
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(api_error(
            "INVALID_REQUEST_KEY",
            "requestKey is invalid",
            false,
        ));
    }
    Ok(())
}

fn validate_origin(origin: &OriginContext) -> std::result::Result<(), ControlError> {
    if origin.thread_id.is_empty()
        || origin.thread_id.len() > 256
        || origin.thread_id.chars().any(char::is_control)
    {
        return Err(api_error(
            "INVALID_ORIGINATING_TASK",
            "originating task is invalid",
            false,
        ));
    }
    Ok(())
}

fn require_revision(actual: u64, expected: u64) -> std::result::Result<(), ControlError> {
    if actual != expected {
        return Err(revision_conflict());
    }
    Ok(())
}

fn revision_conflict() -> ControlError {
    api_error(
        "REVISION_CONFLICT",
        "record changed; read it again before retrying",
        false,
    )
}

fn decode_arguments<T: for<'de> Deserialize<'de>>(
    value: Value,
) -> std::result::Result<T, ControlError> {
    serde_json::from_value(value)
        .map_err(|_| api_error("INVALID_ARGUMENTS", "tool arguments are invalid", false))
}

fn encode_result<T: serde::Serialize>(value: T) -> std::result::Result<Value, ControlError> {
    serde_json::to_value(value).map_err(internal_json_error)
}

fn digest_json(value: &Value) -> std::result::Result<String, ControlError> {
    let encoded = serde_json::to_vec(value).map_err(internal_json_error)?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn digest_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}

fn automation_id_for_request(
    thread_id: &str,
    operation: &str,
    request_key: &str,
    argument_digest: &str,
) -> String {
    let digest = digest_parts(&[
        "automation-request-v2",
        thread_id,
        operation,
        request_key,
        argument_digest,
    ]);
    format!("aut_{}", &digest[..32])
}

fn endpoint_kind(config: &Config) -> EndpointKind {
    match config.app_server.as_ref().map(|value| &value.endpoint) {
        Some(crate::config::AppServerEndpoint::ProxyCommand { .. }) => EndpointKind::SshProxy,
        _ => EndpointKind::Unix,
    }
}

fn app_error_code(kind: AppServerErrorKind) -> &'static str {
    match kind {
        AppServerErrorKind::InvalidConfiguration => "APP_SERVER_INVALID_CONFIGURATION",
        AppServerErrorKind::ConnectionFailed => "APP_SERVER_CONNECTION_FAILED",
        AppServerErrorKind::Timeout => "APP_SERVER_TIMEOUT",
        AppServerErrorKind::MessageTooLarge => "APP_SERVER_MESSAGE_TOO_LARGE",
        AppServerErrorKind::ProtocolViolation => "APP_SERVER_PROTOCOL_VIOLATION",
        AppServerErrorKind::UnexpectedServerRequest => "APP_SERVER_REQUEST_PENDING",
        AppServerErrorKind::RemoteRejected => "APP_SERVER_REMOTE_REJECTED",
        AppServerErrorKind::ConnectionClosed => "APP_SERVER_CONNECTION_CLOSED",
        AppServerErrorKind::UnverifiedBuild => "APP_SERVER_UNVERIFIED_BUILD",
        AppServerErrorKind::UnsupportedBuild => "APP_SERVER_UNSUPPORTED_BUILD",
        AppServerErrorKind::OwnershipViolation => "REMOTE_OWNERSHIP_VIOLATION",
        AppServerErrorKind::SharedQueueBlocked => "SHARED_QUEUE_BLOCKED",
        AppServerErrorKind::ProxyFailed => "APP_SERVER_PROXY_FAILED",
    }
}

fn capability_for_error(kind: AppServerErrorKind) -> StrictQueueCapability {
    match kind {
        AppServerErrorKind::UnsupportedBuild => StrictQueueCapability::Unavailable,
        _ => StrictQueueCapability::Unknown,
    }
}

fn safe_probe_failure(kind: AppServerErrorKind) -> &'static str {
    match kind {
        AppServerErrorKind::InvalidConfiguration => "app-server configuration is invalid",
        AppServerErrorKind::ConnectionFailed
        | AppServerErrorKind::ConnectionClosed
        | AppServerErrorKind::ProxyFailed => "app-server connection could not be established",
        AppServerErrorKind::Timeout => "app-server probe timed out",
        AppServerErrorKind::MessageTooLarge | AppServerErrorKind::ProtocolViolation => {
            "app-server probe returned an incompatible bounded response"
        }
        AppServerErrorKind::UnexpectedServerRequest => {
            "app-server requires interactive handling during the probe"
        }
        AppServerErrorKind::RemoteRejected => "app-server rejected the read-only queue probe",
        AppServerErrorKind::UnverifiedBuild => "app-server build is not verified",
        AppServerErrorKind::UnsupportedBuild => "strict queue methods are unsupported",
        AppServerErrorKind::OwnershipViolation | AppServerErrorKind::SharedQueueBlocked => {
            "app-server queue probe could not be completed safely"
        }
    }
}

fn app_configuration_error() -> ControlError {
    api_error(
        "APP_SERVER_INVALID_CONFIGURATION",
        "app-server configuration is invalid",
        false,
    )
}

fn app_control_error(error: AppServerError) -> ControlError {
    api_error(app_error_code(error.kind()), &error.to_string(), false)
}

fn schedule_error(error: impl std::fmt::Display) -> ControlError {
    api_error("INVALID_SCHEDULE", &error.to_string(), false)
}

fn storage_error(error: anyhow::Error) -> ControlError {
    warn!(%error, "storage operation failed");
    api_error("STORAGE_ERROR", "durable storage operation failed", true)
}

fn internal_json_error(error: serde_json::Error) -> ControlError {
    warn!(%error, "internal JSON operation failed");
    api_error("INTERNAL_ERROR", "internal serialization failed", false)
}

fn api_error(code: &str, message: &str, retryable: bool) -> ControlError {
    ControlError {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable,
        details: None,
    }
}

fn secure_directory(path: &Path) -> Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("refusing insecure non-directory path {}", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o777 != 0o700 {
                bail!(
                    "refusing to repermission existing directory {}",
                    path.display()
                );
            }
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

fn secure_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(())
}

fn acquire_lock(path: &Path) -> Result<File> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("refusing insecure daemon lock path");
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    secure_file(path)?;
    file.try_lock_exclusive()
        .context("another Threadbeat daemon already owns the state directory")?;
    Ok(file)
}

fn prepare_socket_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
                    bail!("refusing to replace non-socket control path");
                }
            }
            fs::remove_file(path)
                .with_context(|| format!("failed to remove stale socket {}", path.display()))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to inspect control socket path"),
    }
    Ok(())
}

fn socket_is_secure(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        return fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.file_type().is_socket() && metadata.permissions().mode() & 0o777 == 0o600
        });
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NormalizedCatchUpPolicy, NormalizedSchedule};
    use tempfile::TempDir;

    async fn test_daemon() -> (TempDir, Daemon) {
        let directory = TempDir::new().expect("tempdir");
        let config = Config {
            state_dir: directory.path().join("state"),
            runtime_dir: directory.path().join("runtime"),
            ..Config::default()
        };
        let store = Store::open(config.database_path())
            .await
            .expect("open store");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(directory.path().join("test.lock"))
            .expect("open lock");
        (directory, Daemon::new(config, store, lock))
    }

    fn automation() -> StoredAutomation {
        let now = Utc::now();
        StoredAutomation {
            prompt: "Check the build".to_owned(),
            view: AutomationView {
                id: "automation-1".to_owned(),
                revision: 1,
                name: "Build check".to_owned(),
                prompt: prompt_summary("Check the build"),
                kind: AutomationKind::Heartbeat,
                target: AutomationTarget {
                    thread_id: "thread-1".to_owned(),
                    source: TargetSource::OriginatingTask,
                },
                schedule: NormalizedSchedule::Once { at: now },
                catch_up: NormalizedCatchUpPolicy::default(),
                state: AutomationState::Active,
                next_nominal_due_at: Some(now),
                pending_run_count: 0,
                authority: AuthoritySummary {
                    fingerprint: "test".to_owned(),
                    state: AuthorityState::Matched,
                    auto_review: false,
                    reauthorization_required: false,
                },
                created_at: now,
                updated_at: now,
                blocker: None,
            },
        }
    }

    #[test]
    fn request_scope_is_task_and_key_specific() {
        assert_ne!(
            digest_parts(&["request-v1", "task-a", "automation_update", "same"]),
            digest_parts(&["request-v1", "task-b", "automation_update", "same"])
        );
    }

    #[tokio::test]
    async fn existing_pending_request_fails_closed() {
        let (_directory, daemon) = test_daemon().await;
        let origin = OriginContext {
            thread_id: "thread-1".to_owned(),
            call_id: Some("call-1".to_owned()),
        };
        let arguments = json!({"action": "pause", "requestKey": "same"});
        let fresh = daemon
            .idempotency_begin(&origin, "automation_update", "same", &arguments)
            .await
            .expect("fresh reservation");
        assert!(fresh.cached.is_none());

        let error = daemon
            .idempotency_begin(&origin, "automation_update", "same", &arguments)
            .await
            .err()
            .expect("pending replay must fail closed");
        assert_eq!(error.code, "REQUEST_STATUS_UNCERTAIN");
        assert!(!error.retryable);

        let conflict = daemon
            .idempotency_begin(
                &origin,
                "automation_update",
                "same",
                &json!({"action": "resume", "requestKey": "same"}),
            )
            .await
            .err()
            .expect("changed arguments must conflict");
        assert_eq!(conflict.code, "REQUEST_KEY_CONFLICT");
    }

    #[tokio::test]
    async fn idempotency_caches_successes_and_failures() {
        let (_directory, daemon) = test_daemon().await;
        let origin = OriginContext {
            thread_id: "thread-1".to_owned(),
            call_id: Some("call-1".to_owned()),
        };

        let success_arguments = json!({"action": "pause", "requestKey": "success"});
        let success = daemon
            .idempotency_begin(&origin, "automation_update", "success", &success_arguments)
            .await
            .expect("success reservation");
        let expected = json!({"outcome": "paused"});
        assert_eq!(
            daemon
                .idempotency_finish(success, Ok(expected.clone()))
                .await
                .expect("complete success"),
            expected
        );
        let cached = daemon
            .idempotency_begin(&origin, "automation_update", "success", &success_arguments)
            .await
            .expect("cached success");
        assert_eq!(cached.cached, Some(expected));

        let failure_arguments = json!({"action": "pause", "requestKey": "failure"});
        let failure = daemon
            .idempotency_begin(&origin, "automation_update", "failure", &failure_arguments)
            .await
            .expect("failure reservation");
        let expected_error = api_error("AUTOMATION_NOT_FOUND", "not found", false);
        let first_error = daemon
            .idempotency_finish(failure, Err(expected_error.clone()))
            .await
            .expect_err("complete failure");
        assert_eq!(first_error.code, expected_error.code);
        let cached_error = daemon
            .idempotency_begin(&origin, "automation_update", "failure", &failure_arguments)
            .await
            .err()
            .expect("cached failure");
        assert_eq!(cached_error.code, expected_error.code);
    }

    #[test]
    fn automation_request_identity_includes_operation_and_arguments() {
        let baseline = automation_id_for_request("thread", "create", "key", "digest-a");
        assert_ne!(
            baseline,
            automation_id_for_request("thread", "suggested_create", "key", "digest-a")
        );
        assert_ne!(
            baseline,
            automation_id_for_request("thread", "create", "key", "digest-b")
        );
    }

    #[test]
    fn stable_occurrence_id_ignores_schedule_revision() {
        let identity = OccurrenceIdentitySeed::Interval {
            anchor: Utc::now(),
            ordinal: 42,
        };
        assert_eq!(
            logical_occurrence_id("automation", &identity).expect("identity"),
            logical_occurrence_id("automation", &identity).expect("identity")
        );
    }

    #[test]
    fn exact_remote_ownership_requires_persisted_submission_and_digest() {
        let automation = automation();
        let mut run = make_run(
            &automation,
            Utc::now(),
            "logical-occurrence".to_owned(),
            None,
        )
        .expect("run");
        let missing = ownership_for_run(&automation, &run, &AppServerLimits::default())
            .expect_err("remote ID is mandatory");
        assert_eq!(missing.code, "OWNERSHIP_EVIDENCE_UNAVAILABLE");

        run.remote_submission_id = Some("queued-submission-1".to_owned());
        assert!(ownership_for_run(&automation, &run, &AppServerLimits::default()).is_ok());
        run.input_digest = "changed".to_owned();
        let changed = ownership_for_run(&automation, &run, &AppServerLimits::default())
            .expect_err("digest mismatch must fail closed");
        assert_eq!(changed.code, "OWNERSHIP_EVIDENCE_UNAVAILABLE");
    }

    #[test]
    fn doctor_probe_failures_are_bounded_static_messages() {
        assert_eq!(
            safe_probe_failure(AppServerErrorKind::ConnectionFailed),
            "app-server connection could not be established"
        );
        assert_eq!(
            capability_for_error(AppServerErrorKind::UnsupportedBuild),
            StrictQueueCapability::Unavailable
        );
    }
}
