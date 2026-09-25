//! One bounded call to the TypeSafe System One API for a score.
//!
//! Wire contract: https://docs.typesafe.ai/api (read 2026-09-26), confirmed
//! by a live call on 2026-09-26 (jev-1.13.0):
//!
//! request  `POST https://api.typesafe.ai/v1/systemone`, `Authorization: Bearer <key>`,
//!          `{"model":"jev-latest","state":..,"questions":{"score":{"type":"score",
//!           "instructions":..,"criteria":["<lowest>",..,"<highest>"]}}}` (2-10 levels).
//! 200      `{"answers":{"score":{"type":"score","score":3.0,"confidence":1.0,
//!           "legend":{"0":"<lowest>",..},"probabilities":{"0":p,..}}},..}`
//! scale    0-based: level `i` of `criteria` is score `i`, so a score lies in
//!          `0..=n-1` and may be fractional. TypeSafe's reference example
//!          and the live call (legend keys "0".."3", score 3.0 for a clearly
//!          top-level state) agree. The base is the single constant
//!          `input::LOWEST_LEVEL_SCORE`.
//! error    401/403/422/429/529 with `{"detail":{"error_type":..,"message":..}}`
//!          (see [`error_details`] for every shape we read).
//!
//! The transport mirrors `choice::jev` line for line rather than sharing it,
//! so the choice slice stays untouched.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::input::ScoreRequest;

pub const MODEL: &str = "jev-latest";
/// The single question name we send; answers are keyed by it.
pub const QUESTION_NAME: &str = "score";
/// Largest response body we read. A score answer is well under 4 KiB.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 100;

/// Jev's answer. `score` is unrounded on the level scale;
/// `confidence` is Jev's separate confidence field; `level_probabilities`
/// is the optional per-level distribution as (level score, p), sorted.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreOutcome {
    pub score: f64,
    pub confidence: f64,
    pub level_probabilities: Vec<(usize, f64)>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScoreError {
    /// No complete answer within the configured timeout.
    Timeout,
    /// Non-2xx; `kind`/`message` come from the provider's error body when present.
    Status {
        status: u16,
        kind: Option<String>,
        message: Option<String>,
    },
    /// 2xx whose body does not match the typed score contract.
    Malformed(String),
    /// Body larger than [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// Connection or protocol failure before a response.
    Transport(String),
}

impl fmt::Display for ScoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScoreError::Timeout => write!(f, "Jev did not answer in time."),
            ScoreError::Status {
                status,
                kind,
                message,
            } => {
                write!(f, "Jev returned HTTP {status}")?;
                if let Some(k) = kind {
                    write!(f, " ({k})")?;
                }
                if let Some(m) = message {
                    write!(f, ": {m}")?;
                }
                if matches!(status, 429 | 529) {
                    write!(f, " - Jev is busy, try again shortly")?;
                }
                Ok(())
            }
            ScoreError::Malformed(why) => write!(f, "Jev sent an unexpected answer: {why}"),
            ScoreError::TooLarge => write!(f, "Jev's response was too large."),
            ScoreError::Transport(why) => write!(f, "Could not reach Jev: {why}"),
        }
    }
}

impl std::error::Error for ScoreError {}

pub struct ScoreClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl fmt::Debug for ScoreClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScoreClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl ScoreClient {
    /// `timeout` bounds the whole call: connect, send, and reading the body.
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> Result<Self, ScoreError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ScoreError::Transport(e.without_url().to_string()))?;
        Ok(ScoreClient {
            http,
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
            api_key: api_key.to_string(),
        })
    }

    /// The exact JSON body sent for `req`.
    pub fn request_body(req: &ScoreRequest) -> serde_json::Value {
        json!({
            "model": MODEL,
            "state": req.state,
            "questions": {
                QUESTION_NAME: {
                    "type": "score",
                    "instructions": req.question,
                    "criteria": req.levels,
                }
            }
        })
    }

    /// Send one request (no retries) and validate the typed answer.
    pub async fn score(
        &self,
        req: &ScoreRequest,
        idempotency_key: Option<&str>,
    ) -> Result<ScoreOutcome, ScoreError> {
        let mut builder = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&Self::request_body(req));
        if let Some(key) = idempotency_key {
            let key: String = key.chars().take(MAX_IDEMPOTENCY_KEY_CHARS).collect();
            builder = builder.header("Idempotency-Key", key);
        }

        let mut resp = builder.send().await.map_err(map_reqwest)?;
        let status = resp.status();

        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(map_reqwest)? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(ScoreError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        if !status.is_success() {
            let (kind, message) = error_details(&body);
            return Err(ScoreError::Status {
                status: status.as_u16(),
                kind,
                message,
            });
        }
        parse_score(&body, req)
    }
}

/// Validate a 200 body against the score contract for `req`.
pub fn parse_score(body: &[u8], req: &ScoreRequest) -> Result<ScoreOutcome, ScoreError> {
    let wire: WireResponse = serde_json::from_slice(body)
        .map_err(|e| ScoreError::Malformed(format!("not a Jev response ({e})")))?;
    let answer = wire
        .answers
        .get(QUESTION_NAME)
        .ok_or_else(|| ScoreError::Malformed(format!("no answer for \"{QUESTION_NAME}\"")))?;
    if answer.kind.as_deref() != Some("score") {
        return Err(ScoreError::Malformed(format!(
            "answer type is {:?}, expected \"score\"",
            answer.kind
        )));
    }
    let (min, max) = (req.min_score(), req.max_score());
    let score = answer
        .score
        .ok_or_else(|| ScoreError::Malformed("no score".into()))?;
    if !(score.is_finite() && (min as f64..=max as f64).contains(&score)) {
        return Err(ScoreError::Malformed(format!(
            "score {score} is outside {min}-{max}"
        )));
    }
    let confidence = answer
        .confidence
        .ok_or_else(|| ScoreError::Malformed("no confidence".into()))?;
    if !(confidence.is_finite() && (0.0..=1.0).contains(&confidence)) {
        return Err(ScoreError::Malformed(format!(
            "confidence {confidence} is outside [0, 1]"
        )));
    }

    let mut level_probabilities = Vec::new();
    for (key, p) in answer.probabilities.iter().flatten() {
        let level: usize = key
            .parse()
            .ok()
            .filter(|l| (min..=max).contains(l))
            .ok_or_else(|| {
                ScoreError::Malformed(format!("probability for unknown level \"{key}\""))
            })?;
        if !(p.is_finite() && (0.0..=1.0).contains(p)) {
            return Err(ScoreError::Malformed(format!(
                "level {level} probability {p} is outside [0, 1]"
            )));
        }
        level_probabilities.push((level, *p));
    }
    level_probabilities.sort_by_key(|(l, _)| *l);

    Ok(ScoreOutcome {
        score,
        confidence,
        level_probabilities,
        model: wire.model,
        input_tokens: wire.usage.and_then(|u| u.input_tokens),
    })
}

fn map_reqwest(e: reqwest::Error) -> ScoreError {
    if e.is_timeout() {
        ScoreError::Timeout
    } else {
        ScoreError::Transport(e.without_url().to_string())
    }
}

#[derive(Deserialize)]
struct WireResponse {
    model: Option<String>,
    answers: HashMap<String, WireAnswer>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireAnswer {
    #[serde(rename = "type")]
    kind: Option<String>,
    score: Option<f64>,
    confidence: Option<f64>,
    probabilities: Option<HashMap<String, f64>>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: Option<u64>,
}

/// Longest provider error message passed on to Discord.
const MAX_ERROR_MESSAGE_CHARS: usize = 300;

/// `(kind, message)` from a non-2xx body, bounded. TypeSafe sends
/// `{"detail":{"error_type":..,"message":..}}`, a bare `{"detail":".."}`, or
/// for some 422s `{"detail":[{"msg":..},..]}`; OpenAI-style gateways send
/// `{"error":{"type":..,"message":..}}`. Anything else gives `(None, None)`.
fn error_details(body: &[u8]) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (None, None);
    };
    let text = |v: Option<&serde_json::Value>| v.and_then(|s| s.as_str()).map(bounded);
    if let Some(e) = v.get("error") {
        return (text(e.get("type")), text(e.get("message")));
    }
    match v.get("detail") {
        Some(d @ serde_json::Value::String(_)) => (None, text(Some(d))),
        Some(serde_json::Value::Array(items)) => {
            (None, text(items.first().and_then(|i| i.get("msg"))))
        }
        Some(d) => (text(d.get("error_type")), text(d.get("message"))),
        None => (None, None),
    }
}

fn bounded(s: &str) -> String {
    if s.chars().count() <= MAX_ERROR_MESSAGE_CHARS {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(MAX_ERROR_MESSAGE_CHARS - 1).collect();
        t.push('…');
        t
    }
}
