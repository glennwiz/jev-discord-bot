//! Parse and bound the user's `/jev choice` input before anything is sent.
//!
//! Limits mirror the jevmodel.org validator (docs, 2026-09-24) so a bad
//! request is refused here with a readable message instead of a 422.

use std::fmt;

/// Jev accepts 2-20 option keys for a `choice` question.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 20;
/// Our own cap per option label; keeps the Discord reply short.
pub const MAX_OPTION_CHARS: usize = 100;
/// Jev: `instructions` is at most 1,800 characters.
pub const MAX_QUESTION_CHARS: usize = 1_800;
/// Jev: serialized `state` is at most 8,000 characters.
pub const MAX_STATE_CHARS: usize = 8_000;
/// Jev: serialized `criteria` is at most 2,000 characters per question.
pub const MAX_CRITERIA_CHARS: usize = 2_000;

/// A validated choice: what to decide, the distinct options, optional context.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceRequest {
    pub question: String,
    pub options: Vec<String>,
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputError {
    EmptyQuestion,
    QuestionTooLong(usize),
    TooFewOptions(usize),
    TooManyOptions(usize),
    /// 1-based position of the blank option.
    EmptyOption(usize),
    OptionTooLong(usize),
    DuplicateOption(String),
    StateTooLong(usize),
    CriteriaTooLong(usize),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputError::EmptyQuestion => write!(f, "The question is empty."),
            InputError::QuestionTooLong(n) => {
                write!(f, "The question is {n} characters; the limit is {MAX_QUESTION_CHARS}.")
            }
            InputError::TooFewOptions(n) => {
                write!(f, "Give at least {MIN_OPTIONS} options separated by commas (got {n}).")
            }
            InputError::TooManyOptions(n) => {
                write!(f, "At most {MAX_OPTIONS} options are allowed (got {n}).")
            }
            InputError::EmptyOption(pos) => write!(f, "Option {pos} is empty."),
            InputError::OptionTooLong(pos) => {
                write!(f, "Option {pos} is longer than {MAX_OPTION_CHARS} characters.")
            }
            InputError::DuplicateOption(o) => write!(f, "Option \"{o}\" is listed more than once."),
            InputError::StateTooLong(n) => {
                write!(f, "The context is {n} characters once encoded; the limit is {MAX_STATE_CHARS}.")
            }
            InputError::CriteriaTooLong(n) => {
                write!(f, "The options are {n} characters once encoded; the limit is {MAX_CRITERIA_CHARS}.")
            }
        }
    }
}

impl std::error::Error for InputError {}

/// Split the raw options string. `|` separates when present (so options may
/// contain commas), otherwise `,`. Items are trimmed; blanks are kept so the
/// caller can report them by position.
pub fn split_options(raw: &str) -> Vec<String> {
    let sep = if raw.contains('|') { '|' } else { ',' };
    raw.split(sep).map(|s| s.trim().to_string()).collect()
}

impl ChoiceRequest {
    pub fn parse(question: &str, options: &str, context: Option<&str>) -> Result<Self, InputError> {
        let question = question.trim();
        if question.is_empty() {
            return Err(InputError::EmptyQuestion);
        }
        let qlen = question.chars().count();
        if qlen > MAX_QUESTION_CHARS {
            return Err(InputError::QuestionTooLong(qlen));
        }

        let mut opts = split_options(options);
        // A single trailing separator ("a, b,") is a typo, not a blank option.
        if opts.len() > 1 && opts.last().is_some_and(|o| o.is_empty()) {
            opts.pop();
        }
        if opts.len() == 1 && opts[0].is_empty() {
            return Err(InputError::TooFewOptions(0));
        }
        if let Some(pos) = opts.iter().position(|o| o.is_empty()) {
            return Err(InputError::EmptyOption(pos + 1));
        }
        if opts.len() < MIN_OPTIONS {
            return Err(InputError::TooFewOptions(opts.len()));
        }
        if opts.len() > MAX_OPTIONS {
            return Err(InputError::TooManyOptions(opts.len()));
        }
        if let Some(pos) = opts.iter().position(|o| o.chars().count() > MAX_OPTION_CHARS) {
            return Err(InputError::OptionTooLong(pos + 1));
        }
        // Options become JSON object keys, so duplicates would collapse;
        // compare case-insensitively since "Pizza" and "pizza" are one choice.
        for (i, o) in opts.iter().enumerate() {
            let lower = o.to_lowercase();
            if opts[..i].iter().any(|p| p.to_lowercase() == lower) {
                return Err(InputError::DuplicateOption(o.clone()));
            }
        }

        let context = context.map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
        let req = ChoiceRequest { question: question.to_string(), options: opts, context };

        let state_len = serde_json::to_string(&req.state()).map(|s| s.chars().count()).unwrap_or(0);
        if state_len > MAX_STATE_CHARS {
            return Err(InputError::StateTooLong(state_len));
        }
        let crit_len = serde_json::to_string(&req.criteria()).map(|s| s.chars().count()).unwrap_or(0);
        if crit_len > MAX_CRITERIA_CHARS {
            return Err(InputError::CriteriaTooLong(crit_len));
        }
        Ok(req)
    }

    /// The text Jev evaluates: the user's context, or the question itself.
    pub fn state(&self) -> &str {
        self.context.as_deref().unwrap_or(&self.question)
    }

    /// Jev `criteria` for a choice: option key -> description. The label is
    /// both, so the returned `choice` maps straight back to the user's text.
    pub fn criteria(&self) -> serde_json::Map<String, serde_json::Value> {
        self.options
            .iter()
            .map(|o| (o.clone(), serde_json::Value::String(o.clone())))
            .collect()
    }
}
