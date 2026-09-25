//! `/jev noul`: ask Jev for the probability that a yes/no question is "yes".
//!
//! The whole flow, in order:
//! 1. [`input`] turns the raw slash-command strings into a bounded,
//!    validated [`NoulRequest`] (text, one yes/no question, optional
//!    descriptions of what yes and no mean) - or an [`InputError`] the user
//!    sees, with no call made.
//! 2. [`jev`] sends exactly one `POST /v1/systemone` with a single `noul`
//!    question and checks the typed answer: one finite P(yes) in `[0, 1]`.
//! 3. [`render`] reports that P(yes) as-is. There is deliberately no yes/no
//!    verdict and no confidence: Jev returns neither, and the action
//!    threshold belongs to whoever reads the number.
//!
//! Self-contained on purpose: shares no code with `choice/` or `score/`.
//! Nothing here knows Discord.

pub mod input;
pub mod jev;
pub mod render;

// The bot and the tests each use a different subset of these.
#[allow(unused_imports)]
pub use input::{InputError, NoulRequest};
#[allow(unused_imports)]
pub use jev::{NoulClient, NoulError, NoulOutcome};
