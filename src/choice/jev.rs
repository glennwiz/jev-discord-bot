//! One bounded call to the TypeSafe System One API for a choice.
//!
//! Wire contract: https://docs.typesafe.ai/api (read 2026-09-26), confirmed
//! by a live call on 2026-09-26 (jev-1.13.0):
//!
//! request  `POST https://api.typesafe.ai/v1/systemone`, `Authorization: Bearer <key>`,
//!          `{"model":"jev-latest","state":..,"questions":{"pick":{"type":"choice",
//!           "instructions":..,"criteria":{"<key>":"<description>",..}}}}`.
//!          We also send `Idempotency-Key` (<=100 chars); TypeSafe accepts it.
//! 200      `{"model":"jev-1.13.0","answers":{"pick":{"type":"choice","choice":"<key>",
//!           "probabilities":{"<key>":p,..},"confidence":c}},"usage":{"input_tokens":n,..}}`
//! error    401/403/422/429/529 with `{"detail":{"error_type":..,"message":..}}`
//!          (see [`error_details`] for every shape we read).

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::input::ChoiceRequest;

pub const MODEL: &str = "jev-latest";
/// The single question name we send; answers are keyed by it.
pub const QUESTION_NAME: &str = "pick";
/// Largest response body we read. A choice answer is well under 4 KiB.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 100;

/// What Jev picked. `probability` is P(picked option) from the
/// `probabilities` map; `confidence` is Jev's separate confidence field.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceOutcome {
    pub choice: String,
    pub probability: f64,
    pub confidence: f64,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JevError {
    /// No complete answer within the configured timeout.
    Timeout,
    /// Non-2xx; `kind`/`message` come from the provider's error body when present.
    Status {
        status: u16,
        kind: Option<String>,
        message: Option<String>,
    },
    /// 2xx whose body does not match the typed choice contract.
    Malformed(String),
    /// Body larger than [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// Connection or protocol failure before a response.
    Transport(String),
}

impl fmt::Display for JevError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JevError::Timeout => write!(f, "Jev did not answer in time."),
            JevError::Status {
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
            JevError::Malformed(why) => write!(f, "Jev sent an unexpected answer: {why}"),
            JevError::TooLarge => write!(f, "Jev's response was too large."),
            JevError::Transport(why) => write!(f, "Could not reach Jev: {why}"),
        }
    }
}

impl std::error::Error for JevError {}

pub struct JevClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl fmt::Debug for JevClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JevClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl JevClient {
    /// `timeout` bounds the whole call: connect, send, and reading the body.
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> Result<Self, JevError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| JevError::Transport(e.without_url().to_string()))?;
        Ok(JevClient {
            http,
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
            api_key: api_key.to_string(),
        })
    }

    /// The exact JSON body sent for `req`.
    pub fn request_body(req: &ChoiceRequest) -> serde_json::Value {
        json!({
            "model": MODEL,
            "state": req.state(),
            "questions": {
                QUESTION_NAME: {
                    "type": "choice",
                    "instructions": req.question,
                    "criteria": req.criteria(),
                }
            }
        })
    }

    /// Send one request (no retries) and validate the typed answer.
    pub async fn choose(
        &self,
        req: &ChoiceRequest,
        idempotency_key: Option<&str>,
    ) -> Result<ChoiceOutcome, JevError> {
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
                return Err(JevError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        if !status.is_success() {
            let (kind, message) = error_details(&body);
            return Err(JevError::Status {
                status: status.as_u16(),
                kind,
                message,
            });
        }
        parse_choice(&body, req)
    }
}

/// Validate a 200 body against the choice contract for `req`.
pub fn parse_choice(body: &[u8], req: &ChoiceRequest) -> Result<ChoiceOutcome, JevError> {
    let wire: WireResponse = serde_json::from_slice(body)
        .map_err(|e| JevError::Malformed(format!("not a Jev response ({e})")))?;
    let answer = wire
        .answers
        .get(QUESTION_NAME)
        .ok_or_else(|| JevError::Malformed(format!("no answer for \"{QUESTION_NAME}\"")))?;
    if answer.kind.as_deref() != Some("choice") {
        return Err(JevError::Malformed(format!(
            "answer type is {:?}, expected \"choice\"",
            answer.kind
        )));
    }
    let choice = answer
        .choice
        .clone()
        .ok_or_else(|| JevError::Malformed("no choice".into()))?;
    if !req.options.contains(&choice) {
        return Err(JevError::Malformed(format!(
            "picked \"{choice}\", which is not one of the options"
        )));
    }
    let probabilities = answer
        .probabilities
        .as_ref()
        .ok_or_else(|| JevError::Malformed("no probabilities".into()))?;
    for (key, p) in probabilities {
        if !req.options.contains(key) {
            return Err(JevError::Malformed(format!(
                "probability for unknown option \"{key}\""
            )));
        }
        check_unit("probability", *p)?;
    }
    let probability = *probabilities.get(&choice).ok_or_else(|| {
        JevError::Malformed(format!("no probability for picked option \"{choice}\""))
    })?;
    let confidence = answer
        .confidence
        .ok_or_else(|| JevError::Malformed("no confidence".into()))?;
    check_unit("confidence", confidence)?;

    Ok(ChoiceOutcome {
        choice,
        probability,
        confidence,
        model: wire.model,
        input_tokens: wire.usage.and_then(|u| u.input_tokens),
    })
}

fn check_unit(what: &str, v: f64) -> Result<(), JevError> {
    if v.is_finite() && (0.0..=1.0).contains(&v) {
        Ok(())
    } else {
        Err(JevError::Malformed(format!("{what} {v} is outside [0, 1]")))
    }
}

fn map_reqwest(e: reqwest::Error) -> JevError {
    if e.is_timeout() {
        JevError::Timeout
    } else {
        JevError::Transport(e.without_url().to_string())
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
    choice: Option<String>,
    probabilities: Option<HashMap<String, f64>>,
    confidence: Option<f64>,
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
