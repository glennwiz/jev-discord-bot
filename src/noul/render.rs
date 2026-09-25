//! Discord reply text for a noul result or failure.
//!
//! Only P(yes) is shown. No "Yes"/"No" headline and no confidence: Jev
//! sends neither, and picking a cut-off is the reader's decision.

use super::input::{InputError, NoulRequest};
use super::jev::{NoulError, NoulOutcome};

/// Discord's message content limit.
pub const MAX_MESSAGE_CHARS: usize = 2_000;
const MAX_QUESTION_ECHO_CHARS: usize = 300;

pub fn outcome(req: &NoulRequest, out: &NoulOutcome) -> String {
    let text = format!(
        "**Question:** {}\n**P(yes):** {} (on 0-1)",
        clip(&req.question, MAX_QUESTION_ECHO_CHARS),
        number(out.p_yes),
    );
    clip(&text, MAX_MESSAGE_CHARS)
}

/// Shortest exact form of the value Jev sent: 0.12 -> "0.12", 1.0 -> "1".
pub fn number(v: f64) -> String {
    format!("{v}")
}

pub fn input_error(e: &InputError) -> String {
    clip(&format!("Can't ask Jev yet: {e}"), MAX_MESSAGE_CHARS)
}

pub fn jev_error(e: &NoulError) -> String {
    clip(&format!("Jev noul failed: {e}"), MAX_MESSAGE_CHARS)
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
