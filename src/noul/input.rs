//! Parse and bound the user's `/jev noul` input before anything is sent.
//!
//! Limits mirror the jevmodel.org validator (docs, 2026-09-24) so a bad
//! request is refused here with a readable message instead of a 422.

use std::fmt;

/// Jev: `instructions` is at most 1,800 characters.
pub const MAX_QUESTION_CHARS: usize = 1_800;
/// Jev: serialized `state` is at most 8,000 characters.
pub const MAX_STATE_CHARS: usize = 8_000;
/// Jev: serialized `criteria` is at most 2,000 characters per question.
pub const MAX_CRITERIA_CHARS: usize = 2_000;
/// Our own cap per yes/no description.
pub const MAX_MEANING_CHARS: usize = 500;
/// What the jevmodel.org playground sends for a side left undescribed.
pub const DEFAULT_YES: &str = "Yes";
pub const DEFAULT_NO: &str = "No";

/// A validated yes/no question. `yes_means`/`no_means` become Jev's
/// optional `criteria`; both are `None` when the user described neither.
#[derive(Debug, Clone, PartialEq)]
pub struct NoulRequest {
    pub state: String,
    pub question: String,
    pub yes_means: Option<String>,
    pub no_means: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputError {
    EmptyState,
    StateTooLong(usize),
    EmptyQuestion,
    QuestionTooLong(usize),
    /// Which side ("yes" or "no") is over [`MAX_MEANING_CHARS`].
    MeaningTooLong(&'static str),
    CriteriaTooLong(usize),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputError::EmptyState => write!(f, "The text to judge is empty."),
            InputError::StateTooLong(n) => {
                write!(f, "The text to judge is {n} characters once encoded; the limit is {MAX_STATE_CHARS}.")
            }
            InputError::EmptyQuestion => write!(f, "The question is empty."),
            InputError::QuestionTooLong(n) => {
                write!(f, "The question is {n} characters; the limit is {MAX_QUESTION_CHARS}.")
            }
            InputError::MeaningTooLong(side) => {
                write!(f, "The description of \"{side}\" is longer than {MAX_MEANING_CHARS} characters.")
            }
            InputError::CriteriaTooLong(n) => {
                write!(f, "The yes/no descriptions are {n} characters once encoded; the limit is {MAX_CRITERIA_CHARS}.")
            }
        }
    }
}

impl std::error::Error for InputError {}

impl NoulRequest {
    pub fn parse(state: &str, question: &str, yes_means: Option<&str>, no_means: Option<&str>) -> Result<Self, InputError> {
        let state = state.trim();
        if state.is_empty() {
            return Err(InputError::EmptyState);
        }
        let question = question.trim();
        if question.is_empty() {
            return Err(InputError::EmptyQuestion);
        }
        let qlen = question.chars().count();
        if qlen > MAX_QUESTION_CHARS {
            return Err(InputError::QuestionTooLong(qlen));
        }
        let meaning = |v: Option<&str>, side: &'static str| -> Result<Option<String>, InputError> {
            match v.map(str::trim).filter(|m| !m.is_empty()) {
                Some(m) if m.chars().count() > MAX_MEANING_CHARS => Err(InputError::MeaningTooLong(side)),
                m => Ok(m.map(str::to_string)),
            }
        };
        let req = NoulRequest {
            state: state.to_string(),
            question: question.to_string(),
            yes_means: meaning(yes_means, "yes")?,
            no_means: meaning(no_means, "no")?,
        };

        let state_len = serde_json::to_string(&req.state).map(|s| s.chars().count()).unwrap_or(0);
        if state_len > MAX_STATE_CHARS {
            return Err(InputError::StateTooLong(state_len));
        }
        if let Some(c) = req.criteria() {
            let crit_len = serde_json::to_string(&c).map(|s| s.chars().count()).unwrap_or(0);
            if crit_len > MAX_CRITERIA_CHARS {
                return Err(InputError::CriteriaTooLong(crit_len));
            }
        }
        Ok(req)
    }

    /// Jev `criteria` for a noul: `{"true": .., "false": ..}`, or `None`
    /// to omit it. A side the user left blank gets the playground's default.
    pub fn criteria(&self) -> Option<serde_json::Value> {
        if self.yes_means.is_none() && self.no_means.is_none() {
            return None;
        }
        Some(serde_json::json!({
            "true": self.yes_means.as_deref().unwrap_or(DEFAULT_YES),
            "false": self.no_means.as_deref().unwrap_or(DEFAULT_NO),
        }))
    }
}
