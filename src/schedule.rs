use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, LocalResult, NaiveDateTime, NaiveTime, Offset,
    TimeZone, Utc,
};
use chrono_tz::Tz;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::model::{
    CatchUpMode, CatchUpPlan, CatchUpPolicy, CoalescedWindow, DstFoldPolicy, DstGapPolicy,
    DstPolicy, DstResolution, MAX_CATCH_UP_RUNS, MAX_INTERVAL_MINUTES, MAX_WEEKLY_GROUPS,
    MAX_WEEKLY_LOCAL_TIMES, MIN_INTERVAL_MINUTES, NominalOccurrence, NormalizedCatchUpPolicy,
    NormalizedSchedule, NormalizedWeeklySlot, OccurrenceBatch, OccurrenceIdentitySeed,
    ScheduleSpec, SkippedOccurrence, Weekday,
};

const MAX_WINDOW_DAYS: i64 = 3_670;
const MAX_WINDOW_OCCURRENCES: usize = 100_000;
const WEEKLY_NEXT_SEARCH_DAYS: i64 = 15;
const CATCH_UP_CHUNK_DAYS: i64 = 14;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("interval must be between {MIN_INTERVAL_MINUTES} and {MAX_INTERVAL_MINUTES} minutes")]
    InvalidInterval,
    #[error("interval arithmetic overflowed the supported timestamp range")]
    IntervalOverflow,
    #[error("weekly schedule must contain between 1 and {MAX_WEEKLY_GROUPS} groups")]
    InvalidWeeklyGroupCount,
    #[error("weekly group {group} must contain at least one weekday")]
    EmptyWeekdays { group: usize },
    #[error("weekly group {group} must contain between 1 and {MAX_WEEKLY_LOCAL_TIMES} local times")]
    InvalidLocalTimeCount { group: usize },
    #[error("invalid local time '{value}'; expected HH:MM or HH:MM:SS")]
    InvalidLocalTime { value: String },
    #[error("unknown IANA timezone '{timezone}'")]
    InvalidTimezone { timezone: String },
    #[error("weekly schedule normalized to no slots")]
    EmptyWeeklySchedule,
    #[error("nonexistent local time {local} in {timezone}")]
    NonexistentLocalTime {
        timezone: String,
        local: NaiveDateTime,
    },
    #[error("could not find a valid local time after DST gap at {local} in {timezone}")]
    UnresolvableDstGap {
        timezone: String,
        local: NaiveDateTime,
    },
    #[error("catch-up maxAgeMinutes must be between 1 and {MAX_INTERVAL_MINUTES}")]
    InvalidCatchUpAge,
    #[error("catch-up mode 'all' requires maxRuns between 1 and {MAX_CATCH_UP_RUNS}")]
    InvalidCatchUpRuns,
    #[error("catch-up mode 'coalesce' always has maxRuns=1")]
    InvalidCoalesceRuns,
    #[error("occurrence window ends before it starts")]
    ReversedWindow,
    #[error("occurrence window exceeds {MAX_WINDOW_DAYS} days")]
    WindowTooLarge,
    #[error("normalized weekly slot contains an invalid local time")]
    InvalidNormalizedSlot,
    #[error("schedule produced too many occurrences in a bounded catch-up chunk")]
    CatchUpDensityExceeded,
}

/// Normalize user input into the immutable representation stored on a schedule
/// revision. Missing interval anchors become the activation instant.
pub fn normalize_schedule(
    input: &ScheduleSpec,
    activation_time: DateTime<Utc>,
) -> Result<NormalizedSchedule, ScheduleError> {
    match input {
        ScheduleSpec::Once { at } => Ok(NormalizedSchedule::Once {
            at: at.with_timezone(&Utc),
        }),
        ScheduleSpec::Interval {
            every_minutes,
            anchor,
        } => {
            validate_interval(*every_minutes)?;
            Ok(NormalizedSchedule::Interval {
                every_minutes: *every_minutes,
                anchor: anchor
                    .as_ref()
                    .map_or(activation_time, |value| value.with_timezone(&Utc)),
            })
        }
        ScheduleSpec::Weekly {
            timezone,
            groups,
            dst,
        } => {
            let tz = parse_timezone(timezone)?;
            if !(1..=MAX_WEEKLY_GROUPS).contains(&groups.len()) {
                return Err(ScheduleError::InvalidWeeklyGroupCount);
            }

            let mut canonical_slots = BTreeSet::new();
            for (group_index, group) in groups.iter().enumerate() {
                if group.weekdays.is_empty() {
                    return Err(ScheduleError::EmptyWeekdays { group: group_index });
                }
                if !(1..=MAX_WEEKLY_LOCAL_TIMES).contains(&group.local_times.len()) {
                    return Err(ScheduleError::InvalidLocalTimeCount { group: group_index });
                }

                let weekdays = group.weekdays.iter().copied().collect::<BTreeSet<_>>();
                let times = group
                    .local_times
                    .iter()
                    .map(|value| parse_local_time(value).map(canonical_time))
                    .collect::<Result<BTreeSet<_>, _>>()?;

                for weekday in weekdays {
                    for local_time in &times {
                        canonical_slots.insert((
                            weekday_sort_key(weekday),
                            weekday,
                            local_time.clone(),
                        ));
                    }
                }
            }

            if canonical_slots.is_empty() {
                return Err(ScheduleError::EmptyWeeklySchedule);
            }

            let slots = canonical_slots
                .into_iter()
                .map(|(_, weekday, local_time)| NormalizedWeeklySlot {
                    stable_slot_id: stable_slot_id(tz, weekday, &local_time),
                    weekday,
                    local_time,
                })
                .collect();

            Ok(NormalizedSchedule::Weekly {
                timezone: tz.name().to_string(),
                slots,
                dst: dst.unwrap_or_default(),
            })
        }
    }
}

pub fn normalize_catch_up(
    input: Option<&CatchUpPolicy>,
) -> Result<NormalizedCatchUpPolicy, ScheduleError> {
    let Some(input) = input else {
        return Ok(NormalizedCatchUpPolicy::default());
    };
    let max_age_minutes = input
        .max_age_minutes
        .unwrap_or(crate::model::DEFAULT_CATCH_UP_AGE_MINUTES);
    if !(1..=MAX_INTERVAL_MINUTES).contains(&max_age_minutes) {
        return Err(ScheduleError::InvalidCatchUpAge);
    }
    if input
        .max_runs
        .is_some_and(|value| !(1..=MAX_CATCH_UP_RUNS).contains(&value))
    {
        return Err(ScheduleError::InvalidCatchUpRuns);
    }

    let max_runs = match input.mode {
        CatchUpMode::Skip => input.max_runs.unwrap_or(1),
        CatchUpMode::Coalesce => {
            if input.max_runs.is_some_and(|value| value != 1) {
                return Err(ScheduleError::InvalidCoalesceRuns);
            }
            1
        }
        CatchUpMode::All => {
            let value = input.max_runs.ok_or(ScheduleError::InvalidCatchUpRuns)?;
            if !(1..=MAX_CATCH_UP_RUNS).contains(&value) {
                return Err(ScheduleError::InvalidCatchUpRuns);
            }
            value
        }
    };

    Ok(NormalizedCatchUpPolicy {
        mode: input.mode,
        max_age_minutes,
        max_runs,
    })
}

/// Return the first executable occurrence strictly after `after`.
pub fn next_occurrence(
    schedule: &NormalizedSchedule,
    after: DateTime<Utc>,
) -> Result<Option<NominalOccurrence>, ScheduleError> {
    match schedule {
        NormalizedSchedule::Once { at } => Ok((*at > after).then(|| once_occurrence(*at))),
        NormalizedSchedule::Interval {
            every_minutes,
            anchor,
        } => interval_next(*every_minutes, *anchor, after).map(Some),
        NormalizedSchedule::Weekly { .. } => {
            let through = after
                .checked_add_signed(Duration::days(WEEKLY_NEXT_SEARCH_DAYS))
                .ok_or(ScheduleError::IntervalOverflow)?;
            let batch = occurrences_between(schedule, after, through, 8_192)?;
            Ok(batch.occurrences.into_iter().next())
        }
    }
}

/// Enumerate executable and DST-skipped logical occurrences in `(after, through]`.
/// The returned `truncated` flag is set when more than `limit` executable entries exist.
pub fn occurrences_between(
    schedule: &NormalizedSchedule,
    after: DateTime<Utc>,
    through: DateTime<Utc>,
    limit: usize,
) -> Result<OccurrenceBatch, ScheduleError> {
    if through < after {
        return Err(ScheduleError::ReversedWindow);
    }
    if through.signed_duration_since(after) > Duration::days(MAX_WINDOW_DAYS) {
        return Err(ScheduleError::WindowTooLarge);
    }
    let limit = limit.clamp(1, MAX_WINDOW_OCCURRENCES);
    match schedule {
        NormalizedSchedule::Once { at } => {
            let occurrences = if *at > after && *at <= through {
                vec![once_occurrence(*at)]
            } else {
                Vec::new()
            };
            Ok(OccurrenceBatch {
                occurrences,
                ..OccurrenceBatch::default()
            })
        }
        NormalizedSchedule::Interval {
            every_minutes,
            anchor,
        } => interval_between(*every_minutes, *anchor, after, through, limit),
        NormalizedSchedule::Weekly {
            timezone,
            slots,
            dst,
        } => weekly_between(timezone, slots, *dst, after, through, limit),
    }
}

/// Compute bounded downtime catch-up without changing the recurrence grid.
pub fn compute_catch_up(
    schedule: &NormalizedSchedule,
    last_materialized_at: DateTime<Utc>,
    now: DateTime<Utc>,
    policy: &NormalizedCatchUpPolicy,
) -> Result<CatchUpPlan, ScheduleError> {
    if now < last_materialized_at {
        return Err(ScheduleError::ReversedWindow);
    }
    let cutoff = now
        .checked_sub_signed(Duration::minutes(i64::from(policy.max_age_minutes)))
        .ok_or(ScheduleError::IntervalOverflow)?;
    let after = last_materialized_at.max(cutoff);

    if let NormalizedSchedule::Interval {
        every_minutes,
        anchor,
    } = schedule
    {
        return interval_catch_up(*every_minutes, *anchor, after, now, policy);
    }

    calendar_catch_up(schedule, after, now, policy)
}

/// Fold calendar occurrences in small time chunks. This keeps catch-up memory
/// bounded even for the densest valid weekly schedule and still finds the true
/// latest occurrence for coalescing.
fn calendar_catch_up(
    schedule: &NormalizedSchedule,
    after: DateTime<Utc>,
    through: DateTime<Utc>,
    policy: &NormalizedCatchUpPolicy,
) -> Result<CatchUpPlan, ScheduleError> {
    let mut cursor = after;
    let mut total = 0_u64;
    let mut earliest = None;
    let mut latest = None;
    let mut selected = Vec::new();
    let mut skipped_for_dst = Vec::new();

    while cursor < through {
        let chunk_end = cursor
            .checked_add_signed(Duration::days(CATCH_UP_CHUNK_DAYS))
            .ok_or(ScheduleError::IntervalOverflow)?
            .min(through);
        let batch = occurrences_between(schedule, cursor, chunk_end, MAX_WINDOW_OCCURRENCES)?;
        if batch.truncated {
            return Err(ScheduleError::CatchUpDensityExceeded);
        }
        skipped_for_dst.extend(batch.skipped);
        for occurrence in batch.occurrences {
            total = total
                .checked_add(1)
                .ok_or(ScheduleError::IntervalOverflow)?;
            earliest.get_or_insert(occurrence.nominal_due_at);
            latest = Some(occurrence.clone());
            if policy.mode == CatchUpMode::All && selected.len() < usize::from(policy.max_runs) {
                selected.push(occurrence);
            }
        }
        cursor = chunk_end;
    }

    let mut plan = CatchUpPlan {
        skipped_for_dst,
        ..CatchUpPlan::default()
    };
    match policy.mode {
        CatchUpMode::Skip => plan.skipped_by_policy = total,
        CatchUpMode::Coalesce => {
            if let (Some(earliest), Some(latest)) = (earliest, latest) {
                plan.coalesced = Some(CoalescedWindow {
                    missed_count: total,
                    earliest,
                    latest: latest.nominal_due_at,
                });
                plan.runs.push(latest);
            }
        }
        CatchUpMode::All => {
            plan.omitted_by_cap = total.saturating_sub(selected.len() as u64);
            plan.runs = selected;
        }
    }
    Ok(plan)
}

fn interval_catch_up(
    every_minutes: u32,
    anchor: DateTime<Utc>,
    after: DateTime<Utc>,
    through: DateTime<Utc>,
    policy: &NormalizedCatchUpPolicy,
) -> Result<CatchUpPlan, ScheduleError> {
    let Some((first_ordinal, last_ordinal)) =
        interval_ordinal_bounds(every_minutes, anchor, after, through)?
    else {
        return Ok(CatchUpPlan::default());
    };
    let total = last_ordinal
        .checked_sub(first_ordinal)
        .and_then(|value| value.checked_add(1))
        .ok_or(ScheduleError::IntervalOverflow)?;
    let mut plan = CatchUpPlan::default();
    match policy.mode {
        CatchUpMode::Skip => plan.skipped_by_policy = total,
        CatchUpMode::Coalesce => {
            let earliest = interval_occurrence(every_minutes, anchor, first_ordinal)?;
            let latest = interval_occurrence(every_minutes, anchor, last_ordinal)?;
            plan.coalesced = Some(CoalescedWindow {
                missed_count: total,
                earliest: earliest.nominal_due_at,
                latest: latest.nominal_due_at,
            });
            plan.runs.push(latest);
        }
        CatchUpMode::All => {
            let count = total.min(u64::from(policy.max_runs));
            for offset in 0..count {
                plan.runs.push(interval_occurrence(
                    every_minutes,
                    anchor,
                    first_ordinal
                        .checked_add(offset)
                        .ok_or(ScheduleError::IntervalOverflow)?,
                )?);
            }
            plan.omitted_by_cap = total.saturating_sub(count);
        }
    }
    Ok(plan)
}

fn interval_between(
    every_minutes: u32,
    anchor: DateTime<Utc>,
    after: DateTime<Utc>,
    through: DateTime<Utc>,
    limit: usize,
) -> Result<OccurrenceBatch, ScheduleError> {
    let Some((first, last)) = interval_ordinal_bounds(every_minutes, anchor, after, through)?
    else {
        return Ok(OccurrenceBatch::default());
    };
    let available = last
        .checked_sub(first)
        .and_then(|value| value.checked_add(1))
        .ok_or(ScheduleError::IntervalOverflow)?;
    let take = available.min(limit as u64);
    let mut occurrences = Vec::with_capacity(usize::try_from(take).unwrap_or(limit));
    for offset in 0..take {
        occurrences.push(interval_occurrence(
            every_minutes,
            anchor,
            first
                .checked_add(offset)
                .ok_or(ScheduleError::IntervalOverflow)?,
        )?);
    }
    Ok(OccurrenceBatch {
        occurrences,
        truncated: available > take,
        ..OccurrenceBatch::default()
    })
}

fn interval_ordinal_bounds(
    every_minutes: u32,
    anchor: DateTime<Utc>,
    after: DateTime<Utc>,
    through: DateTime<Utc>,
) -> Result<Option<(u64, u64)>, ScheduleError> {
    validate_interval(every_minutes)?;
    if through < anchor || through <= after {
        return Ok(None);
    }
    let first = interval_next_ordinal(every_minutes, anchor, after)?;
    let period_seconds = i64::from(every_minutes) * 60;
    let elapsed = through.signed_duration_since(anchor).num_seconds();
    if elapsed < 0 {
        return Ok(None);
    }
    let last =
        u64::try_from(elapsed / period_seconds).map_err(|_| ScheduleError::IntervalOverflow)?;
    Ok((first <= last).then_some((first, last)))
}

fn interval_next(
    every_minutes: u32,
    anchor: DateTime<Utc>,
    after: DateTime<Utc>,
) -> Result<NominalOccurrence, ScheduleError> {
    let ordinal = interval_next_ordinal(every_minutes, anchor, after)?;
    interval_occurrence(every_minutes, anchor, ordinal)
}

fn interval_next_ordinal(
    every_minutes: u32,
    anchor: DateTime<Utc>,
    after: DateTime<Utc>,
) -> Result<u64, ScheduleError> {
    validate_interval(every_minutes)?;
    if after < anchor {
        return Ok(0);
    }
    let period_seconds = i64::from(every_minutes) * 60;
    let elapsed_seconds = after.signed_duration_since(anchor).num_seconds();
    u64::try_from(elapsed_seconds / period_seconds)
        .map_err(|_| ScheduleError::IntervalOverflow)?
        .checked_add(1)
        .ok_or(ScheduleError::IntervalOverflow)
}

fn interval_occurrence(
    every_minutes: u32,
    anchor: DateTime<Utc>,
    ordinal: u64,
) -> Result<NominalOccurrence, ScheduleError> {
    let period_seconds = u64::from(every_minutes)
        .checked_mul(60)
        .ok_or(ScheduleError::IntervalOverflow)?;
    let offset_seconds = ordinal
        .checked_mul(period_seconds)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(ScheduleError::IntervalOverflow)?;
    let nominal_due_at = anchor
        .checked_add_signed(Duration::seconds(offset_seconds))
        .ok_or(ScheduleError::IntervalOverflow)?;
    Ok(NominalOccurrence {
        identity: OccurrenceIdentitySeed::Interval { anchor, ordinal },
        nominal_due_at,
        intended_local_time: None,
        stable_slot_id: None,
        timezone: None,
        resolved_offset: Some("+00:00".to_string()),
        resolution_ordinal: 0,
        dst_resolution: DstResolution::Ordinary,
    })
}

fn once_occurrence(at: DateTime<Utc>) -> NominalOccurrence {
    NominalOccurrence {
        identity: OccurrenceIdentitySeed::Once {
            declared_instant: at,
        },
        nominal_due_at: at,
        intended_local_time: None,
        stable_slot_id: None,
        timezone: None,
        resolved_offset: Some("+00:00".to_string()),
        resolution_ordinal: 0,
        dst_resolution: DstResolution::Ordinary,
    }
}

fn weekly_between(
    timezone: &str,
    slots: &[NormalizedWeeklySlot],
    dst: DstPolicy,
    after: DateTime<Utc>,
    through: DateTime<Utc>,
    limit: usize,
) -> Result<OccurrenceBatch, ScheduleError> {
    let tz = parse_timezone(timezone)?;
    let start_date = after.with_timezone(&tz).date_naive();
    let end_date = through.with_timezone(&tz).date_naive();
    let local_after = after.with_timezone(&tz).naive_local();
    let local_through = through.with_timezone(&tz).naive_local();
    let mut date = start_date;
    let mut occurrences = Vec::new();
    let mut skipped = Vec::new();
    let mut truncated = false;

    while date <= end_date {
        for slot in slots {
            if chrono_weekday(slot.weekday) != date.weekday() {
                continue;
            }
            let time = parse_local_time(&slot.local_time)
                .map_err(|_| ScheduleError::InvalidNormalizedSlot)?;
            let intended = date.and_time(time);
            match resolve_local(tz, intended, dst)? {
                LocalResolution::Resolved(values) => {
                    for value in values {
                        if value.utc <= after || value.utc > through {
                            continue;
                        }
                        if occurrences.len() == limit {
                            truncated = true;
                            continue;
                        }
                        occurrences.push(NominalOccurrence {
                            identity: OccurrenceIdentitySeed::Weekly {
                                stable_slot_id: slot.stable_slot_id.clone(),
                                intended_local_time: intended,
                                resolution_ordinal: value.ordinal,
                            },
                            nominal_due_at: value.utc,
                            intended_local_time: Some(intended),
                            stable_slot_id: Some(slot.stable_slot_id.clone()),
                            timezone: Some(timezone.to_string()),
                            resolved_offset: Some(format_offset(value.offset)),
                            resolution_ordinal: value.ordinal,
                            dst_resolution: value.dst_resolution,
                        });
                    }
                }
                LocalResolution::Skipped if intended > local_after && intended <= local_through => {
                    skipped.push(SkippedOccurrence {
                        identity: OccurrenceIdentitySeed::Weekly {
                            stable_slot_id: slot.stable_slot_id.clone(),
                            intended_local_time: intended,
                            resolution_ordinal: 0,
                        },
                        intended_local_time: intended,
                        stable_slot_id: slot.stable_slot_id.clone(),
                        timezone: timezone.to_string(),
                        dst_resolution: DstResolution::GapSkipped,
                    });
                }
                LocalResolution::Skipped => {}
            }
        }
        date = date.succ_opt().ok_or(ScheduleError::IntervalOverflow)?;
    }
    occurrences.sort_by_key(|occurrence| occurrence.nominal_due_at);
    skipped.sort_by_key(|occurrence| occurrence.intended_local_time);
    Ok(OccurrenceBatch {
        occurrences,
        skipped,
        truncated,
    })
}

#[derive(Debug)]
struct ResolvedLocal {
    utc: DateTime<Utc>,
    offset: FixedOffset,
    ordinal: u8,
    dst_resolution: DstResolution,
}

enum LocalResolution {
    Resolved(Vec<ResolvedLocal>),
    Skipped,
}

fn resolve_local(
    tz: Tz,
    intended: NaiveDateTime,
    dst: DstPolicy,
) -> Result<LocalResolution, ScheduleError> {
    match tz.from_local_datetime(&intended) {
        LocalResult::Single(value) => Ok(LocalResolution::Resolved(vec![resolved_local(
            value,
            0,
            DstResolution::Ordinary,
        )])),
        LocalResult::Ambiguous(first, second) => {
            let (earlier, later) = if first.with_timezone(&Utc) <= second.with_timezone(&Utc) {
                (first, second)
            } else {
                (second, first)
            };
            let values = match dst.fold {
                DstFoldPolicy::First => {
                    vec![resolved_local(earlier, 0, DstResolution::FoldFirst)]
                }
                DstFoldPolicy::Second => {
                    vec![resolved_local(later, 1, DstResolution::FoldSecond)]
                }
                DstFoldPolicy::Both => vec![
                    resolved_local(earlier, 0, DstResolution::FoldFirst),
                    resolved_local(later, 1, DstResolution::FoldSecond),
                ],
            };
            Ok(LocalResolution::Resolved(values))
        }
        LocalResult::None => match dst.gap {
            DstGapPolicy::Skip => Ok(LocalResolution::Skipped),
            DstGapPolicy::Error => Err(ScheduleError::NonexistentLocalTime {
                timezone: tz.name().to_string(),
                local: intended,
            }),
            DstGapPolicy::NextValid => {
                let mut candidate = intended;
                for _ in 0..=(48 * 60) {
                    candidate = candidate
                        .checked_add_signed(Duration::minutes(1))
                        .ok_or(ScheduleError::IntervalOverflow)?;
                    match tz.from_local_datetime(&candidate) {
                        LocalResult::Single(value) => {
                            return Ok(LocalResolution::Resolved(vec![resolved_local(
                                value,
                                0,
                                DstResolution::GapNextValid,
                            )]));
                        }
                        LocalResult::Ambiguous(first, second) => {
                            let value = if first.with_timezone(&Utc) <= second.with_timezone(&Utc) {
                                first
                            } else {
                                second
                            };
                            return Ok(LocalResolution::Resolved(vec![resolved_local(
                                value,
                                0,
                                DstResolution::GapNextValid,
                            )]));
                        }
                        LocalResult::None => {}
                    }
                }
                Err(ScheduleError::UnresolvableDstGap {
                    timezone: tz.name().to_string(),
                    local: intended,
                })
            }
        },
    }
}

fn resolved_local<TzOffset: TimeZone>(
    value: DateTime<TzOffset>,
    ordinal: u8,
    dst_resolution: DstResolution,
) -> ResolvedLocal
where
    TzOffset::Offset: Offset,
{
    ResolvedLocal {
        utc: value.with_timezone(&Utc),
        offset: value.offset().fix(),
        ordinal,
        dst_resolution,
    }
}

fn validate_interval(every_minutes: u32) -> Result<(), ScheduleError> {
    if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&every_minutes) {
        return Err(ScheduleError::InvalidInterval);
    }
    Ok(())
}

fn parse_timezone(timezone: &str) -> Result<Tz, ScheduleError> {
    Tz::from_str(timezone).map_err(|_| ScheduleError::InvalidTimezone {
        timezone: timezone.to_string(),
    })
}

fn parse_local_time(value: &str) -> Result<NaiveTime, ScheduleError> {
    let bytes = value.as_bytes();
    let has_exact_shape = match bytes {
        [h1, h2, b':', m1, m2] => [h1, h2, m1, m2].into_iter().all(u8::is_ascii_digit),
        [h1, h2, b':', m1, m2, b':', s1, s2] => {
            [h1, h2, m1, m2, s1, s2].into_iter().all(u8::is_ascii_digit)
        }
        _ => false,
    };
    if !has_exact_shape {
        return Err(ScheduleError::InvalidLocalTime {
            value: value.to_string(),
        });
    }

    ["%H:%M", "%H:%M:%S"]
        .into_iter()
        .find_map(|format| NaiveTime::parse_from_str(value, format).ok())
        .ok_or_else(|| ScheduleError::InvalidLocalTime {
            value: value.to_string(),
        })
}

fn canonical_time(time: NaiveTime) -> String {
    time.format("%H:%M:%S").to_string()
}

fn stable_slot_id(tz: Tz, weekday: Weekday, local_time: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"threadbeat-weekly-slot-v1\0");
    hasher.update(tz.name().as_bytes());
    hasher.update(b"\0");
    hasher.update(weekday_token(weekday).as_bytes());
    hasher.update(b"\0");
    hasher.update(local_time.as_bytes());
    let digest = hasher.finalize();
    format!("ws_{}", hex::encode(&digest[..10]))
}

const fn weekday_sort_key(weekday: Weekday) -> u8 {
    match weekday {
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
        Weekday::Sun => 7,
    }
}

const fn weekday_token(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

const fn chrono_weekday(weekday: Weekday) -> chrono::Weekday {
    match weekday {
        Weekday::Mon => chrono::Weekday::Mon,
        Weekday::Tue => chrono::Weekday::Tue,
        Weekday::Wed => chrono::Weekday::Wed,
        Weekday::Thu => chrono::Weekday::Thu,
        Weekday::Fri => chrono::Weekday::Fri,
        Weekday::Sat => chrono::Weekday::Sat,
        Weekday::Sun => chrono::Weekday::Sun,
    }
}

fn format_offset(offset: FixedOffset) -> String {
    let seconds = offset.local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let absolute = seconds.unsigned_abs();
    format!(
        "{sign}{:02}:{:02}",
        absolute / 3_600,
        (absolute % 3_600) / 60
    )
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone};
    use proptest::prelude::*;

    use super::*;
    use crate::model::{CatchUpPolicy, WeeklyGroup};

    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("valid test timestamp")
    }

    fn weekly(timezone: &str, weekday: Weekday, local_time: &str) -> NormalizedSchedule {
        normalize_schedule(
            &ScheduleSpec::Weekly {
                timezone: timezone.to_string(),
                groups: vec![WeeklyGroup {
                    weekdays: vec![weekday],
                    local_times: vec![local_time.to_string()],
                }],
                dst: None,
            },
            utc(2026, 1, 1, 0, 0),
        )
        .expect("weekly schedule should normalize")
    }

    #[test]
    fn interval_is_fixed_rate_from_anchor() {
        let anchor = utc(2026, 1, 1, 0, 0);
        let schedule = NormalizedSchedule::Interval {
            every_minutes: 15,
            anchor,
        };
        let next = next_occurrence(&schedule, utc(2026, 1, 1, 1, 7))
            .expect("next occurrence")
            .expect("an interval is unbounded");
        assert_eq!(next.nominal_due_at, utc(2026, 1, 1, 1, 15));
        assert_eq!(
            next.identity,
            OccurrenceIdentitySeed::Interval { anchor, ordinal: 5 }
        );
    }

    #[test]
    fn normalization_expands_weekday_time_cartesian_product() {
        let input = ScheduleSpec::Weekly {
            timezone: "America/Los_Angeles".to_string(),
            groups: vec![WeeklyGroup {
                weekdays: vec![Weekday::Wed, Weekday::Mon, Weekday::Mon],
                local_times: vec!["12:00".to_string(), "10:00:00".to_string()],
            }],
            dst: None,
        };
        let normalized = normalize_schedule(&input, utc(2026, 1, 1, 0, 0))
            .expect("weekly schedule should normalize");
        let NormalizedSchedule::Weekly { slots, dst, .. } = normalized else {
            panic!("expected weekly schedule");
        };
        assert_eq!(slots.len(), 4);
        assert_eq!(slots[0].weekday, Weekday::Mon);
        assert_eq!(slots[0].local_time, "10:00:00");
        assert_eq!(dst, DstPolicy::default());
        assert!(
            slots
                .windows(2)
                .all(|pair| pair[0].stable_slot_id != pair[1].stable_slot_id)
        );
    }

    #[test]
    fn spring_gap_is_recorded_and_skipped() {
        // America/Los_Angeles jumps from 01:59:59 to 03:00 on 2026-03-08.
        let schedule = weekly("America/Los_Angeles", Weekday::Sun, "02:30");
        let batch =
            occurrences_between(&schedule, utc(2026, 3, 8, 0, 0), utc(2026, 3, 9, 12, 0), 10)
                .expect("window should resolve");
        assert!(batch.occurrences.is_empty());
        assert_eq!(batch.skipped.len(), 1);
        assert_eq!(batch.skipped[0].dst_resolution, DstResolution::GapSkipped);

        let next = next_occurrence(&schedule, utc(2026, 3, 8, 0, 0))
            .expect("next occurrence")
            .expect("following Sunday should exist");
        assert_eq!(
            next.intended_local_time.map(|value| value.date()),
            NaiveDate::from_ymd_opt(2026, 3, 15)
        );
    }

    #[test]
    fn fall_fold_defaults_to_first_instant_once() {
        // 01:30 occurs at both 08:30Z and 09:30Z on 2026-11-01 in Los Angeles.
        let schedule = weekly("America/Los_Angeles", Weekday::Sun, "01:30");
        let batch = occurrences_between(
            &schedule,
            utc(2026, 11, 1, 0, 0),
            utc(2026, 11, 2, 0, 0),
            10,
        )
        .expect("window should resolve");
        assert_eq!(batch.occurrences.len(), 1);
        assert_eq!(batch.occurrences[0].nominal_due_at, utc(2026, 11, 1, 8, 30));
        assert_eq!(
            batch.occurrences[0].dst_resolution,
            DstResolution::FoldFirst
        );
        assert_eq!(batch.occurrences[0].resolution_ordinal, 0);
    }

    #[test]
    fn coalesce_keeps_latest_slot_without_moving_grid() {
        let schedule = NormalizedSchedule::Interval {
            every_minutes: 30,
            anchor: utc(2026, 1, 1, 10, 0),
        };
        let policy = normalize_catch_up(Some(&CatchUpPolicy {
            mode: CatchUpMode::Coalesce,
            max_age_minutes: Some(1_440),
            max_runs: None,
        }))
        .expect("policy should normalize");
        let plan = compute_catch_up(
            &schedule,
            utc(2026, 1, 1, 10, 0),
            utc(2026, 1, 1, 11, 3),
            &policy,
        )
        .expect("catch-up should compute");
        assert_eq!(plan.runs.len(), 1);
        assert_eq!(plan.runs[0].nominal_due_at, utc(2026, 1, 1, 11, 0));
        assert_eq!(plan.coalesced.expect("coalesced metadata").missed_count, 2);
        let next = next_occurrence(&schedule, utc(2026, 1, 1, 11, 3))
            .expect("next occurrence")
            .expect("interval is unbounded");
        assert_eq!(next.nominal_due_at, utc(2026, 1, 1, 11, 30));
    }

    #[test]
    fn all_catch_up_is_bounded_but_chronological() {
        let schedule = NormalizedSchedule::Interval {
            every_minutes: 1,
            anchor: utc(2026, 1, 1, 0, 0),
        };
        let policy = normalize_catch_up(Some(&CatchUpPolicy {
            mode: CatchUpMode::All,
            max_age_minutes: Some(60),
            max_runs: Some(3),
        }))
        .expect("policy should normalize");
        let plan = compute_catch_up(
            &schedule,
            utc(2026, 1, 1, 0, 0),
            utc(2026, 1, 1, 0, 10),
            &policy,
        )
        .expect("catch-up should compute");
        let due = plan
            .runs
            .iter()
            .map(|run| run.nominal_due_at)
            .collect::<Vec<_>>();
        assert_eq!(
            due,
            vec![
                utc(2026, 1, 1, 0, 1),
                utc(2026, 1, 1, 0, 2),
                utc(2026, 1, 1, 0, 3)
            ]
        );
        assert_eq!(plan.omitted_by_cap, 7);
    }

    #[test]
    fn weekly_coalesce_streams_across_chunks_and_keeps_actual_latest() {
        let schedule = normalize_schedule(
            &ScheduleSpec::Weekly {
                timezone: "UTC".to_string(),
                groups: vec![WeeklyGroup {
                    weekdays: vec![
                        Weekday::Mon,
                        Weekday::Tue,
                        Weekday::Wed,
                        Weekday::Thu,
                        Weekday::Fri,
                        Weekday::Sat,
                        Weekday::Sun,
                    ],
                    local_times: vec!["10:00".to_string()],
                }],
                dst: None,
            },
            utc(2026, 1, 1, 0, 0),
        )
        .expect("daily weekly schedule should normalize");
        let policy = normalize_catch_up(Some(&CatchUpPolicy {
            mode: CatchUpMode::Coalesce,
            max_age_minutes: Some(60 * 24 * 60),
            max_runs: None,
        }))
        .expect("policy should normalize");

        let plan = compute_catch_up(
            &schedule,
            utc(2026, 1, 1, 0, 0),
            utc(2026, 2, 1, 0, 0),
            &policy,
        )
        .expect("catch-up should stream across chunk boundaries");

        assert_eq!(plan.runs.len(), 1);
        assert_eq!(plan.runs[0].nominal_due_at, utc(2026, 1, 31, 10, 0));
        assert_eq!(plan.coalesced.expect("summary").missed_count, 31);
    }

    #[test]
    fn once_is_strictly_after_cursor() {
        let at = utc(2026, 1, 2, 3, 4);
        let schedule = NormalizedSchedule::Once { at };
        assert!(
            next_occurrence(&schedule, at)
                .expect("valid query")
                .is_none()
        );
        assert_eq!(
            next_occurrence(&schedule, at - Duration::seconds(1))
                .expect("valid query")
                .expect("once should be due")
                .nominal_due_at,
            at
        );
    }

    #[test]
    fn normalization_rejects_non_iana_abbreviation() {
        let error = normalize_schedule(
            &ScheduleSpec::Weekly {
                timezone: "PDT".to_string(),
                groups: vec![WeeklyGroup {
                    weekdays: vec![Weekday::Mon],
                    local_times: vec!["10:00".to_string()],
                }],
                dst: None,
            },
            utc(2026, 1, 1, 0, 0),
        )
        .expect_err("timezone abbreviations must not be accepted");
        assert!(matches!(error, ScheduleError::InvalidTimezone { .. }));
    }

    #[test]
    fn normalization_rejects_noncanonical_local_time_and_invalid_caps() {
        let schedule_error = normalize_schedule(
            &ScheduleSpec::Weekly {
                timezone: "UTC".to_string(),
                groups: vec![WeeklyGroup {
                    weekdays: vec![Weekday::Mon],
                    local_times: vec!["1:00".to_string()],
                }],
                dst: None,
            },
            utc(2026, 1, 1, 0, 0),
        )
        .expect_err("local wall time must use two-digit hours");
        assert!(matches!(
            schedule_error,
            ScheduleError::InvalidLocalTime { .. }
        ));

        let catch_up_error = normalize_catch_up(Some(&CatchUpPolicy {
            mode: CatchUpMode::Skip,
            max_age_minutes: None,
            max_runs: Some(0),
        }))
        .expect_err("provided run caps are always range checked");
        assert_eq!(catch_up_error, ScheduleError::InvalidCatchUpRuns);
    }

    proptest! {
        #[test]
        fn interval_grid_never_drifts_from_its_anchor(
            every_minutes in MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES,
            ordinal in 0_u64..50,
            arbitrary_lateness_seconds in 0_i64..60,
        ) {
            let anchor = utc(2026, 1, 1, 0, 0);
            let period_seconds = i64::from(every_minutes) * 60;
            let due = anchor
                .checked_add_signed(Duration::seconds(
                    i64::try_from(ordinal).expect("small generated ordinal") * period_seconds,
                ))
                .expect("generated occurrence stays in chrono range");
            let cursor = due + Duration::seconds(arbitrary_lateness_seconds);
            let schedule = NormalizedSchedule::Interval {
                every_minutes,
                anchor,
            };

            let next = next_occurrence(&schedule, cursor)
                .expect("valid interval")
                .expect("interval is unbounded");
            let expected_ordinal = ordinal + 1;
            let expected_due = anchor
                + Duration::seconds(
                    i64::try_from(expected_ordinal).expect("small generated ordinal")
                        * period_seconds,
                );

            prop_assert_eq!(next.nominal_due_at, expected_due);
            prop_assert_eq!(
                next.identity,
                OccurrenceIdentitySeed::Interval {
                    anchor,
                    ordinal: expected_ordinal,
                }
            );
        }
    }
}
