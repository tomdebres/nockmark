//! How a run's proofs were scheduled, and which rows a board ranks because of
//! it (M7 step 5, part 2).
//!
//! Until tock's pipelined client, every run ground all `k` wins and then
//! proved them one at a time. The tier rule sizes each win's grind at `R = 2`
//! proofs, so the server window was `k·R·p + k·p` and a row recovered at most
//! `R/(R+1)` = 2/3 of its machine's rate. The pipelined client proves each win
//! while it keeps grinding: the window is `k·R·p + p`, about 97 % at `k = 16`.
//!
//! Both are honest lower bounds, and `p` cancels in both, so either kind of
//! row is comparable with its own kind. Mixed in one ranking they are not: a
//! serial row understates its machine by about 1.4× against a pipelined one.
//!
//! **Ranking uses only what this server observed.** Every run it committed
//! before [`PIPELINED_CLIENT_SINCE_MS`] came from the serial client, the only
//! one there was. Once a board has a current-rules row committed after that,
//! it ranks only such rows, and the older ones stay listed after them,
//! unranked. Before that, nothing changes.
//!
//! The submission's `proving` field is the client's own account of how it
//! scheduled its proofs. It is recorded and shown as a label, and it decides
//! nothing: a row's rank never depends on it. (An earlier draft ranked on the
//! claim; one false "pipelined" row could then unrank every honest row on the
//! board. Adversarial review, 2026-11-13.) A run made after the release with
//! `--serial` is ranked with the rest, at the lower rate its idle grinder
//! cost it.

use serde::{Deserialize, Serialize};

/// How a run's proofs were scheduled, as the client reported it: a label,
/// never a ranking input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proving {
    /// Grind all `k` wins, then prove them: every run before the field
    /// existed, hence the default on replay.
    #[default]
    Serial,
    /// Prove each win while the grind goes on.
    Pipelined,
}

impl Proving {
    /// The submission's optional `proving` field. Absent means serial: every
    /// client before the field was. Any other value is refused rather than
    /// guessed at.
    pub fn from_submission(v: Option<&str>) -> Result<Self, String> {
        match v {
            None | Some("serial") => Ok(Self::Serial),
            Some("pipelined") => Ok(Self::Pipelined),
            Some(other) => Err(format!(
                "unknown proving mode {other:?} (expected \"pipelined\" or \"serial\")"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serial => "serial",
            Self::Pipelined => "pipelined",
        }
    }
}

/// When tock's pipelined client was released (M7 step 5, part 2),
/// 2026-11-13 00:00 UTC, as this server's clock reads it. The last run the
/// live registry committed before it was at 2026-11-12 22:18 UTC.
pub const PIPELINED_CLIENT_SINCE_MS: u64 = 1_794_528_000_000;

/// Was this run committed (server clock) before the pipelined client
/// existed, hence by the serial one?
pub fn before_pipelining(submitted_at_ms: u64) -> bool {
    submitted_at_ms < PIPELINED_CLIENT_SINCE_MS
}

/// Is a current-rules row ranked? `before` is [`before_pipelining`] for the
/// row; `board_has_newer` says whether any current-rules row on its board was
/// committed after the release.
pub fn is_ranked(before: bool, board_has_newer: bool) -> bool {
    !before || !board_has_newer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_is_serial_and_unknown_is_refused() {
        assert_eq!(Proving::from_submission(None), Ok(Proving::Serial));
        assert_eq!(Proving::from_submission(Some("serial")), Ok(Proving::Serial));
        assert_eq!(Proving::from_submission(Some("pipelined")), Ok(Proving::Pipelined));
        assert!(Proving::from_submission(Some("Pipelined")).is_err());
        assert!(Proving::from_submission(Some("")).is_err());
    }

    #[test]
    fn older_rows_are_ranked_only_until_a_newer_row_exists() {
        assert!(is_ranked(true, false));
        assert!(!is_ranked(true, true));
        assert!(is_ranked(false, true));
        assert!(is_ranked(false, false));
        assert!(before_pipelining(PIPELINED_CLIENT_SINCE_MS - 1));
        assert!(!before_pipelining(PIPELINED_CLIENT_SINCE_MS));
    }

    #[test]
    fn wire_names() {
        assert_eq!(serde_json::to_value(Proving::Pipelined).unwrap(), "pipelined");
        assert_eq!(serde_json::to_value(Proving::Serial).unwrap(), "serial");
        assert_eq!(Proving::Pipelined.as_str(), "pipelined");
    }
}
