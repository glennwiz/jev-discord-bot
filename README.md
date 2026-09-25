# jev-discord-bot

Discord bot that asks [Jev](https://jevmodel.org) (TypeSafe's System One
decision model, via jevmodel.org) typed questions. Slice JEV-01: `/jev choice`.

```
/jev choice question:<what to decide> options:<a, b, c> [context:<background>]
```

The bot validates the input, defers the interaction (Discord's 3 s window),
makes **one** `POST https://jevmodel.org/v1/systemone`, then edits the
deferred reply with the picked option, the probability Jev gave *that option*,
and Jev's separate *confidence*:

```
**Jev picks:** sushi
Probability of this option: 70.0% · Jev confidence: 55.0%
```

Bad input (fewer than 2 or more than 20 options, blank or duplicate options,
over-long text) is answered privately and never reaches Jev.

## Layout

| Path | Role |
|---|---|
| `src/choice/input.rs` | Parse and bound the command input |
| `src/choice/jev.rs` | The single bounded HTTP call and typed-answer validation |
| `src/choice/render.rs` | Discord reply text |
| `src/main.rs` | Gateway glue: guild command registration, defer, edit |
| `src/config.rs` | Environment configuration (secrets redacted in `Debug`) |
| `tests/choice_fake_jev.rs` | Offline tests against a fake Jev HTTP server on 127.0.0.1 |

## Jev wire contract (verified 2026-09-25)

Source: <https://jevmodel.org/docs> ("Updated September 24, 2026") and a
live unauthenticated probe, which returned `401` with
`{"error":{"type":"authentication_error","message":"Missing or invalid API key. ..."}}`.

- `POST /v1/systemone`, `Authorization: Bearer sk-...`, `Content-Type: application/json`,
  optional `Idempotency-Key` (at most 100 characters; the bot sends `discord-<interaction id>`).
- Request: `{"model":"jev-latest","state":<text>,"questions":{"pick":{"type":"choice","instructions":<question>,"criteria":{"<option>":"<option>",...}}}}`.
- Limits: 1-8 questions; serialized state 8,000 chars; instructions 1,800;
  choice criteria 2-20 keys, serialized 2,000 chars; 120 requests/min per key.
- 200: `{"model":..,"answers":{"pick":{"type":"choice","choice":"<key>","probabilities":{"<key>":p,..},"confidence":c}},"usage":{"input_tokens":n,"output_tokens":m}}`.
- Errors: `{"error":{"type","message"}}` - 401 `authentication_error`,
  402 `insufficient_credits`, 422 `invalid_request_error`, 429
  `rate_limit_error`, 502 `upstream_error`. None are billed. The bot does not
  retry; it reports the status to the user.

Not verifiable without a key: whether option keys with spaces/punctuation are
accepted (docs put no rule on criteria keys) and the real answer values. The
live test settles both.

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
cargo test --offline              # 12 tests, no Discord / internet / keys
# proof of "offline": no network namespace except loopback
unshare -rn sh -c 'ip link set lo up; cargo test --offline'
cargo build --release --offline
```

## Live run

1. Create a dedicated Discord application/bot for Jev testing (not the
   WhiteMage/DarkMage bots), invite it to the test guild with the
   `applications.commands` and `bot` scopes. No privileged intents are needed.
2. On the box: `cp .env.example .env && chmod 600 .env`, then fill in
   `DISCORD_TOKEN`, `DISCORD_GUILD_ID` and `JEVMODEL_API_KEY`.
3. `./target/release/jev-discord-bot` - logs `registered 1 guild command(s)`.
4. In the test guild: `/jev choice question:Where should the team eat? options:pizza, sushi, tacos`.

Logs carry the interaction id, option count, picked index, probability,
confidence, input tokens and latency - never tokens, keys or the user's text.

## Results

- Offline: see the task's result post on the board for the pinned commit.
- Live: pending credentials.
