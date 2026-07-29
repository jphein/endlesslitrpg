//! Reading the engine's heartbeat, and judging it.
//!
//! The store records what the engine reported and deliberately refuses to decide whether
//! it is stale, because staleness depends on `poll_interval_secs`, which lives in config
//! the store does not read. So the judgement lives here, next to the config that informs
//! it.

use litrpg_store::Store;
use serde::Serialize;

use crate::Result;

/// Multiple of `poll_interval_secs` beyond which a heartbeat is called stale.
///
/// **Generous on purpose.** The engine stamps once per poll cycle, and a cycle that
/// renders a chapter includes generation, TTS and mp3 encoding — minutes of work during
/// which the heartbeat legitimately ages without anything being wrong. A tight threshold
/// would report "the engine has stopped" exactly when it is busiest, which is the worst
/// moment to be wrong, and would train the operator to ignore the line.
pub const STALE_AFTER_CYCLES: u64 = 20;

/// Floor for the same threshold, so a short poll interval cannot make it shorter than a
/// single render takes.
pub const STALE_FLOOR_SECS: u64 = 900;

/// How long a heartbeat may age before it is reported as stale.
pub fn stale_after_secs(poll_interval_secs: u64) -> u64 {
    poll_interval_secs
        .saturating_mul(STALE_AFTER_CYCLES)
        .max(STALE_FLOOR_SECS)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum EngineStatus {
    /// No engine has ever run against this database.
    ///
    /// Distinct from stale on purpose: nothing has ever rendered here, versus something
    /// rendered and then stopped. They call for different words.
    NeverSeen,
    Reported {
        age_secs: u64,
        stale: bool,
        pid: i64,
        version: String,
        /// TTS backends the running engine actually registered.
        backends: Vec<String>,
    },
}

impl EngineStatus {
    /// Whether queued work can be expected to happen.
    pub fn will_render(&self) -> bool {
        matches!(self, Self::Reported { stale: false, .. })
    }

    pub fn backends(&self) -> &[String] {
        match self {
            Self::NeverSeen => &[],
            Self::Reported { backends, .. } => backends,
        }
    }

    /// Backends referenced by `wanted` that the running engine does not provide.
    ///
    /// Empty when the engine's registry is unknown — an absent heartbeat is not evidence
    /// that a backend is missing, and claiming otherwise would be the "silence is not
    /// evidence" mistake.
    pub fn missing_backends(&self, wanted: &[String]) -> Vec<String> {
        let have = self.backends();
        if have.is_empty() {
            return Vec::new();
        }
        let mut missing: Vec<String> = wanted
            .iter()
            .filter(|w| !have.iter().any(|h| h == *w))
            .cloned()
            .collect();
        missing.sort();
        missing.dedup();
        missing
    }
}

/// Read and judge the heartbeat. `now_ms` is injected so staleness is testable.
pub fn engine_status(store: &Store, poll_interval_secs: u64, now_ms: i64) -> Result<EngineStatus> {
    let Some(hb) = store.engine_heartbeat()? else {
        return Ok(EngineStatus::NeverSeen);
    };
    // A heartbeat stamped in the future (clock skew, a restored backup) reads as age 0
    // rather than underflowing into a huge "stale" number.
    let age_secs = now_ms.saturating_sub(hb.seen_at).max(0) as u64 / 1000;
    Ok(EngineStatus::Reported {
        age_secs,
        stale: age_secs > stale_after_secs(poll_interval_secs),
        pid: hb.pid,
        version: hb.version,
        backends: hb.backends,
    })
}

/// A span of time, with no "ago".
///
/// Split from [`describe_age`] because reusing that for the staleness *threshold* read
/// as "longer than the 15 min ago it is allowed" — a duration described as a point in
/// the past. Caught by smoke-testing the real output.
pub fn describe_duration(secs: u64) -> String {
    match secs {
        0..=90 => format!("{secs}s"),
        s if s < 5400 => format!("{} min", s / 60),
        s => format!("{} h", s / 3600),
    }
}

pub fn describe_age(age_secs: u64) -> String {
    format!("{} ago", describe_duration(age_secs))
}

/// One line about the engine, or `None` when there is nothing useful to say.
pub fn describe(status: &EngineStatus, poll_interval_secs: u64) -> String {
    match status {
        EngineStatus::NeverSeen => format!(
            "No engine has ever run against this database, so nothing will render until\n\
             `litrpg-engine` is started. Once running it picks queued chapters up within\n\
             {poll_interval_secs} seconds.\n"
        ),
        EngineStatus::Reported {
            age_secs,
            stale: false,
            pid,
            version,
            backends,
        } => format!(
            "litrpg-engine {version} is running (pid {pid}, last cycle {}); it picks queued\n\
             chapters up within {poll_interval_secs} seconds. Backends: {}.\n",
            describe_age(*age_secs),
            if backends.is_empty() {
                "none registered".to_string()
            } else {
                backends.join(", ")
            }
        ),
        EngineStatus::Reported {
            age_secs,
            stale: true,
            pid,
            version,
            ..
        } => format!(
            "!! litrpg-engine {version} (pid {pid}) last reported {} — longer than the {}\n\
             !! it is allowed, so it has probably stopped. Queued chapters will not render\n\
             !! until it is running again.\n",
            describe_age(*age_secs),
            describe_duration(stale_after_secs(poll_interval_secs))
        ),
    }
}
