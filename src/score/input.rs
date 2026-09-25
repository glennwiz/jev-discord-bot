//! Parse and bound the user's `/jev score` input before anything is sent.
//!
//! Limits mirror the jevmodel.org validator (docs, 2026-09-24) so a bad
//! request is refused here with a readable message instead of a 422.

use std::fmt;

/// Jev accepts an ordered array of 2-10 levels for a `score` question.
pub const MIN_LEVELS: usize = 2;
pub const MAX_LEVELS: usize = 10;
/// Our own cap per level label; keeps the Discord legend short.
pub const MAX_LEVEL_CHARS: usize = 100;
/// Jev: `instructions` is at most 1,800 characters.
pub const MAX_QUESTION_CHARS: usize = 1_800;
/// Jev: serialized `state` is at most 8,000 characters.
pub const MAX_STATE_CHARS: usize = 8_000;
/// Jev: serialized `criteria` is at most 2,000 characters per question.
pub const MAX_CRITERIA_CHARS: usize = 2_000;

/// A validated score request. `levels` is ordered lowest first; level `i`
/// is score `i` on Jev's scale.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreRequest {
    pub state: String,
    pub question: String,
    pub levels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputError {
    EmptyState,
    StateTooLong(usize),
    EmptyQuestion,
    QuestionTooLong(usize),
    TooFewLevels(usize),
    TooManyLevels(usize),
    /// 1-based position of the blank level, as the user typed it.
    EmptyLevel(usize),
    LevelTooLong(usize),
    DuplicateLevel(String),
    CriteriaTooLong(usize),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputError::EmptyState => write!(f, "The text to score is empty."),
            InputError::StateTooLong(n) => {
                write!(f, "The text to score is {n} characters once encoded; the limit is {MAX_STATE_CHARS}.")
            }
            InputError::EmptyQuestion => write!(f, "The question is empty."),
            InputError::QuestionTooLong(n) => {
                write!(f, "The question is {n} characters; the limit is {MAX_QUESTION_CHARS}.")
            }
            InputError::TooFewLevels(n) => {
                write!(f, "Give at least {MIN_LEVELS} levels, lowest first, separated by commas (got {n}).")
            }
            InputError::TooManyLevels(n) => write!(f, "At most {MAX_LEVELS} levels are allowed (got {n})."),
            InputError::EmptyLevel(pos) => write!(f, "Level {pos} is empty."),
            InputError::LevelTooLong(pos) => {
                write!(f, "Level {pos} is longer than {MAX_LEVEL_CHARS} characters.")
            }
            InputError::DuplicateLevel(l) => write!(f, "Level \"{l}\" is listed more than once."),
            InputError::CriteriaTooLong(n) => {
                write!(f, "The levels are {n} characters once encoded; the limit is {MAX_CRITERIA_CHARS}.")
            }
        }
    }
}

impl std::error::Error for InputError {}

/// Split the raw levels string. `|` separates when present (so levels may
/// contain commas), otherwise `,`. Items are trimmed; blanks are kept so the
/// caller can report them by position.
pub fn split_levels(raw: &str) -> Vec<String> {
    let sep = if raw.contains('|') { '|' } else { ',' };
    raw.split(sep).map(|s| s.trim().to_string()).collect()
}

impl ScoreRequest {
    pub fn parse(state: &str, question: &str, levels: &str) -> Result<Self, InputError> {
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

        let mut lv = split_levels(levels);
        // A single trailing separator ("low, high,") is a typo, not a blank level.
        if lv.len() > 1 && lv.last().is_some_and(|l| l.is_empty()) {
            lv.pop();
        }
        if lv.len() == 1 && lv[0].is_empty() {
            return Err(InputError::TooFewLevels(0));
        }
        if let Some(pos) = lv.iter().position(|l| l.is_empty()) {
            return Err(InputError::EmptyLevel(pos + 1));
        }
        if lv.len() < MIN_LEVELS {
            return Err(InputError::TooFewLevels(lv.len()));
        }
        if lv.len() > MAX_LEVELS {
            return Err(InputError::TooManyLevels(lv.len()));
        }
        if let Some(pos) = lv.iter().position(|l| l.chars().count() > MAX_LEVEL_CHARS) {
            return Err(InputError::LevelTooLong(pos + 1));
        }
        // Two levels with one name make the scale ambiguous to read back.
        for (i, l) in lv.iter().enumerate() {
            let lower = l.to_lowercase();
            if lv[..i].iter().any(|p| p.to_lowercase() == lower) {
                return Err(InputError::DuplicateLevel(l.clone()));
            }
        }

        let req = ScoreRequest { state: state.to_string(), question: question.to_string(), levels: lv };
        let state_len = serde_json::to_string(&req.state).map(|s| s.chars().count()).unwrap_or(0);
        if state_len > MAX_STATE_CHARS {
            return Err(InputError::StateTooLong(state_len));
        }
        let crit_len = serde_json::to_string(&req.levels).map(|s| s.chars().count()).unwrap_or(0);
        if crit_len > MAX_CRITERIA_CHARS {
            return Err(InputError::CriteriaTooLong(crit_len));
        }
        Ok(req)
    }

    /// Highest score on Jev's 0-based scale.
    pub fn max_score(&self) -> usize {
        self.levels.len() - 1
    }
}
