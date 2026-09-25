//! Offline tests for `/jev choice` against a fake Jev HTTP server on
//! 127.0.0.1. No Discord, internet, real key or paid call is involved.

#[path = "../src/choice/mod.rs"]
#[allow(dead_code)]
mod choice;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use choice::{render, ChoiceRequest, InputError, JevClient, JevError};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FAKE_KEY: &str = "sk-test-not-a-real-key";

/// One request as the fake server saw it.
#[derive(Debug, Clone)]
struct Seen {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

enum Reply {
    Json(u16, String),
    /// Accept the request, then say nothing for this long.
    Stall(Duration),
}

/// Serve `reply` to every connection; returns base URL and the request log.
async fn fake_jev(reply: Reply) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let log = log.clone();
            let reply = reply.clone();
            tokio::spawn(async move {
                let Some(req) = read_request(&mut sock).await else { return };
                log.lock().unwrap().push(req);
                match &*reply {
                    Reply::Stall(d) => tokio::time::sleep(*d).await,
                    Reply::Json(status, body) => {
                        let resp = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                        let _ = sock.shutdown().await;
                    }
                }
            });
        }
    });
    (base, seen)
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(Value::Null);
    Some(Seen { request_line, headers, body })
}

fn lunch() -> ChoiceRequest {
    ChoiceRequest::parse("Where should the team eat?", "pizza, sushi, tacos", None).unwrap()
}

fn client(base: &str, timeout_ms: u64) -> JevClient {
    JevClient::new(base, FAKE_KEY, Duration::from_millis(timeout_ms)).unwrap()
}

fn ok_body(answer: Value) -> String {
    json!({"model": "jev-latest", "answers": {"pick": answer}, "usage": {"input_tokens": 57, "output_tokens": 9}})
        .to_string()
}

fn sushi_answer() -> Value {
    json!({"type": "choice", "choice": "sushi",
           "probabilities": {"pizza": 0.2, "sushi": 0.7, "tacos": 0.1}, "confidence": 0.55})
}

// ---- valid choice -------------------------------------------------------

#[tokio::test]
async fn valid_choice_sends_contract_request_once_and_returns_typed_outcome() {
    let (base, seen) = fake_jev(Reply::Json(200, ok_body(sushi_answer()))).await;
    let req = lunch();
    let out = client(&base, 2_000).choose(&req, Some("discord-123")).await.unwrap();

    assert_eq!(out.choice, "sushi");
    assert_eq!(out.probability, 0.7, "probability is P(picked option) from the map");
    assert_eq!(out.confidence, 0.55, "confidence is the separate field");
    assert_eq!(out.input_tokens, Some(57));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one call to Jev");
    let r = &seen[0];
    assert_eq!(r.request_line, "POST /v1/systemone HTTP/1.1");
    assert_eq!(r.header("authorization"), Some(format!("Bearer {FAKE_KEY}").as_str()));
    assert_eq!(r.header("idempotency-key"), Some("discord-123"));
    assert!(r.header("content-type").unwrap().starts_with("application/json"));
    assert_eq!(
        r.body,
        json!({
            "model": "jev-latest",
            "state": "Where should the team eat?",
            "questions": {"pick": {
                "type": "choice",
                "instructions": "Where should the team eat?",
                "criteria": {"pizza": "pizza", "sushi": "sushi", "tacos": "tacos"}
            }}
        })
    );
    // Option order is preserved on the wire.
    let keys: Vec<_> = r.body["questions"]["pick"]["criteria"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["pizza", "sushi", "tacos"]);

    let text = render::outcome(&req, &out);
    assert!(text.contains("**Jev picks:** sushi"), "{text}");
    assert!(text.contains("Probability of this option: 70.0%"), "{text}");
    assert!(text.contains("Jev confidence: 55.0%"), "{text}");
}

#[tokio::test]
async fn context_becomes_state() {
    let (base, seen) = fake_jev(Reply::Json(200, ok_body(sushi_answer()))).await;
    let req = ChoiceRequest::parse("Where to eat?", "pizza|sushi|tacos", Some("  Two of us are vegetarian. ")).unwrap();
    client(&base, 2_000).choose(&req, None).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].body["state"], "Two of us are vegetarian.");
    assert_eq!(seen[0].header("idempotency-key"), None);
}

// ---- empty / duplicate options: refused before any call -----------------

#[test]
fn empty_and_duplicate_options_are_refused_locally() {
    let p = |o: &str| ChoiceRequest::parse("Pick one", o, None);
    assert_eq!(p(""), Err(InputError::TooFewOptions(0)));
    assert_eq!(p("   "), Err(InputError::TooFewOptions(0)));
    assert_eq!(p("pizza"), Err(InputError::TooFewOptions(1)));
    assert_eq!(p("pizza, , sushi"), Err(InputError::EmptyOption(2)));
    assert_eq!(p(",pizza,sushi"), Err(InputError::EmptyOption(1)));
    assert_eq!(p("pizza, sushi, Pizza"), Err(InputError::DuplicateOption("Pizza".into())));
    assert_eq!(p("a|a"), Err(InputError::DuplicateOption("a".into())));
    assert_eq!(ChoiceRequest::parse("   ", "a, b", None), Err(InputError::EmptyQuestion));
    // One trailing separator is tolerated.
    assert_eq!(p("pizza, sushi,").unwrap().options, ["pizza", "sushi"]);
}

#[test]
fn input_bounds_follow_jev_limits() {
    let many: Vec<String> = (1..=21).map(|i| format!("o{i}")).collect();
    assert_eq!(ChoiceRequest::parse("q", &many.join(","), None), Err(InputError::TooManyOptions(21)));
    assert!(ChoiceRequest::parse("q", &many[..20].join(","), None).is_ok());
    let long_q = "x".repeat(1_801);
    assert_eq!(ChoiceRequest::parse(&long_q, "a,b", None), Err(InputError::QuestionTooLong(1_801)));
    let long_opt = format!("a,{}", "y".repeat(101));
    assert_eq!(ChoiceRequest::parse("q", &long_opt, None), Err(InputError::OptionTooLong(2)));
    let long_ctx = "z".repeat(8_000); // 8,002 once JSON-quoted
    assert_eq!(ChoiceRequest::parse("q", "a,b", Some(&long_ctx)), Err(InputError::StateTooLong(8_002)));
    let wide: Vec<String> = (0..20).map(|i| format!("{i:02}{}", "w".repeat(60))).collect();
    assert!(matches!(ChoiceRequest::parse("q", &wide.join(","), None), Err(InputError::CriteriaTooLong(_))));
}

#[tokio::test]
async fn refused_input_makes_no_http_call() {
    // The bot only calls choose() with a parsed request; prove parse failing
    // leaves the server untouched (nothing else could reach it).
    let (_base, seen) = fake_jev(Reply::Json(200, ok_body(sushi_answer()))).await;
    assert!(ChoiceRequest::parse("q", "a, a", None).is_err());
    assert!(seen.lock().unwrap().is_empty());
    assert!(render::input_error(&InputError::DuplicateOption("a".into())).contains("more than once"));
}

// ---- timeout ------------------------------------------------------------

#[tokio::test]
async fn timeout_is_bounded_and_reported() {
    let (base, seen) = fake_jev(Reply::Stall(Duration::from_secs(10))).await;
    let started = std::time::Instant::now();
    let err = client(&base, 300).choose(&lunch(), None).await.unwrap_err();
    assert_eq!(err, JevError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(seen.lock().unwrap().len(), 1, "no retry after timeout");
    assert!(render::jev_error(&err).contains("did not answer in time"));
}

// ---- non-2xx ------------------------------------------------------------

#[tokio::test]
async fn non_2xx_surfaces_status_and_error_envelope_without_retry() {
    for (status, kind) in [
        (401, "authentication_error"),
        (402, "insufficient_credits"),
        (422, "invalid_request_error"),
        (429, "rate_limit_error"),
        (502, "upstream_error"),
    ] {
        let body = json!({"error": {"type": kind, "message": "nope"}}).to_string();
        let (base, seen) = fake_jev(Reply::Json(status, body)).await;
        let err = client(&base, 2_000).choose(&lunch(), None).await.unwrap_err();
        assert_eq!(
            err,
            JevError::Status { status, kind: Some(kind.into()), message: Some("nope".into()) },
            "status {status}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "no retry on {status}");
        let text = render::jev_error(&err);
        assert!(text.contains(&status.to_string()) && !text.contains(FAKE_KEY), "{text}");
    }
}

#[tokio::test]
async fn non_2xx_without_envelope_still_reports_status() {
    let (base, _) = fake_jev(Reply::Json(503, "<html>down</html>".into())).await;
    let err = client(&base, 2_000).choose(&lunch(), None).await.unwrap_err();
    assert_eq!(err, JevError::Status { status: 503, kind: None, message: None });
}

// ---- malformed typed response ------------------------------------------

#[tokio::test]
async fn malformed_typed_responses_are_rejected() {
    // (case, body, substring the rejection reason must contain) - the reason
    // pins WHICH check fired, since several checks overlap.
    let cases: Vec<(&str, String, &str)> = vec![
        ("not json", "this is not json".into(), "not a Jev response"),
        ("no answers", json!({"model": "jev-latest"}).to_string(), "not a Jev response"),
        ("wrong question name", json!({"answers": {"route": sushi_answer()}}).to_string(), "no answer for \"pick\""),
        ("wrong type", ok_body(json!({"type": "score", "score": 1.4, "confidence": 0.8})), "expected \"choice\""),
        ("missing choice", ok_body(json!({"type": "choice", "probabilities": {"sushi": 0.7}, "confidence": 0.5})), "no choice"),
        ("choice not offered", ok_body(json!({"type": "choice", "choice": "ramen",
            "probabilities": {"sushi": 0.3, "ramen": 0.7}, "confidence": 0.5})), "not one of the options"),
        ("choice as number", ok_body(json!({"type": "choice", "choice": 2,
            "probabilities": {"sushi": 0.7}, "confidence": 0.5})), "not a Jev response"),
        ("missing probabilities", ok_body(json!({"type": "choice", "choice": "sushi", "confidence": 0.5})), "no probabilities"),
        ("no probability for choice", ok_body(json!({"type": "choice", "choice": "sushi",
            "probabilities": {"pizza": 0.7}, "confidence": 0.5})), "no probability for picked option"),
        ("unknown option in probabilities", ok_body(json!({"type": "choice", "choice": "sushi",
            "probabilities": {"sushi": 0.7, "ramen": 0.3}, "confidence": 0.5})), "unknown option \"ramen\""),
        ("probability out of range", ok_body(json!({"type": "choice", "choice": "sushi",
            "probabilities": {"sushi": 1.7}, "confidence": 0.5})), "probability 1.7 is outside"),
        ("probability as string", ok_body(json!({"type": "choice", "choice": "sushi",
            "probabilities": {"sushi": "0.7"}, "confidence": 0.5})), "not a Jev response"),
        ("missing confidence", ok_body(json!({"type": "choice", "choice": "sushi",
            "probabilities": {"sushi": 0.7}})), "no confidence"),
        ("negative confidence", ok_body(json!({"type": "choice", "choice": "sushi",
            "probabilities": {"sushi": 0.7}, "confidence": -0.1})), "confidence -0.1 is outside"),
    ];
    for (name, body, reason) in cases {
        let (base, _) = fake_jev(Reply::Json(200, body)).await;
        let err = client(&base, 2_000).choose(&lunch(), None).await.unwrap_err();
        match &err {
            JevError::Malformed(why) => assert!(why.contains(reason), "{name}: reason {why:?} lacks {reason:?}"),
            other => panic!("{name}: got {other:?}"),
        }
    }
}

#[tokio::test]
async fn oversized_response_is_refused() {
    let huge = format!("{{\"pad\":\"{}\"}}", "x".repeat(choice::jev::MAX_RESPONSE_BYTES + 10));
    let (base, _) = fake_jev(Reply::Json(200, huge)).await;
    let err = client(&base, 2_000).choose(&lunch(), None).await.unwrap_err();
    assert_eq!(err, JevError::TooLarge);
}

#[tokio::test]
async fn unreachable_server_is_a_transport_error() {
    // Bind then drop to get a port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let err = client(&format!("http://127.0.0.1:{port}"), 2_000).choose(&lunch(), None).await.unwrap_err();
    assert!(matches!(err, JevError::Transport(_)), "{err:?}");
}

#[test]
fn client_debug_redacts_key() {
    let c = JevClient::new("http://127.0.0.1:1", FAKE_KEY, Duration::from_secs(1)).unwrap();
    assert!(!format!("{c:?}").contains(FAKE_KEY));
}
