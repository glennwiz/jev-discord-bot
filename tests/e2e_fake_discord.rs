//! End-to-end resilience and shutdown test for the real bot binary.
//!
//! A SECOND instance of the bot runs with a deliberately invalid Discord
//! token against a fake Discord (HTTP API via the loopback-only
//! `DISCORD_API_PROXY` test seam, plus a `ws://` gateway) and a fake
//! TypeSafe, all on 127.0.0.1 - it never reaches Discord or TypeSafe and
//! cannot collide with a running bot. The test then drives interactions:
//!
//! - malformed input -> private reply, no TypeSafe call
//! - TypeSafe 5xx, TypeSafe timeout, malformed TypeSafe answer -> error reply
//! - Discord reply (edit) failure -> logged, dropped
//! - the process is still alive and answers a valid command after all that
//! - SIGTERM while a TypeSafe call is in flight -> that reply is still
//!   delivered (drain), then exit status 0
//!
//! Unix only (SIGTERM via `kill`). Offline: loopback only.
#![cfg(unix)]

use std::collections::VecDeque;
use std::future::Future;
use std::io::{BufRead, BufReader};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const TOKEN: &str = "invalid-e2e-token-not-a-discord-token";
const API_KEY: &str = "apikey_e2e-fake";
const APP_ID: &str = "444";
const GUILD_ID: &str = "111";

// ---- a tiny HTTP/1.1 server --------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    body: Value,
    at: Instant,
}

struct Resp {
    status: u16,
    body: String,
    delay: Duration,
}

impl Resp {
    fn json(status: u16, body: impl Into<String>) -> Self {
        Resp {
            status,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }
}

type Handler = Arc<dyn Fn(Req) -> Pin<Box<dyn Future<Output = Resp> + Send>> + Send + Sync>;

/// Serve `handler` on 127.0.0.1; every request is logged. Returns the port.
async fn http_server(handler: Handler, log: Arc<Mutex<Vec<Req>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (handler, log) = (handler.clone(), log.clone());
            tokio::spawn(async move {
                let Some(req) = read_req(&mut sock).await else {
                    return;
                };
                log.lock().unwrap().push(req.clone());
                let resp = handler(req).await;
                tokio::time::sleep(resp.delay).await;
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp.status,
                    resp.body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(resp.body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

async fn read_req(sock: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
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
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let len: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(Value::Null);
    Some(Req {
        method,
        path,
        body,
        at: Instant::now(),
    })
}

// ---- fake Discord HTTP API ---------------------------------------------

/// Interaction tokens whose reply edit should fail with HTTP 500.
type FailEdits = Arc<Mutex<Vec<String>>>;

fn discord_handler(ws_port: u16, fail_edits: FailEdits) -> Handler {
    Arc::new(move |req: Req| {
        let fail_edits = fail_edits.clone();
        Box::pin(async move {
            let p = req.path.split('?').next().unwrap_or("").to_string();
            if req.method == "GET" && p == "/api/v10/gateway" {
                return Resp::json(
                    200,
                    json!({"url": format!("ws://127.0.0.1:{ws_port}")}).to_string(),
                );
            }
            if req.method == "PUT" && p.ends_with(&format!("/guilds/{GUILD_ID}/commands")) {
                return Resp::json(200, "[]");
            }
            if req.method == "POST"
                && p.starts_with("/api/v10/interactions/")
                && p.ends_with("/callback")
            {
                return Resp::json(204, "");
            }
            if req.method == "PATCH" && p.ends_with("/messages/@original") {
                let token = p.split('/').nth(5).unwrap_or("").to_string();
                if fail_edits.lock().unwrap().contains(&token) {
                    return Resp::json(500, r#"{"message":"fake discord failure","code":0}"#);
                }
                let content = req.body["content"].clone();
                return Resp::json(200, message_json(content).to_string());
            }
            Resp::json(404, r#"{"message":"404: Not Found","code":0}"#)
        })
    })
}

fn user_json(id: &str, name: &str, bot: bool) -> Value {
    json!({"id": id, "username": name, "discriminator": "0", "global_name": null,
           "avatar": null, "bot": bot})
}

fn message_json(content: Value) -> Value {
    json!({
        "id": "900", "channel_id": "555", "author": user_json(APP_ID, "e2e-bot", true),
        "content": content, "timestamp": "2026-09-26T00:00:00.000000+00:00",
        "edited_timestamp": null, "tts": false, "mention_everyone": false,
        "mentions": [], "mention_roles": [], "attachments": [], "embeds": [],
        "pinned": false, "type": 20, "flags": 0
    })
}

// ---- fake Discord gateway ----------------------------------------------

/// Accepts the bot's gateway connection: HELLO, READY after IDENTIFY,
/// heartbeat ACKs, and every Value sent on `dispatch` as an
/// INTERACTION_CREATE.
async fn gateway(mut dispatch: mpsc::UnboundedReceiver<Value>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((sock, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
            return;
        };
        let hello = json!({"op": 10, "d": {"heartbeat_interval": 45_000}, "s": null, "t": null});
        let _ = ws.send(WsMessage::Text(hello.to_string())).await;
        let mut seq = 0u64;
        loop {
            tokio::select! {
                incoming = ws.next() => {
                    let Some(Ok(msg)) = incoming else { return };
                    let WsMessage::Text(text) = msg else {
                        if matches!(msg, WsMessage::Close(_)) { return; }
                        continue;
                    };
                    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    match v["op"].as_u64() {
                        Some(2) => {
                            seq += 1;
                            let ready = json!({"op": 0, "s": seq, "t": "READY", "d": {
                                "v": 10, "user": user_json(APP_ID, "e2e-bot", true),
                                "guilds": [], "session_id": "e2e-session",
                                "resume_gateway_url": format!("ws://127.0.0.1:{port}"),
                                "shard": [0, 1], "application": {"id": APP_ID, "flags": 0}
                            }});
                            let _ = ws.send(WsMessage::Text(ready.to_string())).await;
                        }
                        Some(1) => {
                            let ack = json!({"op": 11, "d": null, "s": null, "t": null});
                            let _ = ws.send(WsMessage::Text(ack.to_string())).await;
                        }
                        _ => {}
                    }
                }
                out = dispatch.recv() => {
                    let Some(d) = out else { return };
                    seq += 1;
                    let ev = json!({"op": 0, "s": seq, "t": "INTERACTION_CREATE", "d": d});
                    let _ = ws.send(WsMessage::Text(ev.to_string())).await;
                }
            }
        }
    });
    port
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A `/jev <sub>` command interaction with a fresh snowflake id.
fn interaction(token: &str, sub: &str, opts: &[(&str, &str)]) -> (String, Value) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let id = ((ms - 1_420_070_400_000) << 22) + NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let options: Vec<Value> = opts
        .iter()
        .map(|(k, v)| json!({"name": k, "type": 3, "value": v}))
        .collect();
    let d = json!({
        "id": id.to_string(), "application_id": APP_ID, "type": 2, "token": token,
        "version": 1, "guild_id": GUILD_ID, "channel_id": "555",
        "locale": "en-US", "guild_locale": "en-US", "app_permissions": "0",
        "user": user_json("666", "tester", false), "entitlements": [],
        // Required by serenity 0.12.5's Interaction deserializer.
        "attachment_size_limit": 8_388_608,
        "data": {"id": "777", "name": "jev", "type": 1,
                 "options": [{"name": sub, "type": 1, "options": options}]}
    });
    (id.to_string(), d)
}

// ---- fake TypeSafe -------------------------------------------------------

type JevQueue = Arc<Mutex<VecDeque<Resp>>>;

fn jev_handler(queue: JevQueue) -> Handler {
    Arc::new(move |_req: Req| {
        let next = queue.lock().unwrap().pop_front();
        Box::pin(
            async move { next.unwrap_or_else(|| Resp::json(500, r#"{"detail":"queue empty"}"#)) },
        )
    })
}

fn answer(name: &str, answer: Value) -> String {
    json!({"model": "jev-1.13.0", "answers": {name: answer},
           "usage": {"input_tokens": 10, "output_tokens": 1}})
    .to_string()
}

// ---- the bot process ----------------------------------------------------

struct Bot {
    child: Child,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Bot {
    fn start(discord_port: u16, jev_port: u16) -> Bot {
        Bot::start_with(discord_port, jev_port, None)
    }

    /// With `close_stderr_after`, the test stops reading the bot's stderr and
    /// closes its end of the pipe right after the first line starting with
    /// that prefix - like a killed `tee` - so later writes get EPIPE.
    fn start_with(discord_port: u16, jev_port: u16, close_stderr_after: Option<&'static str>) -> Bot {
        // Empty working dir: no .env can leak real credentials in.
        let cwd = std::env::temp_dir().join(format!("jev-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_jev-discord-bot"))
            .current_dir(&cwd)
            .env_clear()
            .env("DISCORD_TOKEN", TOKEN)
            .env("DISCORD_GUILD_ID", GUILD_ID)
            .env("TYPESAFE_API_KEY", API_KEY)
            .env("JEV_BASE_URL", format!("http://127.0.0.1:{jev_port}"))
            .env("JEV_TIMEOUT_SECS", "2")
            .env(
                "DISCORD_API_PROXY",
                format!("http://127.0.0.1:{discord_port}"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn bot binary");
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let lines = stderr.clone();
        let pipe = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let stop = close_stderr_after.is_some_and(|p| line.starts_with(p));
                lines.lock().unwrap().push(line);
                if stop {
                    return; // drops the reader: the pipe is now broken
                }
            }
        });
        Bot { child, stderr }
    }

    fn log(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Bot {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Poll `cond` every 20 ms until true or `secs` elapse.
async fn wait_for(what: &str, secs: u64, bot: &Bot, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        if Instant::now() > deadline {
            panic!(
                "timed out waiting for {what}\nbot stderr:\n{}",
                bot.log().join("\n")
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn find<'a>(log: &'a [Req], method: &str, token: &str, tail: &str) -> Option<&'a Req> {
    log.iter().find(|r| {
        r.method == method && r.path.contains(&format!("/{token}/")) && r.path.ends_with(tail)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_instance_survives_failures_and_drains_on_sigterm() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ws_port = gateway(rx).await;
    let fail_edits: FailEdits = Arc::new(Mutex::new(Vec::new()));
    let discord_log = Arc::new(Mutex::new(Vec::new()));
    let discord_port = http_server(
        discord_handler(ws_port, fail_edits.clone()),
        discord_log.clone(),
    )
    .await;
    let jev_queue: JevQueue = Arc::new(Mutex::new(VecDeque::new()));
    let jev_log = Arc::new(Mutex::new(Vec::new()));
    let jev_port = http_server(jev_handler(jev_queue.clone()), jev_log.clone()).await;

    let mut bot = Bot::start(discord_port, jev_port);
    let dlog = || discord_log.lock().unwrap().clone();
    let jev_calls = || jev_log.lock().unwrap().len();

    wait_for("READY and command registration", 20, &bot, || {
        bot.log().iter().any(|l| l.starts_with("registered "))
    })
    .await;
    assert!(bot
        .log()
        .iter()
        .any(|l| l.starts_with("WARN: DISCORD_API_PROXY is set")));
    let put = dlog()
        .into_iter()
        .find(|r| r.method == "PUT")
        .expect("command registration");
    let subs: Vec<&str> = put.body[0]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(subs, ["choice", "score", "noul"]);

    // 1. Malformed input: private reply, no TypeSafe call.
    let (_, d) = interaction(
        "tok-bad-input",
        "choice",
        &[("question", "Pick?"), ("options", "a, a")],
    );
    tx.send(d).unwrap();
    wait_for("private reply to bad input", 10, &bot, || {
        find(&dlog(), "POST", "tok-bad-input", "/callback").is_some()
    })
    .await;
    let cb = find(&dlog(), "POST", "tok-bad-input", "/callback")
        .unwrap()
        .body
        .clone();
    assert_eq!(cb["type"], 4, "{cb}");
    assert_eq!(cb["data"]["flags"], 64, "ephemeral: {cb}");
    assert!(
        cb["data"]["content"]
            .as_str()
            .unwrap()
            .contains("more than once"),
        "{cb}"
    );
    assert_eq!(jev_calls(), 0);

    // 2. TypeSafe 5xx -> deferred, one call, error reply with its message.
    jev_queue.lock().unwrap().push_back(Resp::json(
        502,
        r#"{"detail":{"error_type":"upstream_error","message":"model offline"}}"#,
    ));
    let (_, d) = interaction(
        "tok-5xx",
        "choice",
        &[("question", "Pick?"), ("options", "a, b")],
    );
    tx.send(d).unwrap();
    wait_for("edit after 5xx", 10, &bot, || {
        find(&dlog(), "PATCH", "tok-5xx", "@original").is_some()
    })
    .await;
    assert_eq!(
        find(&dlog(), "POST", "tok-5xx", "/callback").unwrap().body["type"],
        5,
        "deferred first"
    );
    let text = find(&dlog(), "PATCH", "tok-5xx", "@original").unwrap().body["content"].to_string();
    assert!(
        text.contains("HTTP 502") && text.contains("model offline"),
        "{text}"
    );
    assert_eq!(jev_calls(), 1);

    // 3. TypeSafe timeout (JEV_TIMEOUT_SECS=2) -> error reply.
    jev_queue.lock().unwrap().push_back(Resp {
        status: 200,
        body: "{}".into(),
        delay: Duration::from_secs(8),
    });
    let (_, d) = interaction(
        "tok-timeout",
        "score",
        &[
            ("text", "t"),
            ("question", "How bad?"),
            ("levels", "low, high"),
        ],
    );
    tx.send(d).unwrap();
    wait_for("edit after timeout", 10, &bot, || {
        find(&dlog(), "PATCH", "tok-timeout", "@original").is_some()
    })
    .await;
    let text = find(&dlog(), "PATCH", "tok-timeout", "@original")
        .unwrap()
        .body["content"]
        .to_string();
    assert!(text.contains("did not answer in time"), "{text}");

    // 4. Malformed TypeSafe answer -> error reply.
    jev_queue.lock().unwrap().push_back(Resp::json(
        200,
        answer("noul", json!({"type": "noul", "noul": 7})),
    ));
    let (_, d) = interaction(
        "tok-malformed",
        "noul",
        &[("text", "t"), ("question", "Yes?")],
    );
    tx.send(d).unwrap();
    wait_for("edit after malformed answer", 10, &bot, || {
        find(&dlog(), "PATCH", "tok-malformed", "@original").is_some()
    })
    .await;
    let text = find(&dlog(), "PATCH", "tok-malformed", "@original")
        .unwrap()
        .body["content"]
        .to_string();
    assert!(text.contains("unexpected answer"), "{text}");

    // 5. Discord reply failure: the edit gets HTTP 500 -> logged, dropped.
    fail_edits.lock().unwrap().push("tok-edit-fails".into());
    jev_queue.lock().unwrap().push_back(Resp::json(
        200,
        answer("noul", json!({"type": "noul", "noul": 0.3})),
    ));
    let (id, d) = interaction(
        "tok-edit-fails",
        "noul",
        &[("text", "t"), ("question", "Yes?")],
    );
    tx.send(d).unwrap();
    wait_for("discord reply failure logged", 10, &bot, || {
        bot.log()
            .iter()
            .any(|l| l.starts_with(&format!("discord reply failed: interaction={id}")))
    })
    .await;

    // 6. Still alive, and a valid command still works end to end.
    assert!(
        bot.alive(),
        "bot exited after failures:\n{}",
        bot.log().join("\n")
    );
    jev_queue.lock().unwrap().push_back(Resp::json(
        200,
        answer("pick", json!({"type": "choice", "choice": "b", "probabilities": {"a": 0.3, "b": 0.7}, "confidence": 0.4})),
    ));
    let (id, d) = interaction(
        "tok-ok",
        "choice",
        &[("question", "Pick?"), ("options", "a, b")],
    );
    tx.send(d).unwrap();
    wait_for("valid reply", 10, &bot, || {
        find(&dlog(), "PATCH", "tok-ok", "@original").is_some()
    })
    .await;
    let text = find(&dlog(), "PATCH", "tok-ok", "@original").unwrap().body["content"].to_string();
    assert!(
        text.contains("**Jev picks:** b") && text.contains("70.0%") && text.contains("40.0%"),
        "{text}"
    );
    wait_for("timing line", 5, &bot, || {
        bot.log()
            .iter()
            .any(|l| l.starts_with(&format!("choice timing: interaction={id} ")))
    })
    .await;

    // 7. SIGTERM while a TypeSafe call is in flight: the reply is still
    //    delivered after the signal, then the process exits 0.
    let calls_before = jev_calls();
    jev_queue.lock().unwrap().push_back(Resp {
        status: 200,
        body: answer("noul", json!({"type": "noul", "noul": 0.81})),
        delay: Duration::from_millis(1_500),
    });
    let (_, d) = interaction("tok-drain", "noul", &[("text", "t"), ("question", "Yes?")]);
    tx.send(d).unwrap();
    wait_for("in-flight TypeSafe call", 10, &bot, || {
        jev_calls() > calls_before
    })
    .await;
    let signalled = Instant::now();
    let pid = bot.child.id().to_string();
    assert!(Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .unwrap()
        .success());
    let status = loop {
        if let Some(s) = bot.child.try_wait().unwrap() {
            break s;
        }
        assert!(
            signalled.elapsed() < Duration::from_secs(20),
            "no exit after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // The stderr reader thread may lag the exit slightly.
    wait_for("shutdown log", 5, &bot, || {
        bot.log().iter().any(|l| l.starts_with("shutdown: clean"))
    })
    .await;
    let log = bot.log();
    assert_eq!(status.code(), Some(0), "exit status\n{}", log.join("\n"));
    assert!(
        log.iter().any(|l| l == "shutdown: SIGTERM received"),
        "{}",
        log.join("\n")
    );
    let edit = find(&dlog(), "PATCH", "tok-drain", "@original")
        .cloned()
        .expect("drained reply delivered");
    assert!(edit.at > signalled, "reply edit happened after SIGTERM");
    assert!(
        edit.body["content"]
            .to_string()
            .contains("**P(yes):** 0.81"),
        "{}",
        edit.body
    );

    // Secrets never reach the log.
    let all = log.join("\n");
    assert!(
        !all.contains(TOKEN) && !all.contains(API_KEY),
        "secret in log:\n{all}"
    );
}

// ---- #6: stderr broken (reader gone) --------------------------------------

/// Fake Discord + fake TypeSafe, and a bot whose stderr pipe is closed by
/// the test as soon as it has registered its commands.
struct BrokenStderrWorld {
    tx: mpsc::UnboundedSender<Value>,
    discord_log: Arc<Mutex<Vec<Req>>>,
    jev_queue: JevQueue,
    bot: Bot,
}

async fn broken_stderr_world() -> BrokenStderrWorld {
    let (tx, rx) = mpsc::unbounded_channel();
    let ws_port = gateway(rx).await;
    let discord_log = Arc::new(Mutex::new(Vec::new()));
    let fail_edits: FailEdits = Arc::new(Mutex::new(Vec::new()));
    let discord_port = http_server(discord_handler(ws_port, fail_edits), discord_log.clone()).await;
    let jev_queue: JevQueue = Arc::new(Mutex::new(VecDeque::new()));
    let jev_port = http_server(jev_handler(jev_queue.clone()), Arc::new(Mutex::new(Vec::new()))).await;
    let bot = Bot::start_with(discord_port, jev_port, Some("registered "));
    wait_for("READY and command registration", 20, &bot, || {
        bot.log().iter().any(|l| l.starts_with("registered "))
    })
    .await;
    // Give the reader thread a moment to drop its end of the pipe.
    tokio::time::sleep(Duration::from_millis(200)).await;
    BrokenStderrWorld { tx, discord_log, jev_queue, bot }
}

/// JEV-06: the bot's stderr reader is gone (its `tee` was killed), then
/// SIGTERM. Every shutdown log line now hits EPIPE; the bot must still exit
/// 0 within DRAIN_TIMEOUT (15 s) - not hang until SIGKILL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_exits_even_when_stderr_is_broken() {
    let mut w = broken_stderr_world().await;
    let pid = w.bot.child.id().to_string();
    let signalled = Instant::now();
    assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
    let limit = Duration::from_secs(15 + 5);
    let status = loop {
        if let Some(s) = w.bot.child.try_wait().unwrap() {
            break s;
        }
        assert!(
            signalled.elapsed() < limit,
            "bot did not exit within {limit:?} of SIGTERM with a broken stderr (the #6 hang)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status.code(), Some(0), "exit status with a broken stderr");
}

/// JEV-06: with a broken stderr, a command is still answered - logging a
/// result must never take down the handler before it edits the reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_are_answered_even_when_stderr_is_broken() {
    let mut w = broken_stderr_world().await;
    w.jev_queue.lock().unwrap().push_back(Resp::json(
        200,
        answer("noul", json!({"type": "noul", "noul": 0.42})),
    ));
    let (_, d) = interaction("tok-broken-stderr", "noul", &[("text", "t"), ("question", "Yes?")]);
    w.tx.send(d).unwrap();
    let dlog = || w.discord_log.lock().unwrap().clone();
    wait_for("reply edit with a broken stderr", 10, &w.bot, || {
        find(&dlog(), "PATCH", "tok-broken-stderr", "@original").is_some()
    })
    .await;
    let edit = find(&dlog(), "PATCH", "tok-broken-stderr", "@original").unwrap().body.clone();
    assert!(edit["content"].to_string().contains("**P(yes):** 0.42"), "{edit}");
    assert!(w.bot.alive());
}
