//! Jev-only live contract probe: exactly one real call each for choice,
//! score and noul (3 paid calls). Ignored by default - `cargo test` never
//! runs it. Run on purpose, with JEVMODEL_API_KEY in .env:
//!
//!     cargo test --offline --test live_probe -- --ignored --nocapture
//!
//! Prints each request body (the key is never printed), the HTTP status,
//! the raw response body, and what the feature's own parser makes of it.
//! Stops before the next call if the key is refused (401) or out of
//! credit (402).

#[path = "../src/choice/mod.rs"]
#[allow(dead_code)]
mod choice;
#[path = "../src/noul/mod.rs"]
#[allow(dead_code)]
mod noul;
#[path = "../src/score/mod.rs"]
#[allow(dead_code)]
mod score;

use std::time::Duration;

async fn call(http: &reqwest::Client, base: &str, key: &str, label: &str, body: &serde_json::Value) -> Option<Vec<u8>> {
    println!("\n=== {label}: request body\n{}", serde_json::to_string_pretty(body).unwrap());
    let resp = http
        .post(format!("{base}/v1/systemone"))
        .bearer_auth(key)
        .header("Idempotency-Key", format!("jev-probe-{label}-{}", std::process::id()))
        .json(body)
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            println!("=== {label}: transport error: {}", e.without_url());
            return None;
        }
    };
    let status = resp.status();
    let bytes = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    println!("=== {label}: HTTP {}", status.as_u16());
    if status.as_u16() == 401 || status.as_u16() == 402 {
        println!("=== {label}: stopping - key refused or out of credit; no further calls");
        return None;
    }
    println!("=== {label}: response body\n{}", String::from_utf8_lossy(&bytes));
    status.is_success().then_some(bytes)
}

#[tokio::test]
#[ignore = "live: makes 3 paid jevmodel.org calls"]
async fn live_probe_one_call_per_feature() {
    let _ = dotenvy::dotenv();
    let key = std::env::var("JEVMODEL_API_KEY").expect("JEVMODEL_API_KEY not set");
    let key = key.trim();
    let base = std::env::var("JEV_BASE_URL").unwrap_or_else(|_| "https://jevmodel.org".into());
    println!("key: {} chars, starts with sk-: {}", key.len(), key.starts_with("sk-"));
    let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();

    // 1. choice
    let req = choice::ChoiceRequest::parse("Where should the team eat?", "pizza, sushi, tacos", Some("Two of us are vegetarian and we have 30 minutes.")).unwrap();
    let Some(body) = call(&http, &base, key, "choice", &choice::JevClient::request_body(&req)).await else { return };
    println!("=== choice: parsed -> {:?}", choice::jev::parse_choice(&body, &req));

    // 2. score - a clearly top-level state, so the returned number shows
    // the base: ~3 means 0-based (0-3), ~4 means 1-based (1-4).
    let req = score::ScoreRequest::parse(
        "Production database is down for every customer, payments are failing and data is being lost right now.",
        "How urgent is this for the customer?",
        "routine, soon, urgent, critical",
    )
    .unwrap();
    let Some(body) = call(&http, &base, key, "score", &score::ScoreClient::request_body(&req)).await else { return };
    println!("=== score: parsed with LOWEST_LEVEL_SCORE={} -> {:?}", score::input::LOWEST_LEVEL_SCORE, score::jev::parse_score(&body, &req));

    // 3. noul
    let req = noul::NoulRequest::parse("The customer was billed twice and wants a refund today.", "Does this need a human right now?", None, None).unwrap();
    let Some(body) = call(&http, &base, key, "noul", &noul::NoulClient::request_body(&req)).await else { return };
    println!("=== noul: parsed -> {:?}", noul::jev::parse_noul(&body));
}
