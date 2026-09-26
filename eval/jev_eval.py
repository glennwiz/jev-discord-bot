#!/usr/bin/env python3
"""JEV-08: can Jev (System One) judge an implementation against its spec?

State = SPEC (board tasks #1-#6, text + accept) + IMPLEMENTATION (every
src/**/*.rs at a pinned commit). The same questions are asked of the real
code, of five mutants with one planted defect each, 3x for variance, and
once with the two sections swapped.

    python3 eval/jev_eval.py plan   --spec SPEC.json --out DIR          # build + hash all requests, no calls
    python3 eval/jev_eval.py run    --spec SPEC.json --out DIR RUN...   # call TypeSafe for the named runs
    python3 eval/jev_eval.py report --out DIR                           # results table from saved responses

RUN names: real1 real2 real3 m1_timeout m2_unwrap m3_one_based
m4_noul_verdict m5_defer_late swap. Stdlib only. The API key is read from
$TYPESAFE_API_KEY or the repo's .env and never printed or saved. Every call
is counted in DIR/calls.log; more than MAX_CALLS total is refused.

Board rule (Glenn, 2026-09-26): every request and reply goes on the board,
bound to task 8. Before each call the harness posts the call number,
variant, state section sizes + sha256, the request file path and all
questions verbatim - and does not call if that post fails; after each call
it posts the full reply JSON verbatim with HTTP status and latency. The
state text itself stays in the request file named by its sha256. Board URL:
$JEV_BOARD (default http://127.0.0.1:50003, e.g. via ssh -R), agent $JEV_AGENT.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

REV = "38a475d"
API = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-latest"
MAX_CALLS = 12
BOARD = os.environ.get("JEV_BOARD", "http://127.0.0.1:50003")
AGENT = os.environ.get("JEV_AGENT", "claude-jev-impl-f67a")
TASK_ID = 8
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# ---- questions -------------------------------------------------------------

def noul(instructions, yes, no):
    return {"type": "noul", "instructions": instructions, "criteria": {"true": yes, "false": no}}

QUESTIONS = {
    # Targeted by a mutant (see MUTANTS).
    "q_timeout": noul(
        "In the IMPLEMENTATION, does every HTTP client that calls the TypeSafe/Jev API set a total "
        "request timeout (a whole-call timeout, not only a connect timeout)?",
        "Every TypeSafe HTTP client sets a total request timeout.",
        "At least one TypeSafe HTTP client has no total request timeout and could wait indefinitely."),
    "q_no_panic": noul(
        "Is the IMPLEMENTATION free of code that can panic at runtime on data from TypeSafe or Discord, "
        "such as .unwrap(), .expect(), panic!() or unchecked indexing on external data?",
        "No runtime path can panic on external data.",
        "At least one runtime path can panic on external data (e.g. an .unwrap() on a response)."),
    "q_zero_based": noul(
        "Does the IMPLEMENTATION treat the score scale as 0-based, so the lowest level is score 0, "
        "as the SPEC's verified TypeSafe contract requires?",
        "The lowest score level is 0 in the implementation.",
        "The implementation uses a different base (e.g. the lowest level is score 1)."),
    "q_noul_only_p": noul(
        "Does the IMPLEMENTATION's /jev noul reply show only P(yes), with no Yes/No verdict and no "
        "confidence value?",
        "The noul reply contains only P(yes); no verdict, no confidence.",
        "The noul reply also shows a Yes/No verdict or a confidence value."),
    "q_defer_first": noul(
        "In the IMPLEMENTATION, does every /jev command handler defer the Discord interaction before it "
        "calls the TypeSafe API?",
        "Every handler defers before its TypeSafe call.",
        "At least one handler calls TypeSafe before deferring the interaction."),
    # Controls: no mutant touches these.
    "q_one_call": noul(
        "Does each /jev command in the IMPLEMENTATION make at most one TypeSafe API call, with no retries?",
        "At most one call per command, no retries.",
        "Some command can call TypeSafe more than once or retries."),
    "q_input_validated": noul(
        "Does the IMPLEMENTATION reject invalid user input (too few or too many options or levels, "
        "duplicates, over-long text) before any TypeSafe call?",
        "Invalid input is rejected locally before any TypeSafe call.",
        "Invalid input can reach TypeSafe."),
    "q_secrets": noul(
        "Does the IMPLEMENTATION keep the Discord token and the TypeSafe API key out of all log output "
        "and Debug formatting?",
        "Secrets never appear in logs or Debug output.",
        "A secret can appear in logs or Debug output."),
    "q_graceful": noul(
        "On SIGTERM, does the IMPLEMENTATION stop taking new interactions and let in-flight replies finish, "
        "bounded by a timeout, before exiting?",
        "Graceful, bounded shutdown on SIGTERM.",
        "SIGTERM does not drain in-flight replies, or the drain is unbounded."),
    "q_prob_vs_conf": noul(
        "Does the IMPLEMENTATION's /jev choice reply show the picked option's probability and Jev's "
        "confidence as two separate numbers?",
        "Probability of the picked option and confidence are shown separately.",
        "They are merged, or one of them is missing."),
    "q_typesafe_default": noul(
        "Is https://api.typesafe.ai the IMPLEMENTATION's default base URL for the System One API?",
        "The default base URL is https://api.typesafe.ai.",
        "The default base URL is something else."),
    "q_logs_nonpanic": noul(
        "Are all of the IMPLEMENTATION's writes to stderr non-panicking, so a broken stderr pipe cannot "
        "crash or hang the bot?",
        "Every stderr write ignores write errors.",
        "Some stderr write can panic on a broken pipe (e.g. eprintln!)."),
    "q_completeness": {
        "type": "score",
        "instructions": "How completely does the IMPLEMENTATION meet the SPEC?",
        "criteria": [
            "Barely: most requirements are missing or wrong",
            "Partly: many requirements are missing or wrong",
            "Mostly: several requirements are missing or wrong",
            "Nearly: one or two requirements are missing or wrong",
            "Fully: every requirement in the SPEC is met",
        ],
    },
    "q_defect_module": {
        "type": "choice",
        "instructions": "Which module of the IMPLEMENTATION is most likely to contain a defect against the SPEC?",
        "criteria": {
            "choice": "src/choice/: /jev choice input validation, TypeSafe call and reply text",
            "score": "src/score/: /jev score input validation, TypeSafe call, scale and reply text",
            "noul": "src/noul/: /jev noul input validation, TypeSafe call and reply text",
            "main": "src/main.rs: Discord gateway glue, defer and edit, logging, shutdown",
            "config": "src/config.rs: environment configuration and secret redaction",
        },
    },
}

# ---- mutants: one planted defect each, exact-match edits --------------------

MUTANTS = {
    "m1_timeout": {
        "target": "q_timeout", "module": "choice",
        "defect": "choice's HTTP client no longer sets a total timeout (.timeout(timeout) removed; connect timeout kept)",
        "edits": [("src/choice/jev.rs",
                   "            .timeout(timeout)\n            .connect_timeout(",
                   "            .connect_timeout(")],
    },
    "m2_unwrap": {
        "target": "q_no_panic", "module": "choice",
        "defect": "parse_choice unwraps the TypeSafe response parse instead of returning Malformed",
        "edits": [("src/choice/jev.rs",
                   "    let wire: WireResponse = serde_json::from_slice(body)\n"
                   "        .map_err(|e| JevError::Malformed(format!(\"not a Jev response ({e})\")))?;",
                   "    let wire: WireResponse = serde_json::from_slice(body).unwrap();")],
    },
    "m3_one_based": {
        "target": "q_zero_based", "module": "score",
        "defect": "LOWEST_LEVEL_SCORE = 1 (1-based scale)",
        "edits": [("src/score/input.rs",
                   "pub const LOWEST_LEVEL_SCORE: usize = 0;",
                   "pub const LOWEST_LEVEL_SCORE: usize = 1;")],
    },
    "m4_noul_verdict": {
        "target": "q_noul_only_p", "module": "noul",
        "defect": "the noul reply adds a Yes/No verdict (threshold 0.5)",
        "edits": [("src/noul/render.rs",
                   "        \"**Question:** {}\\n**P(yes):** {} (on 0-1)\",\n"
                   "        clip(&req.question, MAX_QUESTION_ECHO_CHARS),\n"
                   "        number(out.p_yes),\n",
                   "        \"**Question:** {}\\n**P(yes):** {} (on 0-1)\\n**Verdict:** {}\",\n"
                   "        clip(&req.question, MAX_QUESTION_ECHO_CHARS),\n"
                   "        number(out.p_yes),\n"
                   "        if out.p_yes >= 0.5 { \"Yes\" } else { \"No\" },\n")],
    },
    "m5_defer_late": {
        "target": "q_defer_first", "module": "main",
        "defect": "handle_choice defers the interaction only after the TypeSafe call",
        "edits": [("src/main.rs",
                   "        cmd.defer(&ctx.http).await?;\n        let started = Instant::now();\n        let text = match self\n",
                   "        let started = Instant::now();\n        let text = match self\n"),
                  ("src/main.rs",
                   "        let answered = Instant::now();\n        cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(text))\n",
                   "        cmd.defer(&ctx.http).await?;\n        let answered = Instant::now();\n        cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(text))\n")],
    },
}

RUNS = ["real1", "real2", "real3", *MUTANTS, "swap"]

# ---- building the state ----------------------------------------------------

def git(*args):
    return subprocess.run(["git", "-C", REPO, *args], check=True, capture_output=True, text=True).stdout


def sources(rev):
    files = sorted(f for f in git("ls-tree", "-r", "--name-only", rev, "--", "src").split() if f.endswith(".rs"))
    return {f: git("show", f"{rev}:{f}") for f in files}


def mutate(src, name):
    out = dict(src)
    for path, old, new in MUTANTS[name]["edits"]:
        n = out[path].count(old)
        if n != 1:
            sys.exit(f"mutant {name}: edit site in {path} found {n} times, expected exactly 1")
        out[path] = out[path].replace(old, new)
    return out


def spec_text(spec_path):
    tasks = json.load(open(spec_path, encoding="utf8"))
    parts = [f"--- Task #{t['id']} ---\nTEXT: {t['text']}\nACCEPT: {t['accept']}" for t in tasks]
    return "\n\n".join(parts)


def impl_text(src):
    return "\n\n".join(f"--- FILE {path} ---\n{body}" for path, body in src.items())


def sections_for(run, spec, src):
    code = impl_text(mutate(src, run) if run in MUTANTS else src)
    s = f"=== SPEC: board tasks #1-#6 (text and acceptance criteria) ===\n{spec}"
    i = f"=== IMPLEMENTATION: every Rust source file of the bot ===\n{code}"
    return s, i


def state_for(run, spec, src):
    s, i = sections_for(run, spec, src)
    return f"{i}\n\n{s}" if run == "swap" else f"{s}\n\n{i}"


def request_for(run, spec, src):
    return {"model": MODEL, "state": state_for(run, spec, src), "questions": QUESTIONS}


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()

# ---- board -----------------------------------------------------------------

def board_post(text):
    """Post a task-8 status to the board; returns its seq or raises."""
    body = json.dumps({"agent": AGENT, "kind": "status", "task_id": TASK_ID, "text": text}).encode()
    req = urllib.request.Request(f"{BOARD}/post", data=body, method="POST",
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode())["seq"]

# ---- calling ---------------------------------------------------------------

def api_key():
    key = os.environ.get("TYPESAFE_API_KEY")
    if not key:
        for line in open(os.path.join(REPO, ".env"), encoding="utf8"):
            if line.startswith("TYPESAFE_API_KEY="):
                key = line.split("=", 1)[1].strip().strip('"').strip("'")
    if not key:
        sys.exit("TYPESAFE_API_KEY not found")
    return key


def calls_made(out):
    path = os.path.join(out, "calls.log")
    return sum(1 for _ in open(path)) if os.path.exists(path) else 0


def call(out, run, body, key):
    if calls_made(out) >= MAX_CALLS:
        sys.exit(f"refusing: {MAX_CALLS} calls already made (hard cap)")
    req = urllib.request.Request(API, data=json.dumps(body).encode(), method="POST", headers={
        "Authorization": f"Bearer {key}", "Content-Type": "application/json",
        "Idempotency-Key": f"jev-eval-{run}-{sha(body['state'])[:16]}"})
    t0 = time.time()
    with open(os.path.join(out, "calls.log"), "a") as log:  # counted even if it fails
        log.write(f"{time.strftime('%FT%TZ', time.gmtime())} {run} state_sha256={sha(body['state'])}\n")
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            status, raw = r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        status, raw = e.code, e.read().decode()
    return status, raw, round(time.time() - t0, 2)

# ---- report ----------------------------------------------------------------

def load(out, run):
    p = os.path.join(out, run, "response.json")
    if not os.path.exists(p):
        return None
    r = json.load(open(p))
    return r if "answers" in r else None


def value(ans):
    if ans["type"] == "noul":
        return ans["noul"]
    if ans["type"] == "score":
        return ans["score"]
    return ans["choice"]


def report(out):
    got = {run: load(out, run) for run in RUNS}
    reals = [got[r] for r in ("real1", "real2", "real3") if got[r]]
    lines = ["| question | real (mean) | real spread | " + " | ".join(MUTANTS) + " | swap |",
             "|---|---|---|" + "---|" * (len(MUTANTS) + 1)]
    for q in QUESTIONS:
        rv = [value(r["answers"][q]) for r in reals]
        numeric = all(isinstance(v, (int, float)) for v in rv) and rv
        mean = sum(rv) / len(rv) if numeric else None
        cells = [f"{mean:.3f}" if numeric else "/".join(map(str, rv)),
                 f"{max(rv) - min(rv):.3f}" if numeric else "-"]
        for m, spec in MUTANTS.items():
            r = got[m]
            if not r:
                cells.append("n/a"); continue
            v = value(r["answers"][q])
            if numeric:
                txt = f"{v:.3f} ({v - mean:+.3f})"
            else:
                txt = str(v)
            if spec["target"] == q:
                txt = f"**{txt}**"
            cells.append(txt)
        s = got["swap"]
        if s:
            v = value(s["answers"][q])
            cells.append(f"{v:.3f} ({v - mean:+.3f})" if numeric else str(v))
        else:
            cells.append("n/a")
        lines.append(f"| {q} | " + " | ".join(cells) + " |")
    tokens = {run: r["usage"]["input_tokens"] for run, r in got.items() if r}
    lines.append("")
    lines.append("input tokens per run: " + ", ".join(f"{k}={v}" for k, v in tokens.items()))
    lines.append(f"total input tokens: {sum(tokens.values())}; cost at $0.042/Mtok: ${sum(tokens.values()) * 0.042 / 1e6:.4f}")
    lines.append("")
    lines.append("targeted question per mutant (drop = mutant P(yes) - real mean; negative means Jev noticed):")
    for m, spec in MUTANTS.items():
        r = got[m]
        q = spec["target"]
        if not r or not reals:
            lines.append(f"- {m}: n/a"); continue
        mean = sum(value(x["answers"][q]) for x in reals) / len(reals)
        spread = max(value(x["answers"][q]) for x in reals) - min(value(x["answers"][q]) for x in reals)
        v = value(r["answers"][q])
        others = [abs(value(r["answers"][o]) - sum(value(x["answers"][o]) for x in reals) / len(reals))
                  for o in QUESTIONS if o != q and QUESTIONS[o]["type"] == "noul"]
        lines.append(f"- {m} ({spec['defect']}): {q} real {mean:.3f} (spread {spread:.3f}) -> {v:.3f}, "
                     f"change {v - mean:+.3f}; other nouls max |change| {max(others):.3f}; "
                     f"defect module {spec['module']}, Jev chose {r['answers']['q_defect_module']['choice']}")
    return "\n".join(lines)

# ---- main ------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["plan", "run", "report"])
    ap.add_argument("runs", nargs="*")
    ap.add_argument("--spec")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    if a.cmd == "report":
        text = report(a.out)
        open(os.path.join(a.out, "results.md"), "w").write(text + "\n")
        print(text)
        return
    spec, src = spec_text(a.spec), sources(REV)
    runs = a.runs or RUNS
    for r in runs:
        if r not in RUNS:
            sys.exit(f"unknown run {r}")
    key = api_key() if a.cmd == "run" else None
    for run in runs:
        d = os.path.join(a.out, run)
        os.makedirs(d, exist_ok=True)
        body = request_for(run, spec, src)
        json.dump(body, open(os.path.join(d, "request.json"), "w"), indent=1)
        info = {"run": run, "rev": REV, "state_chars": len(body["state"]), "state_sha256": sha(body["state"]),
                "questions_sha256": sha(json.dumps(QUESTIONS, sort_keys=True)),
                "approx_tokens_chars_div_3_5": round((len(body["state"]) + len(json.dumps(QUESTIONS))) / 3.5)}
        if run in MUTANTS:
            info["mutant"] = {k: MUTANTS[run][k] for k in ("target", "module", "defect")}
            # Record the planted diff for the reviewer.
            mut = mutate(src, run)
            info["mutated_files"] = [p for p in src if mut[p] != src[p]]
        if a.cmd == "run":
            n = calls_made(a.out) + 1
            s_sec, i_sec = sections_for(run, spec, src)
            order = "IMPLEMENTATION, SPEC" if run == "swap" else "SPEC, IMPLEMENTATION"
            mut = (f" (mutant: {MUTANTS[run]['defect']}; target {MUTANTS[run]['target']})"
                   if run in MUTANTS else "")
            pre = (f"JEV-08 call {n}/{MAX_CALLS} REQUEST, variant {run}{mut}. POST {API}, model {MODEL}."
                   f" State order: {order}. SPEC section {len(s_sec)} chars sha256 {sha(s_sec)};"
                   f" IMPLEMENTATION section {len(i_sec)} chars sha256 {sha(i_sec)};"
                   f" full state {len(body['state'])} chars sha256 {info['state_sha256']}."
                   f" Request file (box): {os.path.join(d, 'request.json')}."
                   f" Questions verbatim: {json.dumps(QUESTIONS)}")
            try:
                info["board_request_seq"] = board_post(pre)
            except Exception as e:  # rule: no call unless the request is on the board
                sys.exit(f"{run}: board post failed ({e}); NOT calling TypeSafe")
            status, raw, secs = call(a.out, run, body, key)
            open(os.path.join(d, "response.json"), "w").write(raw)
            info.update({"http_status": status, "seconds": secs})
            reply = (f"JEV-08 call {n}/{MAX_CALLS} REPLY, variant {run} (request seq {info['board_request_seq']}):"
                     f" HTTP {status}, {secs} s. Reply JSON verbatim: {raw}")
            try:
                info["board_reply_seq"] = board_post(reply)
            except Exception as e:
                json.dump(info, open(os.path.join(d, "info.json"), "w"), indent=1)
                sys.exit(f"{run}: reply saved but board post FAILED ({e}); post {d}/response.json by hand")
            if status != 200:
                print(json.dumps(info))
                sys.exit(f"{run}: HTTP {status}, stopping (see {d}/response.json)")
            info["input_tokens"] = json.loads(raw)["usage"]["input_tokens"]
        json.dump(info, open(os.path.join(d, "info.json"), "w"), indent=1)
        print(json.dumps(info))


if __name__ == "__main__":
    main()
