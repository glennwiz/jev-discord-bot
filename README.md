# jev-discord-bot

Discord bot that asks Jev, TypeSafe's System One decision model
([docs.typesafe.ai](https://docs.typesafe.ai)), typed questions. Slices JEV-01 `/jev choice`,
JEV-02 `/jev score` and JEV-03 `/jev noul`.

```
/jev choice question:<what to decide> options:<a, b, c> [context:<background>]
```

The bot validates the input, defers the interaction (Discord's 3 s window),
makes **one** `POST https://api.typesafe.ai/v1/systemone`, then edits the
deferred reply with the picked option, the probability Jev gave *that option*,
and Jev's separate *confidence*:

```
**Jev picks:** sushi
Probability of this option: 70.0% · Jev confidence: 55.0%
```

Bad input (fewer than 2 or more than 20 options, blank or duplicate options,
over-long text) is answered privately and never reaches Jev.

```
/jev score text:<what to score> question:<what to measure> levels:<lowest, ..., highest>
```

Same flow, one `score` question. The reply shows the score exactly as Jev
sent it (never rounded to a level), where it falls between levels, the ordered
legend numbered like the scale, and Jev's confidence separately:

```
**Jev score:** 1.4 on 0-3 (between soon and urgent)
**Levels:** `0` routine · `1` soon · `2` urgent · `3` critical
Jev confidence: 88.0%
```

Levels must be 2-10, non-blank, distinct (case-insensitive), each at most
100 characters.

```
/jev noul text:<what to judge> question:<yes/no question> [yes_means:<..>] [no_means:<..>]
```

One `noul` question; the reply is P(yes) exactly as Jev sent it, and nothing
else - no Yes/No verdict and no confidence, because Jev returns neither and
the action threshold is the reader's call:

```
**P(yes):** 0.12 (on 0-1)
```

## Layout

| Path | Role |
|---|---|
| `src/choice/input.rs` | Parse and bound the command input |
| `src/choice/jev.rs` | The single bounded HTTP call and typed-answer validation |
| `src/choice/render.rs` | Discord reply text |
| `src/main.rs` | Gateway glue: guild command registration, defer, edit |
| `src/score/input.rs`, `jev.rs`, `render.rs` | The same three roles for `/jev score`; shares no code with `choice/` |
| `src/noul/input.rs`, `jev.rs`, `render.rs` | The same three roles for `/jev noul`; shares no code with `choice/` or `score/` |
| `tests/noul_fake_jev.rs` | Offline noul tests (fake server from `tests/common/`) |
| `tests/score_fake_jev.rs`, `tests/common/` | Offline score tests and their fake Jev server |
| `src/config.rs` | Environment configuration (secrets redacted in `Debug`) |
| `tests/choice_fake_jev.rs` | Offline tests against a fake Jev HTTP server on 127.0.0.1 |

## Jev wire contract: TypeSafe System One API (verified 2026-09-26)

Provider: TypeSafe, `https://api.typesafe.ai` (the default `JEV_BASE_URL`).
Source: <https://docs.typesafe.ai/api>, confirmed by one live call per
feature on 2026-09-26 (evidence below). Keys come from
<https://console.typesafe.ai/keys> and start with `apikey_`.

- `POST /v1/systemone`, `Authorization: Bearer <TYPESAFE_API_KEY>`,
  `Content-Type: application/json`. The bot also sends
  `Idempotency-Key: discord-<interaction id>`; TypeSafe accepts it.
- Request: `{"model":"jev-latest","state":<text>,"questions":{"<name>":<question>}}`.
  `jev-latest` currently resolves to `jev-1.13.0` (reported in the response's `model`).
- Choice: `{"type":"choice","instructions":..,"criteria":{"<option>":"<description>",..}}`
  -> `{"type":"choice","choice":"<option>","probabilities":{"<option>":p,..},"confidence":c}`.
  The reply shows P(picked option) and confidence as separate numbers.
- Score: `{"type":"score","instructions":..,"criteria":["<lowest>",..,"<highest>"]}` (2-10 levels)
  -> `{"type":"score","score":s,"confidence":c,"legend":{"0":..},"probabilities":{"0":p,..}}`.
  **The scale is 0-based**: level `i` is score `i`, `s` lies in `0..=n-1` and may be
  fractional. TypeSafe's reference example and the live call (legend keys
  `"0".."3"`) agree; the base is the single constant
  `score::input::LOWEST_LEVEL_SCORE`.
- Noul: `{"type":"noul","instructions":..}` with optional
  `"criteria":{"true":"<yes means>","false":"<no means>"}` -> `{"type":"noul","noul":p}`,
  P(yes) in `[0, 1]`. A noul has no confidence field; the bot shows only P(yes).
- Errors: 401 (bad key), 403 (no key), 422 (validation), 429 (rate limit),
  529 (overloaded), body `{"detail":{"error_type":..,"message":..}}` (also
  `{"detail":".."}` and a 422 list form). The bot never retries. It shows the
  status and the provider's message, clipped to 300 characters, plus
  "try again shortly" for 429/529.

The bot's own input bounds (20 choice options, 1,800-char question, 8,000-char
state, 2,000-char criteria) are tighter than TypeSafe's limits (255 options,
32k tokens of state plus question) and keep Discord replies short.

### Live evidence (2026-09-26, `tests/live_probe.rs` at d94a5b4, key redacted)

| Feature | Request (state / question) | Response |
|---|---|---|
| choice | "Two of us are vegetarian and we have 30 minutes." / pizza, sushi, tacos | `choice:"sushi"`, probabilities sushi 0.46, pizza 0.42, tacos 0.12, `confidence:0.2` |
| score | "Production database is down for every customer, ..." / routine..critical | `score:3.0`, `confidence:1.0`, legend `"0":"routine".."3":"critical"`, P(3)=1.0 |
| noul | "The customer was billed twice and wants a refund today." / needs a human? | `noul:0.78` |

All three were HTTP 200, model `jev-1.13.0`, 335 / 323 / 285 input tokens.
Re-run (3 paid calls) in tmux `jev:live`:
`cargo test --offline --test live_probe -- --ignored --nocapture --test-threads=1`.

## Build and test (ArchBlackMage test box)

Host: Arch Linux, kernel 7.1.9-arch1-2 x86_64. Rust installed user-local with
`rustup` 1.29.1 (`--profile minimal`, no sudo): rustc 1.98.1, cargo 1.98.1.
The box's free RAM is small because llama-server holds most of it, so builds
use two jobs. Every build/test/live run on the box goes inside tmux session
`jev`, one window per purpose (`build`, `test`, `live`, `review`), with output
tee'd to `~/jev-logs/<window>.log`; the session is never killed:

```sh
mkdir -p ~/jev-logs
tmux has-session -t jev 2>/dev/null || tmux new-session -d -s jev -n build
tmux new-window -t jev -n test 2>/dev/null   # once per window name
tmux send-keys -t jev:test 'cd ~/dev/jev-discord-bot && cargo test --offline 2>&1 | tee ~/jev-logs/test.log' Enter
```

The commands to run there:

```sh
# one-time
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path

cd ~/dev/jev-discord-bot
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=2
cargo fetch                       # the only step that needs the internet
cargo test --offline              # 12 choice + 10 score + 9 noul tests, no Discord / internet / keys
# proof of "offline": no network namespace except loopback
unshare -rn sh -c 'ip link set lo up; cargo test --offline'
cargo build --release --offline
```

## Live run

1. Create a dedicated Discord application/bot for Jev testing (not the
   WhiteMage/DarkMage bots), invite it to the test guild with the
   `applications.commands` and `bot` scopes. No privileged intents are needed.
2. On the box: `cp .env.example .env && chmod 600 .env`, then fill in
   `DISCORD_TOKEN`, `DISCORD_GUILD_ID` and `TYPESAFE_API_KEY`.
3. `./target/release/jev-discord-bot` - logs `registered 1 guild command(s)`.
4. In the test guild: `/jev choice question:Where should the team eat? options:pizza, sushi, tacos`
   and `/jev score text:Billed twice, wants a refund today. question:How urgent is this? levels:routine, soon, urgent, critical`
   and `/jev noul text:Billed twice, wants a refund today. question:Does this need a human right now?`.

Logs carry the interaction id, option count, picked index, probability,
confidence, input tokens and latency - never tokens, keys or the user's text.

## Results

- Offline: see the task's result post on the board for the pinned commit.
- Live: pending credentials.
