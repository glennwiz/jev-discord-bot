//! `/jev choice`: ask Jev to pick one of the user's options.
//!
//! The whole flow, in order:
//! 1. [`input`] turns the raw slash-command strings into a bounded,
//!    validated [`ChoiceRequest`] (2-20 distinct, non-empty options) - or an
//!    [`InputError`] the user sees, without any network call.
//! 2. [`jev`] sends exactly one `POST /v1/systemone` with a single `choice`
//!    question and checks the typed answer: the picked option must be one of
//!    ours, and probability and confidence must be numbers in `[0, 1]`.
//! 3. [`render`] formats the [`ChoiceOutcome`] (or the error) as the Discord
//!    reply, showing the picked option's probability and Jev's confidence as
//!    two separate numbers.
//!
//! Nothing here knows about Discord; `main.rs` owns the Gateway glue.

pub mod input;
pub mod jev;
pub mod render;

pub use input::{ChoiceRequest, InputError};
pub use jev::{ChoiceOutcome, JevClient, JevError};
