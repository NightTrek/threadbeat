use chrono::{DateTime, FixedOffset, NaiveDateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MIN_INTERVAL_MINUTES: u32 = 1;
pub const MAX_INTERVAL_MINUTES: u32 = 525_600;
pub const MAX_WEEKLY_GROUPS: usize = 32;
pub const MAX_WEEKLY_LOCAL_TIMES: usize = 24;
pub const MAX_CATCH_UP_RUNS: u16 = 100;
pub const DEFAULT_CATCH_UP_AGE_MINUTES: u32 = 1_440;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyGroup {
    pub weekdays: Vec<Weekday>,
    /// A local wall time in `HH:MM` or `HH:MM:SS` form.
    pub local_times: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DstGapPolicy {
    #[default]
    Skip,
    NextValid,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DstFoldPolicy {
    #[default]
    First,
    Second,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct DstPolicy {
    #[serde(default)]
    pub gap: DstGapPolicy,
    #[serde(default)]
    pub fold: DstFoldPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleSpec {
    Once {
        at: DateTime<FixedOffset>,
    },
    Interval {
        #[serde(rename = "everyMinutes")]
        every_minutes: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<DateTime<FixedOffset>>,
    },
    Weekly {
        timezone: String,
        groups: Vec<WeeklyGroup>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dst: Option<DstPolicy>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedWeeklySlot {
    pub stable_slot_id: String,
    pub weekday: Weekday,
    /// Canonical `HH:MM:SS` local wall time.
    pub local_time: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NormalizedSchedule {
    Once {
        at: DateTime<Utc>,
    },
    Interval {
        #[serde(rename = "everyMinutes")]
        every_minutes: u32,
        anchor: DateTime<Utc>,
    },
    Weekly {
        timezone: String,
        slots: Vec<NormalizedWeeklySlot>,
        dst: DstPolicy,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatchUpMode {
    Skip,
    Coalesce,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatchUpPolicy {
    pub mode: CatchUpMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedCatchUpPolicy {
    pub mode: CatchUpMode,
    pub max_age_minutes: u32,
    pub max_runs: u16,
}

impl Default for NormalizedCatchUpPolicy {
    fn default() -> Self {
        Self {
            mode: CatchUpMode::Coalesce,
            max_age_minutes: DEFAULT_CATCH_UP_AGE_MINUTES,
            max_runs: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DstResolution {
    Ordinary,
    GapSkipped,
    GapNextValid,
    FoldFirst,
    FoldSecond,
}

/// Schedule-derived identity fields. The store combines these fields with the
/// automation ID to create the immutable logical occurrence ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OccurrenceIdentitySeed {
    Once {
        declared_instant: DateTime<Utc>,
    },
    Interval {
        anchor: DateTime<Utc>,
        ordinal: u64,
    },
    Weekly {
        stable_slot_id: String,
        intended_local_time: NaiveDateTime,
        resolution_ordinal: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StableOccurrenceIdentityInputs {
    pub automation_id: String,
    pub seed: OccurrenceIdentitySeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NominalOccurrence {
    pub identity: OccurrenceIdentitySeed,
    pub nominal_due_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_local_time: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_offset: Option<String>,
    pub resolution_ordinal: u8,
    pub dst_resolution: DstResolution,
}

/// A display-safe upcoming occurrence returned by `automation_update` after
/// normalization. The daemon derives `logical_occurrence_id` by hashing the
/// automation ID together with [`OccurrenceIdentitySeed`]; callers never
/// supply it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OccurrencePreview {
    pub logical_occurrence_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_local_time: Option<NaiveDateTime>,
    pub resolved_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_ordinal: Option<u8>,
    pub dst_resolution: DstResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedOccurrence {
    pub identity: OccurrenceIdentitySeed,
    pub intended_local_time: NaiveDateTime,
    pub stable_slot_id: String,
    pub timezone: String,
    pub dst_resolution: DstResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct OccurrenceBatch {
    pub occurrences: Vec<NominalOccurrence>,
    pub skipped: Vec<SkippedOccurrence>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoalescedWindow {
    pub missed_count: u64,
    pub earliest: DateTime<Utc>,
    pub latest: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct CatchUpPlan {
    pub runs: Vec<NominalOccurrence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesced: Option<CoalescedWindow>,
    pub skipped_by_policy: u64,
    pub skipped_for_dst: Vec<SkippedOccurrence>,
    pub omitted_by_cap: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationState {
    Active,
    Paused,
    NeedsAttention,
    AuthorityChanged,
    ClockBlocked,
    StorageBlocked,
    Incompatible,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Scheduled,
    Due,
    Delivering,
    ThreadQueued,
    Running,
    WaitingApproval,
    WaitingUserInput,
    Succeeded,
    TurnFailed,
    DeliveryFailed,
    Interrupted,
    AuthorityChanged,
    RemoteOwnershipViolation,
    TaskClosed,
    Cancelled,
    Coalesced,
    DeliveryUncertain,
    Superseded,
    Expired,
    Skipped,
}

impl RunState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::TurnFailed
                | Self::DeliveryFailed
                | Self::Interrupted
                | Self::AuthorityChanged
                | Self::RemoteOwnershipViolation
                | Self::TaskClosed
                | Self::Cancelled
                | Self::Coalesced
                | Self::Superseded
                | Self::Expired
                | Self::Skipped
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationKind {
    Heartbeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    OriginatingTask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationTarget {
    pub thread_id: String,
    pub source: TargetSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptSummary {
    pub sha256: String,
    pub byte_length: u64,
    pub redacted_preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityState {
    Matched,
    Changed,
    Missing,
    NotAuthorized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthoritySummary {
    pub fingerprint: String,
    pub state: AuthorityState,
    pub auto_review: bool,
    pub reauthorization_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackendMode {
    StrictQueue,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendSummary {
    pub mode: BackendMode,
    pub tested_build: bool,
    pub experimental: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub code: String,
    pub safe_message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserActionCode {
    ConsentRequired,
    ApprovalRequired,
    UserInputRequired,
    Interrupted,
    AuthorityChanged,
    DeliveryUncertain,
    RemoteOwnershipViolation,
    StrictQueueUnavailable,
    ClockUntrusted,
    StorageUnhealthy,
    RevisionConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserAction {
    pub code: UserActionCode,
    pub safe_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationView {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub prompt: PromptSummary,
    pub kind: AutomationKind,
    pub target: AutomationTarget,
    pub schedule: NormalizedSchedule,
    pub catch_up: NormalizedCatchUpPolicy,
    pub state: AutomationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_nominal_due_at: Option<DateTime<Utc>>,
    pub pending_run_count: u64,
    pub authority: AuthoritySummary,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<UserAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationSummary {
    #[serde(flatten)]
    pub automation: AutomationView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_state: Option<RunState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoundedError {
    pub code: String,
    pub safe_message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DispatchAttempt {
    pub attempt: u32,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<BoundedError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: String,
    pub revision: u64,
    pub automation_id: String,
    pub automation_revision: u64,
    pub state: RunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub logical_occurrence_id: String,
    pub nominal_due_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_local_time: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_ordinal: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_offset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_lag_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_lag_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesced: Option<CoalescedWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_error: Option<BoundedError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<DispatchAttempt>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[schemars(extend("type" = "object"))]
pub enum AutomationUpdateArgs {
    SuggestedCreate {
        request_key: String,
        name: String,
        prompt: String,
        schedule: ScheduleSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catch_up: Option<CatchUpPolicy>,
    },
    Create {
        request_key: String,
        name: String,
        prompt: String,
        schedule: ScheduleSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catch_up: Option<CatchUpPolicy>,
    },
    Update {
        request_key: String,
        automation_id: String,
        expected_revision: u64,
        patch: AutomationUpdatePatch,
    },
    Pause {
        request_key: String,
        automation_id: String,
        expected_revision: u64,
    },
    Resume {
        request_key: String,
        automation_id: String,
        expected_revision: u64,
    },
    SoftDelete {
        request_key: String,
        automation_id: String,
        expected_revision: u64,
        #[serde(default)]
        cancel_pending: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationUpdatePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<CatchUpPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationUpdateOutcome {
    Suggested,
    Created,
    Updated,
    Paused,
    Resumed,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationUpdateResult {
    pub outcome: AutomationUpdateOutcome,
    pub automation: AutomationView,
    /// Bounded, daemon-computed occurrences from the normalized schedule.
    /// This keeps conversational before/after review in the mutation result
    /// without a separate preview tool or a caller-provided intent token.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_occurrences: Vec<OccurrencePreview>,
    pub authority: AuthoritySummary,
    pub backend: BackendSummary,
    pub warnings: Vec<Warning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_user_action: Option<UserAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[schemars(extend("type" = "object"))]
pub enum AutomationRunArgs {
    RunNow {
        request_key: String,
        automation_id: String,
        expected_revision: u64,
    },
    Cancel {
        request_key: String,
        run_id: String,
        expected_run_revision: u64,
    },
    Retry {
        request_key: String,
        run_id: String,
        expected_run_revision: u64,
    },
    StartBlockedRun {
        request_key: String,
        run_id: String,
        expected_run_revision: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunOutcome {
    RunCreated,
    Cancelled,
    TooLate,
    CancelUncertain,
    RetryCreated,
    BlockedRunStarted,
    StartUncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedQueueSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_item_is_head: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threadbeat_owned_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunResult {
    pub outcome: AutomationRunOutcome,
    pub run: RunView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_queue: Option<SharedQueueSummary>,
    pub authority: AuthoritySummary,
    pub warnings: Vec<Warning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_user_action: Option<UserAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGetArgs {
    pub automation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationGetResult {
    pub automation: AutomationView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_run: Option<RunView>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutomationListArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<AutomationState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    #[serde(default)]
    pub include_deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationListResult {
    pub automations: Vec<AutomationSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunsArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<RunState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub include_attempts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunsResult {
    pub runs: Vec<RunView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DoctorLevel {
    #[default]
    Quick,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDoctorArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub level: DoctorLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DoctorOverall {
    Healthy,
    Degraded,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClockState {
    Untrusted,
    Trusted,
    TrustedOverride,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    Unix,
    SshProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrictQueueCapability {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DaemonDoctorSummary {
    pub version: String,
    pub control_protocol_version: u32,
    pub uptime_seconds: u64,
    pub os_lock_held: bool,
    pub control_socket_secure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseDoctorSummary {
    pub healthy: bool,
    pub schema_version: u32,
    pub size_bytes: u64,
    pub wal_bytes: u64,
    pub free_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_retention_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClockDoctorSummary {
    pub state: ClockState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppServerDoctorSummary {
    pub connected: bool,
    pub endpoint_kind: EndpointKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_digest: Option<String>,
    pub tested_build: bool,
    pub strict_queue_capability: StrictQueueCapability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerDoctorSummary {
    pub active_automations: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_dispatch_lag_ms: Option<i64>,
    pub blocked_runs: u64,
    pub uncertain_runs: u64,
    pub ownership_violations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub code: String,
    pub healthy: bool,
    pub safe_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDoctorResult {
    pub overall: DoctorOverall,
    pub daemon: DaemonDoctorSummary,
    pub database: DatabaseDoctorSummary,
    pub clock: ClockDoctorSummary,
    pub app_server: AppServerDoctorSummary,
    pub authority: AuthoritySummary,
    pub scheduler: SchedulerDoctorSummary,
    pub checks: Vec<DoctorCheck>,
    /// Trusted identifiers from a daemon-owned action catalog, not remote text.
    pub recommended_actions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn mutation_arguments_use_agent_facing_camel_case_without_intent_tokens() {
        let value = serde_json::to_value(AutomationUpdateArgs::Pause {
            request_key: "req-1".to_string(),
            automation_id: "aut-1".to_string(),
            expected_revision: 7,
        })
        .expect("arguments should serialize");

        assert_eq!(
            value,
            json!({
                "operation": "pause",
                "requestKey": "req-1",
                "automationId": "aut-1",
                "expectedRevision": 7
            })
        );
        assert!(!value.to_string().contains("intent"));
    }

    #[test]
    fn delivery_uncertainty_remains_nonterminal_for_reconciliation() {
        assert!(!RunState::DeliveryUncertain.is_terminal());
        assert!(RunState::Succeeded.is_terminal());
        assert!(RunState::DeliveryFailed.is_terminal());
    }
}
