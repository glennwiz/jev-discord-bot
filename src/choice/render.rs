//! Discord reply text for a choice result or failure.

use super::input::{ChoiceRequest, InputError};
use super::jev::{ChoiceOutcome, JevError};

/// Discord's message content limit.
pub const MAX_MESSAGE_CHARS: usize = 2_000;
const MAX_QUESTION_ECHO_CHARS: usize = 300;

/// Picked option, its probability, and Jev's confidence on separate labels.
pub fn outcome(req: &ChoiceRequest, out: &ChoiceOutcome) -> String {
    let text = format!(
        "**Question:** {}\n**Options:** {}\n**Jev picks:** {}\nProbability of this option: {} · Jev confidence: {}",
        clip(&req.question, MAX_QUESTION_ECHO_CHARS),
        req.options.join(", "),
        out.choice,
        percent(out.probability),
        percent(out.confidence),
    );
    clip(&text, MAX_MESSAGE_CHARS)
}

pub fn input_error(e: &InputError) -> String {
    clip(&format!("Can't ask Jev yet: {e}"), MAX_MESSAGE_CHARS)
}

pub fn jev_error(e: &JevError) -> String {
    clip(&format!("Jev choice failed: {e}"), MAX_MESSAGE_CHARS)
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
