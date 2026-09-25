//! Offline tests for `/jev noul` against a fake Jev HTTP server on
//! 127.0.0.1. No Discord, internet, real key or paid call is involved.

#[path = "../src/noul/mod.rs"]
#[allow(dead_code)]
mod noul;
mod common;

use std::time::Duration;

use common::{fake_jev, Reply};
use noul::{render, InputError, NoulClient, NoulError, NoulRequest};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const FAKE_KEY: &str = "sk-test-not-a-real-key";

fn escalate() -> NoulRequest {
    NoulRequest::parse("Billed twice, wants a refund today.", "Does this need a human right now?", None, None).unwrap()
}

fn client(base: &str, timeout_ms: u64) -> NoulClient {
    NoulClient::new(base, FAKE_KEY, Duration::from_millis(timeout_ms)).unwrap()
}

fn ok_body(answer: Value) -> String {
    json!({"model": "jev-latest", "answers": {"noul": answer}, "usage": {"input_tokens": 40, "output_tokens": 1}})
        .to_string()
}

// ---- valid probability --------------------------------------------------

#[tokio::test]
async fn valid_noul_sends_contract_request_once_and_reports_p_yes_only() {
    let (base, seen) = fake_jev(Reply::Json(200, ok_body(json!({"type": "noul", "noul": 0.12})))).await;
    let req = escalate();
    let out = client(&base, 2_000).ask(&req, Some("discord-789")).await.unwrap();
    assert_eq!(out.p_yes, 0.12);
    assert_eq!(out.input_tokens, Some(40));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one call to Jev");
    let r = &seen[0];
    assert_eq!(r.request_line, "POST /v1/systemone HTTP/1.1");
    assert_eq!(r.header("authorization"), Some(format!("Bearer {FAKE_KEY}").as_str()));
    assert_eq!(r.header("idempotency-key"), Some("discord-789"));
    // No criteria key at all when the user described neither side.
    assert_eq!(
        r.body,
        json!({
            "model": "jev-latest",
            "state": "Billed twice, wants a refund today.",
            "questions": {"noul": {"type": "noul", "instructions": "Does this need a human right now?"}}
        })
    );

    let text = render::outcome(&req, &out);
    assert_eq!(text, "**Question:** Does this need a human right now?\n**P(yes):** 0.12 (on 0-1)");
}

#[tokio::test]
async fn yes_no_descriptions_become_true_false_criteria() {
    let (base, seen) = fake_jev(Reply::Json(200, ok_body(json!({"type": "noul", "noul": 0.5})))).await;
    let req = NoulRequest::parse("t", "q?", Some(" Deadline today "), None).unwrap();
    let out = client(&base, 2_000).ask(&req, None).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].body["questions"]["noul"]["criteria"], json!({"true": "Deadline today", "false": "No"}));
    assert_eq!(seen[0].header("idempotency-key"), None);
    // Exactly 0.5 still gets no verdict: the reply never says yes or no.
    let text = render::outcome(&req, &out);
    assert!(text.ends_with("**P(yes):** 0.5 (on 0-1)"), "{text}");
}

#[tokio::test]
async fn extremes_are_reported_without_a_verdict_or_confidence() {
    for (p, shown) in [(0.0, "0"), (1.0, "1"), (0.9996, "0.9996")] {
        let (base, _) = fake_jev(Reply::Json(200, ok_body(json!({"type": "noul", "noul": p})))).await;
        let out = client(&base, 2_000).ask(&escalate(), None).await.unwrap();
        let text = render::outcome(&escalate(), &out);
        assert!(text.contains(&format!("**P(yes):** {shown} (on 0-1)")), "{text}");
        for fabricated in ["Yes", "No", "onfidence", "true", "false", "%"] {
            let after_question = text.split_once('\n').unwrap().1;
            assert!(!after_question.contains(fabricated), "{fabricated:?} in {text}");
        }
    }
}

// ---- input validation ---------------------------------------------------

#[test]
fn input_is_validated_locally() {
    assert_eq!(NoulRequest::parse("  ", "q?", None, None), Err(InputError::EmptyState));
    assert_eq!(NoulRequest::parse("t", " ", None, None), Err(InputError::EmptyQuestion));
    assert_eq!(NoulRequest::parse("t", &"q".repeat(1_801), None, None), Err(InputError::QuestionTooLong(1_801)));
    assert_eq!(NoulRequest::parse(&"s".repeat(8_000), "q?", None, None), Err(InputError::StateTooLong(8_002)));
    assert_eq!(NoulRequest::parse("t", "q?", Some(&"y".repeat(501)), None), Err(InputError::MeaningTooLong("yes")));
    assert_eq!(NoulRequest::parse("t", "q?", None, Some(&"n".repeat(501))), Err(InputError::MeaningTooLong("no")));
    // Two 500-char descriptions only exceed 2,000 once JSON escaping doubles the quotes.
    let quotes = "\"".repeat(500);
    assert!(matches!(
        NoulRequest::parse("t", "q?", Some(&quotes), Some(&quotes)),
        Err(InputError::CriteriaTooLong(_))
    ));
    // Blank descriptions count as absent.
    let r = NoulRequest::parse("t", "q?", Some("  "), Some("")).unwrap();
    assert_eq!(r.criteria(), None);
    assert!(render::input_error(&InputError::EmptyQuestion).contains("question is empty"));
}

// ---- timeout ------------------------------------------------------------

#[tokio::test]
async fn timeout_is_bounded_and_reported() {
    let (base, seen) = fake_jev(Reply::Stall(Duration::from_secs(10))).await;
    let started = std::time::Instant::now();
    let err = client(&base, 300).ask(&escalate(), None).await.unwrap_err();
    assert_eq!(err, NoulError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert_eq!(seen.lock().unwrap().len(), 1, "no retry after timeout");
    assert!(render::jev_error(&err).contains("did not answer in time"));
}

// ---- HTTP failure -------------------------------------------------------

#[tokio::test]
async fn http_failures_surface_status_without_retry() {
    for (status, kind) in [
        (401, "authentication_error"),
        (402, "insufficient_credits"),
        (422, "invalid_request_error"),
        (429, "rate_limit_error"),
        (502, "upstream_error"),
    ] {
        let body = json!({"error": {"type": kind, "message": "nope"}}).to_string();
        let (base, seen) = fake_jev(Reply::Json(status, body)).await;
        let err = client(&base, 2_000).ask(&escalate(), None).await.unwrap_err();
        assert_eq!(
            err,
            NoulError::Status { status, kind: Some(kind.into()), message: Some("nope".into()) },
            "status {status}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "no retry on {status}");
        let text = render::jev_error(&err);
        assert!(text.contains(&status.to_string()) && !text.contains(FAKE_KEY), "{text}");
    }
    let (base, _) = fake_jev(Reply::Json(503, "<html>down</html>".into())).await;
    let err = client(&base, 2_000).ask(&escalate(), None).await.unwrap_err();
    assert_eq!(err, NoulError::Status { status: 503, kind: None, message: None });

    let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let err = client(&format!("http://127.0.0.1:{port}"), 2_000).ask(&escalate(), None).await.unwrap_err();
    assert!(matches!(err, NoulError::Transport(_)), "{err:?}");
}

// ---- missing / invalid values -------------------------------------------

#[tokio::test]
async fn missing_and_invalid_values_are_rejected() {
    // (case, body, substring the rejection reason must contain)
    let cases: Vec<(&str, String, &str)> = vec![
        ("not json", "nope".into(), "not a Jev response"),
        ("no answers", json!({"model": "jev-latest"}).to_string(), "not a Jev response"),
        ("wrong question name", json!({"answers": {"escalate": {"type": "noul", "noul": 0.2}}}).to_string(),
            "no answer for \"noul\""),
        ("wrong type", ok_body(json!({"type": "score", "score": 1.0, "confidence": 0.5})), "expected \"noul\""),
        ("missing noul", ok_body(json!({"type": "noul"})), "no noul probability"),
        ("null noul", ok_body(json!({"type": "noul", "noul": null})), "no noul probability"),
        ("noul as string", ok_body(json!({"type": "noul", "noul": "0.2"})), "not a Jev response"),
        ("noul as bool", ok_body(json!({"type": "noul", "noul": true})), "not a Jev response"),
        ("noul above 1", ok_body(json!({"type": "noul", "noul": 1.01})), "P(yes) 1.01 is outside"),
        ("negative noul", ok_body(json!({"type": "noul", "noul": -0.2})), "P(yes) -0.2 is outside"),
    ];
    for (name, body, reason) in cases {
        let (base, _) = fake_jev(Reply::Json(200, body)).await;
        let err = client(&base, 2_000).ask(&escalate(), None).await.unwrap_err();
        match &err {
            NoulError::Malformed(why) => assert!(why.contains(reason), "{name}: reason {why:?} lacks {reason:?}"),
            other => panic!("{name}: got {other:?}"),
        }
    }
}

#[tokio::test]
async fn oversized_response_is_refused() {
    let huge = format!("{{\"pad\":\"{}\"}}", "x".repeat(noul::jev::MAX_RESPONSE_BYTES + 10));
    let (base, _) = fake_jev(Reply::Json(200, huge)).await;
    let err = client(&base, 2_000).ask(&escalate(), None).await.unwrap_err();
    assert_eq!(err, NoulError::TooLarge);
}

#[test]
fn client_debug_redacts_key() {
    let c = NoulClient::new("http://127.0.0.1:1", FAKE_KEY, Duration::from_secs(1)).unwrap();
    assert!(!format!("{c:?}").contains(FAKE_KEY));
}
