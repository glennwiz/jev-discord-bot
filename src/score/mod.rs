//! `/jev score`: ask Jev where a state sits on the user's ordered levels.
//!
//! The whole flow, in order:
//! 1. [`input`] turns the raw slash-command strings into a bounded,
//!    validated [`ScoreRequest`] (a state, a focused question, 2-10 distinct
//!    ordered levels) - or an [`InputError`] the user sees, with no call made.
//! 2. [`jev`] sends exactly one `POST /v1/systemone` with a single `score`
//!    question and checks the typed answer: a finite score on the level
//!    scale (base: `input::LOWEST_LEVEL_SCORE`) and a confidence in `[0, 1]`.
//! 3. [`render`] shows the unrounded score, the ordered legend with the
//!    same level numbers, and Jev's confidence as a separate figure.
//!
//! Self-contained on purpose: it shares no code with `choice/`, so the
//! approved choice slice stays byte-identical. Nothing here knows Discord.

pub mod input;
pub mod jev;
pub mod render;

// The bot and the tests each use a different subset of these.
#[allow(unused_imports)]
pub use input::{InputError, ScoreRequest};
#[allow(unused_imports)]
pub use jev::{ScoreClient, ScoreError, ScoreOutcome};
