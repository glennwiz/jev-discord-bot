//! One bounded call to jevmodel.org `POST /v1/systemone` for a noul.
//!
//! Wire contract (https://jevmodel.org/docs updated 2026-09-24, plus the
//! jevmodel.org playground's request builder, read 2026-09-25):
//!
//! request  `{"model":"jev-latest","state":..,"questions":{"noul":{"type":"noul",
//!           "instructions":..}}}` with optional
//!          `"criteria":{"true":"<what yes means>","false":"<what no means>"}`.
//! 200      `{"answers":{"noul":{"type":"noul","noul":0.12}},..}` - `noul` is
//!          P(yes) in `[0, 1]`. Docs: "There is no separate confidence field;
//!          the probability is the certainty measure."
//! error    `{"error":{"type":"..","message":".."}}` with 401/402/422/429/502.
//!
//! The transport mirrors `choice::jev` line for line rather than sharing it,
//! so each feature stays readable on its own.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::input::NoulRequest;

pub const MODEL: &str = "jev-latest";
/// The single question name we send; answers are keyed by it.
pub const QUESTION_NAME: &str = "noul";
/// Largest response body we read. A noul answer is well under 1 KiB.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 100;

/// Jev's answer: P(yes) exactly as sent. No verdict, no confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct NoulOutcome {
    pub p_yes: f64,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NoulError {
    /// No complete answer within the configured timeout.
    Timeout,
    /// Non-2xx; `kind`/`message` come from Jev's error envelope when present.
    Status {
        status: u16,
        kind: Option<String>,
        message: Option<String>,
    },
    /// 2xx whose body does not match the typed noul contract.
    Malformed(String),
    /// Body larger than [`MAX_RESPONSE_BYTES`].
    TooLarge,
    /// Connection or protocol failure before a response.
    Transport(String),
}

impl fmt::Display for NoulError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoulError::Timeout => write!(f, "Jev did not answer in time."),
            NoulError::Status {
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
                Ok(())
            }
            NoulError::Malformed(why) => write!(f, "Jev sent an unexpected answer: {why}"),
            NoulError::TooLarge => write!(f, "Jev's response was too large."),
            NoulError::Transport(why) => write!(f, "Could not reach Jev: {why}"),
        }
    }
}

impl std::error::Error for NoulError {}

pub struct NoulClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl fmt::Debug for NoulClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoulClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl NoulClient {
    /// `timeout` bounds the whole call: connect, send, and reading the body.
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> Result<Self, NoulError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| NoulError::Transport(e.without_url().to_string()))?;
        Ok(NoulClient {
            http,
            endpoint: format!("{}/v1/systemone", base_url.trim_end_matches('/')),
            api_key: api_key.to_string(),
        })
    }

    /// The exact JSON body sent for `req`.
    pub fn request_body(req: &NoulRequest) -> serde_json::Value {
        let mut question = json!({"type": "noul", "instructions": req.question});
        if let Some(criteria) = req.criteria() {
            question["criteria"] = criteria;
        }
        json!({
            "model": MODEL,
            "state": req.state,
            "questions": { QUESTION_NAME: question }
        })
    }

    /// Send one request (no retries) and validate the typed answer.
    pub async fn ask(
        &self,
        req: &NoulRequest,
        idempotency_key: Option<&str>,
    ) -> Result<NoulOutcome, NoulError> {
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
                return Err(NoulError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        if !status.is_success() {
            let envelope = serde_json::from_slice::<ErrorEnvelope>(&body)
                .ok()
                .map(|e| e.error);
            return Err(NoulError::Status {
                status: status.as_u16(),
                kind: envelope.as_ref().and_then(|e| e.kind.clone()),
                message: envelope.and_then(|e| e.message),
            });
        }
        parse_noul(&body)
    }
}

/// Validate a 200 body against the noul contract.
pub fn parse_noul(body: &[u8]) -> Result<NoulOutcome, NoulError> {
    let wire: WireResponse = serde_json::from_slice(body)
        .map_err(|e| NoulError::Malformed(format!("not a Jev response ({e})")))?;
    let answer = wire
        .answers
        .get(QUESTION_NAME)
        .ok_or_else(|| NoulError::Malformed(format!("no answer for \"{QUESTION_NAME}\"")))?;
    if answer.kind.as_deref() != Some("noul") {
        return Err(NoulError::Malformed(format!(
            "answer type is {:?}, expected \"noul\"",
            answer.kind
        )));
    }
    let p_yes = answer
        .noul
        .ok_or_else(|| NoulError::Malformed("no noul probability".into()))?;
    if !(p_yes.is_finite() && (0.0..=1.0).contains(&p_yes)) {
        return Err(NoulError::Malformed(format!(
            "P(yes) {p_yes} is outside [0, 1]"
        )));
    }
    Ok(NoulOutcome {
        p_yes,
        model: wire.model,
        input_tokens: wire.usage.and_then(|u| u.input_tokens),
    })
}

fn map_reqwest(e: reqwest::Error) -> NoulError {
    if e.is_timeout() {
        NoulError::Timeout
    } else {
        NoulError::Transport(e.without_url().to_string())
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
    noul: Option<f64>,
}

#[derive(Deserialize)]
struct WireUsage {
    input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}
