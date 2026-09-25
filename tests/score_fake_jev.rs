//! Offline tests for `/jev score` against a fake Jev HTTP server on
//! 127.0.0.1. No Discord, internet, real key or paid call is involved.

mod common;
#[path = "../src/score/mod.rs"]
#[allow(dead_code)]
mod score;

use std::time::Duration;

use common::{fake_jev, Reply};
use score::input::LOWEST_LEVEL_SCORE;
use score::{render, InputError, ScoreClient, ScoreError, ScoreRequest};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const FAKE_KEY: &str = "sk-test-not-a-real-key";

// Every expected score below is relative to the scale base, so flipping
// LOWEST_LEVEL_SCORE is a one-line change that keeps this suite meaningful.
const LO: usize = LOWEST_LEVEL_SCORE;

/// A score `offset` levels above the lowest one.
fn at(offset: f64) -> f64 {
    LO as f64 + offset
}

fn urgency() -> ScoreRequest {
    ScoreRequest::parse(
        "Billed twice, wants a refund today.",
        "How urgent is this for the customer?",
        "routine, soon, urgent, critical",
    )
    .unwrap()
}

fn client(base: &str, timeout_ms: u64) -> ScoreClient {
    ScoreClient::new(base, FAKE_KEY, Duration::from_millis(timeout_ms)).unwrap()
}

fn ok_body(answer: Value) -> String {
    json!({"model": "jev-latest", "answers": {"score": answer}, "usage": {"input_tokens": 61, "output_tokens": 4}})
        .to_string()
}

// ---- valid output -------------------------------------------------------

#[tokio::test]
async fn valid_score_sends_contract_request_once_and_keeps_fraction() {
    let body = ok_body(json!({"type": "score", "score": at(1.4), "confidence": 0.88}));
    let (base, seen) = fake_jev(Reply::Json(200, body)).await;
    let req = urgency();
    let out = client(&base, 2_000)
        .score(&req, Some("discord-456"))
        .await
        .unwrap();

    assert_eq!(out.score, at(1.4));
    assert_eq!(out.confidence, 0.88);
    assert!(out.level_probabilities.is_empty());
    assert_eq!(out.input_tokens, Some(61));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one call to Jev");
    let r = &seen[0];
    assert_eq!(r.request_line, "POST /v1/systemone HTTP/1.1");
    assert_eq!(
        r.header("authorization"),
        Some(format!("Bearer {FAKE_KEY}").as_str())
    );
    assert_eq!(r.header("idempotency-key"), Some("discord-456"));
    assert_eq!(
        r.body,
        json!({
            "model": "jev-latest",
            "state": "Billed twice, wants a refund today.",
            "questions": {"score": {
                "type": "score",
                "instructions": "How urgent is this for the customer?",
                "criteria": ["routine", "soon", "urgent", "critical"]
            }}
        })
    );

    let text = render::outcome(&req, &out);
    let head = format!(
        "**Jev score:** {} on {LO}-{} (between soon and urgent)",
        at(1.4),
        LO + 3
    );
    assert!(text.contains(&head), "{text}");
    let legend = format!(
        "`{LO}` routine · `{}` soon · `{}` urgent · `{}` critical",
        LO + 1,
        LO + 2,
        LO + 3
    );
    assert!(text.contains(&legend), "{text}");
    assert!(text.contains("Jev confidence: 88.0%"), "{text}");
}

#[tokio::test]
async fn level_probabilities_are_accepted_sorted_and_shown() {
    let k = |i: usize| (LO + i).to_string();
    let body = ok_body(json!({"type": "score", "score": at(2.0), "confidence": 0.7,
        "probabilities": {k(3): 0.1, k(0): 0.05, k(2): 0.6, k(1): 0.25},
        "legend": {k(0): "routine", k(1): "soon", k(2): "urgent", k(3): "critical"}}));
    let (base, _) = fake_jev(Reply::Json(200, body)).await;
    let req = urgency();
    let out = client(&base, 2_000).score(&req, None).await.unwrap();
    assert_eq!(
        out.level_probabilities,
        vec![(LO, 0.05), (LO + 1, 0.25), (LO + 2, 0.6), (LO + 3, 0.1)]
    );
    let text = render::outcome(&req, &out);
    assert!(
        text.contains(&format!(
            "**Jev score:** {} on {LO}-{} (at urgent)",
            at(2.0),
            LO + 3
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("`{}` urgent (60.0%)", LO + 2)),
        "{text}"
    );
}

#[test]
fn fractional_scores_are_never_rounded_to_a_level() {
    for (v, shown) in [
        (1.4, "1.4"),
        (1.9996, "1.9996"),
        (0.05, "0.05"),
        (3.0, "3"),
        (2.25, "2.25"),
    ] {
        assert_eq!(render::number(v), shown);
    }
}

#[test]
fn position_is_pinned_at_both_ends_of_the_scale() {
    let req = urgency();
    assert_eq!((req.min_score(), req.max_score()), (LO, LO + 3));
    // Bottom end.
    assert_eq!(render::position(&req, at(0.0)), "at routine");
    assert_eq!(
        render::position(&req, at(0.001)),
        "between routine and soon"
    );
    // Middle.
    assert_eq!(render::position(&req, at(1.0)), "at soon");
    assert_eq!(render::position(&req, at(1.4)), "between soon and urgent");
    // Top end.
    assert_eq!(
        render::position(&req, at(2.999)),
        "between urgent and critical"
    );
    assert_eq!(render::position(&req, at(3.0)), "at critical");
}

// ---- invalid / duplicate / too-few levels: refused before any call -------

#[test]
fn bad_levels_and_text_are_refused_locally() {
    let p = |l: &str| ScoreRequest::parse("some text", "How bad?", l);
    assert_eq!(p(""), Err(InputError::TooFewLevels(0)));
    assert_eq!(p("only"), Err(InputError::TooFewLevels(1)));
    assert_eq!(p("low, , high"), Err(InputError::EmptyLevel(2)));
    assert_eq!(
        p("low, high, Low"),
        Err(InputError::DuplicateLevel("Low".into()))
    );
    assert_eq!(p("a|b|a"), Err(InputError::DuplicateLevel("a".into())));
    let eleven: Vec<String> = (1..=11).map(|i| format!("l{i}")).collect();
    assert_eq!(p(&eleven.join(",")), Err(InputError::TooManyLevels(11)));
    assert!(p(&eleven[..10].join(",")).is_ok());
    assert_eq!(
        p(&format!("low,{}", "x".repeat(101))),
        Err(InputError::LevelTooLong(2))
    );
    // 10 levels x 100 chars only exceeds the 2,000-char criteria limit once
    // JSON escaping doubles the quotes.
    let wide: Vec<String> = (0..10).map(|i| format!("{i}{}", "\"".repeat(99))).collect();
    assert!(matches!(
        p(&wide.join(",")),
        Err(InputError::CriteriaTooLong(_))
    ));
    assert_eq!(p("low, high,").unwrap().levels, ["low", "high"]);

    assert_eq!(
        ScoreRequest::parse("  ", "q", "a,b"),
        Err(InputError::EmptyState)
    );
    assert_eq!(
        ScoreRequest::parse("t", " ", "a,b"),
        Err(InputError::EmptyQuestion)
    );
    assert_eq!(
        ScoreRequest::parse("t", &"q".repeat(1_801), "a,b"),
        Err(InputError::QuestionTooLong(1_801))
    );
    assert_eq!(
        ScoreRequest::parse(&"s".repeat(8_000), "q", "a,b"),
        Err(InputError::StateTooLong(8_002))
    );
    assert!(render::input_error(&InputError::DuplicateLevel("a".into())).contains("more than once"));
}

// ---- timeout ------------------------------------------------------------

#[tokio::test]
async fn timeout_is_bounded_and_reported() {
    let (base, seen) = fake_jev(Reply::Stall(Duration::from_secs(10))).await;
    let started = std::time::Instant::now();
    let err = client(&base, 300)
        .score(&urgency(), None)
        .await
        .unwrap_err();
    assert_eq!(err, ScoreError::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
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
        let err = client(&base, 2_000)
            .score(&urgency(), None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            ScoreError::Status {
                status,
                kind: Some(kind.into()),
                message: Some("nope".into())
            },
            "status {status}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "no retry on {status}");
        let text = render::jev_error(&err);
        assert!(
            text.contains(&status.to_string()) && !text.contains(FAKE_KEY),
            "{text}"
        );
    }
    let (base, _) = fake_jev(Reply::Json(503, "<html>down</html>".into())).await;
    let err = client(&base, 2_000)
        .score(&urgency(), None)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ScoreError::Status {
            status: 503,
            kind: None,
            message: None
        }
    );
}

// ---- malformed response -------------------------------------------------

#[tokio::test]
async fn malformed_typed_responses_are_rejected() {
    // (case, body, substring the rejection reason must contain) - the reason
    // pins WHICH check fired.
    let above_top = format!("score {} is outside {LO}-{}", at(3.2), LO + 3);
    let below_bottom = format!("score {} is outside {LO}-{}", at(-0.1), LO + 3);
    let unknown_above = format!("unknown level \"{}\"", LO + 4);
    let prob_range = format!("level {} probability 1.5 is outside", LO + 1);
    let cases: Vec<(&str, String, &str)> = vec![
        ("not json", "nope".into(), "not a Jev response"),
        (
            "no answers",
            json!({"model": "jev-latest"}).to_string(),
            "not a Jev response",
        ),
        (
            "wrong question name",
            json!({"answers": {"urgency": {"type": "score", "score": 1.0, "confidence": 0.5}}})
                .to_string(),
            "no answer for \"score\"",
        ),
        (
            "wrong type",
            ok_body(json!({"type": "choice", "choice": "soon", "confidence": 0.5})),
            "expected \"score\"",
        ),
        (
            "missing score",
            ok_body(json!({"type": "score", "confidence": 0.5})),
            "no score",
        ),
        (
            "score as string",
            ok_body(json!({"type": "score", "score": "1.4", "confidence": 0.5})),
            "not a Jev response",
        ),
        (
            "score above top level",
            ok_body(json!({"type": "score", "score": at(3.2), "confidence": 0.5})),
            &above_top,
        ),
        (
            "score below bottom level",
            ok_body(json!({"type": "score", "score": at(-0.1), "confidence": 0.5})),
            &below_bottom,
        ),
        (
            "missing confidence",
            ok_body(json!({"type": "score", "score": 1.0})),
            "no confidence",
        ),
        (
            "confidence above 1",
            ok_body(json!({"type": "score", "score": 1.0, "confidence": 1.2})),
            "confidence 1.2 is outside",
        ),
        (
            "probability for level above top",
            ok_body(json!({"type": "score", "score": at(1.0), "confidence": 0.5,
            "probabilities": {(LO + 4).to_string(): 0.3}})),
            &unknown_above,
        ),
        (
            "probability key not a level",
            ok_body(json!({"type": "score", "score": 1.0, "confidence": 0.5,
            "probabilities": {"soon": 0.3}})),
            "unknown level \"soon\"",
        ),
        (
            "probability out of range",
            ok_body(json!({"type": "score", "score": 1.0, "confidence": 0.5,
            "probabilities": {(LO + 1).to_string(): 1.5}})),
            &prob_range,
        ),
    ];
    for (name, body, reason) in cases {
        let (base, _) = fake_jev(Reply::Json(200, body)).await;
        let err = client(&base, 2_000)
            .score(&urgency(), None)
            .await
            .unwrap_err();
        match &err {
            ScoreError::Malformed(why) => assert!(
                why.contains(reason),
                "{name}: reason {why:?} lacks {reason:?}"
            ),
            other => panic!("{name}: got {other:?}"),
        }
    }
}

#[tokio::test]
async fn oversized_response_is_refused() {
    let huge = format!(
        "{{\"pad\":\"{}\"}}",
        "x".repeat(score::jev::MAX_RESPONSE_BYTES + 10)
    );
    let (base, _) = fake_jev(Reply::Json(200, huge)).await;
    let err = client(&base, 2_000)
        .score(&urgency(), None)
        .await
        .unwrap_err();
    assert_eq!(err, ScoreError::TooLarge);
}

#[tokio::test]
async fn unreachable_server_is_a_transport_error() {
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let err = client(&format!("http://127.0.0.1:{port}"), 2_000)
        .score(&urgency(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, ScoreError::Transport(_)), "{err:?}");
}

#[test]
fn client_debug_redacts_key() {
    let c = ScoreClient::new("http://127.0.0.1:1", FAKE_KEY, Duration::from_secs(1)).unwrap();
    assert!(!format!("{c:?}").contains(FAKE_KEY));
}
