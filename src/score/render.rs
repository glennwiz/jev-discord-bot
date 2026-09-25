//! Discord reply text for a score result or failure.

use super::input::{InputError, ScoreRequest};
use super::jev::{ScoreError, ScoreOutcome};

/// Discord's message content limit.
pub const MAX_MESSAGE_CHARS: usize = 2_000;
const MAX_QUESTION_ECHO_CHARS: usize = 300;

/// Unrounded score on its scale, where it falls between levels, the ordered
/// legend numbered like the scale, and Jev's confidence on its own line.
pub fn outcome(req: &ScoreRequest, out: &ScoreOutcome) -> String {
    let legend: Vec<String> = req
        .levels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let n = req.level_score(i);
            match out.level_probabilities.iter().find(|(lv, _)| *lv == n) {
                Some((_, p)) => format!("`{n}` {l} ({})", percent(*p)),
                None => format!("`{n}` {l}"),
            }
        })
        .collect();
    let text = format!(
        "**Question:** {}\n**Jev score:** {} on {}-{} ({})\n**Levels:** {}\nJev confidence: {}",
        clip(&req.question, MAX_QUESTION_ECHO_CHARS),
        number(out.score),
        req.min_score(),
        req.max_score(),
        position(req, out.score),
        legend.join(" · "),
        percent(out.confidence),
    );
    clip(&text, MAX_MESSAGE_CHARS)
}

/// "at soon" on a whole level, else "between soon and urgent".
pub fn position(req: &ScoreRequest, score: f64) -> String {
    match req.level_at_or_below(score) {
        (i, true) => format!("at {}", req.levels[i]),
        (i, false) => format!("between {} and {}", req.levels[i], req.levels[i + 1]),
    }
}

/// Shortest exact form of the value Jev sent: 1.4 -> "1.4", 2.0 -> "2",
/// 1.9996 -> "1.9996". Never rounds to a whole level.
pub fn number(v: f64) -> String {
    format!("{v}")
}

pub fn input_error(e: &InputError) -> String {
    clip(&format!("Can't ask Jev yet: {e}"), MAX_MESSAGE_CHARS)
}

pub fn jev_error(e: &ScoreError) -> String {
    clip(&format!("Jev score failed: {e}"), MAX_MESSAGE_CHARS)
}

pub fn percent(v: f64) -> String {
    format!("{:.1}%", v * 100.0)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}
