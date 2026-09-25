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
| `src/main.rs` | Gateway glue: guild command registration, defer, edit, graceful shutdown |
| `src/config.rs` | Environment configuration (secrets redacted in `Debug`) |
| `src/choice/input.rs` | Parse and bound the command input |
| `src/choice/jev.rs` | The single bounded HTTP call and typed-answer validation |
| `src/choice/render.rs` | Discord reply text |
| `src/score/input.rs`, `jev.rs`, `render.rs` | The same three roles for `/jev score`; shares no code with `choice/` |
| `src/noul/input.rs`, `jev.rs`, `render.rs` | The same three roles for `/jev noul`; shares no code with `choice/` or `score/` |
| `tests/{choice,score,noul}_fake_jev.rs` | Offline tests against a fake Jev HTTP server on 127.0.0.1 (`tests/common/`) |
| `tests/live_probe.rs` | Ignored by default: one paid TypeSafe call per feature |
| `deploy/jev-discord-bot.service` | Hardened systemd unit |

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

## Setup

1. **Discord application.** In the Discord developer portal create a new
   application with a bot user, dedicated to Jev (never reuse the
   WhiteMage/DarkMage tokens). Copy its token. No privileged intents are
   needed: slash commands arrive without them.
2. **Invite** the bot to your guild with the `bot` and
   `applications.commands` scopes (no extra permissions). Copy the guild id
   (Developer Mode -> right-click the server -> Copy Server ID).
3. **TypeSafe key** from <https://console.typesafe.ai/keys> (`apikey_...`).

The bot registers `/jev` in that one guild when it connects (guild commands
appear immediately; there is no global registration).

## Configuration

All configuration is environment variables. For a local run they may sit in
a `.env` file in the working directory (`cp .env.example .env && chmod 600 .env`);
under systemd they live in `/etc/jev-discord-bot/env`.

| Variable | Required | Meaning |
|---|---|---|
| `DISCORD_TOKEN` | yes | Bot token |
| `DISCORD_GUILD_ID` | yes | Numeric guild id where `/jev` is registered |
| `TYPESAFE_API_KEY` | yes | TypeSafe API key |
| `JEV_BASE_URL` | no | Default `https://api.typesafe.ai` |
| `JEV_TIMEOUT_SECS` | no | Whole-call timeout per Jev request, 1-60, default 20 |

A missing or invalid value exits with status 2 and a `config error: ...`
line naming the variable (never its value). Secrets are never logged: the
startup `config:` line prints them as `<redacted>`.

## Local run and tests

```sh
cargo fetch                       # the only step that needs the internet
cargo test --offline              # 35 tests: 13 choice, 10 score, 12 noul; no Discord, network or keys
cargo fmt --check
cargo clippy --offline --all-targets -- -D warnings
cargo build --release --offline
./target/release/jev-discord-bot  # reads .env; logs "registered 1 guild command(s)"
```

`cargo test` needs no network: it also passes inside
`unshare -rn sh -c 'ip link set lo up; cargo test --offline'`. The live probe
(`tests/live_probe.rs`, 3 paid calls) only runs with `-- --ignored`.

One example per command, as typed in Discord:

```
/jev choice question:Where should the team eat? options:pizza, sushi, tacos context:Two of us are vegetarian.
/jev score text:Billed twice, wants a refund today. question:How urgent is this? levels:routine, soon, urgent, critical
/jev noul text:Billed twice, wants a refund today. question:Does this need a human right now?
```

### ArchBlackMage test box

Arch Linux x86_64, kernel 7.1.9-arch1-2; Rust user-local via `rustup` 1.29.1
(`--profile minimal` plus `rustfmt`, `clippy`, target `aarch64-unknown-linux-gnu`;
no sudo): rustc/cargo 1.98.1. llama-server holds most of the RAM, so builds
use `CARGO_BUILD_JOBS=2` and scratch goes on disk (`/tmp` is RAM-backed).
Every build/test/live run goes in tmux session `jev`, one window per purpose
(`build`, `test`, `live`, `review`), output tee'd to `~/jev-logs/<window>.log`;
the session is never killed:

```sh
mkdir -p ~/jev-logs
tmux has-session -t jev 2>/dev/null || tmux new-session -d -s jev -n build
tmux new-window -t jev -n test 2>/dev/null   # once per window name
tmux send-keys -t jev:test 'cd ~/dev/jev-discord-bot && cargo test --offline 2>&1 | tee ~/jev-logs/test.log' Enter
```

## Build for the Raspberry Pi (ARM64)

Either path gives a binary for `aarch64-unknown-linux-gnu`.

- **Native, on the Pi** (simplest; 64-bit Raspberry Pi OS): install rustup
  there, then `cargo build --release --locked`. Use `CARGO_BUILD_JOBS=2` on
  small boards.
- **Cross, from an x86_64 Linux box, no sudo** (verified on ArchBlackMage
  2026-09-26: produced an `ELF 64-bit LSB pie executable, ARM aarch64`):

  ```sh
  rustup target add aarch64-unknown-linux-gnu
  mkdir -p ~/opt && curl -sSfL https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz | tar -xJ -C ~/opt
  mv ~/opt/zig-linux-x86_64-0.13.0 ~/opt/zig && export PATH=$HOME/opt/zig:$PATH
  cargo install --locked cargo-zigbuild
  cargo zigbuild --release --locked --target aarch64-unknown-linux-gnu.2.36
  # -> target/aarch64-unknown-linux-gnu/release/jev-discord-bot
  ```

  Plain `cargo build --target aarch64-...` fails without an
  `aarch64-linux-gnu-gcc`, because `ring` (TLS) compiles C; zig supplies
  that. The `.2.36` suffix pins glibc 2.36 (Debian 12 / Raspberry Pi OS
  bookworm) so the binary does not need a newer glibc than the Pi has.

**Left for JEV-05 (on the device):** run the binary on the Pi itself
(glibc/loader match, TLS to Discord and TypeSafe, memory under
`MemoryMax=128M`), and the systemd install below on real hardware.

## Deploy (systemd)

The unit `deploy/jev-discord-bot.service` runs the bot as the dedicated,
login-less user `jevbot`, reads secrets only from a root-owned env file, and
applies systemd sandboxing (read-only system, no home access, no new
privileges, IPv4/IPv6/Unix sockets only, `MemoryMax=128M`).

```sh
# once
sudo useradd --system --no-create-home --shell /usr/sbin/nologin jevbot
sudo install -d -m 0755 /opt/jev-discord-bot
sudo install -d -m 0750 -o root -g jevbot /etc/jev-discord-bot
sudo install -m 0640 -o root -g jevbot /dev/null /etc/jev-discord-bot/env
sudoedit /etc/jev-discord-bot/env       # the variables from "Configuration"
sudo install -m 0644 deploy/jev-discord-bot.service /etc/systemd/system/
sudo install -m 0644 README.md /opt/jev-discord-bot/

# install a release (versioned file + symlink, so rollback is a relink)
V=$(git rev-parse --short HEAD)
sudo install -m 0755 target/release/jev-discord-bot /opt/jev-discord-bot/jev-discord-bot-$V
sudo ln -sfn jev-discord-bot-$V /opt/jev-discord-bot/jev-discord-bot
sudo systemctl daemon-reload
sudo systemctl enable --now jev-discord-bot
```

**Update:** build the new commit, install it as `jev-discord-bot-<sha>`,
relink, then `sudo systemctl restart jev-discord-bot`. The restart sends
SIGTERM: the bot closes the gateway, lets replies already in progress
finish for up to 15 s, logs `shutdown: clean`, and exits; systemd waits up
to 30 s (`TimeoutStopSec`).

**Rollback:** `ls /opt/jev-discord-bot/` to see the kept versions, then
`sudo ln -sfn jev-discord-bot-<previous-sha> /opt/jev-discord-bot/jev-discord-bot && sudo systemctl restart jev-discord-bot`.
Keep the last two or three versions; delete older ones by hand.

**Rotate a secret:** `sudoedit /etc/jev-discord-bot/env`, then restart.

Restart policy: `on-failure` every 5 s, at most 5 starts in 5 minutes; a
config error (exit 2) is not restarted, because restarting cannot fix it.

## Logs

The bot writes one line per event to stderr; under systemd that is the
journal:

```sh
journalctl -u jev-discord-bot -f              # follow
journalctl -u jev-discord-bot --since today   # today's
```

Lines to expect: `config: Config { ..<redacted>.. }`, `connected as <bot> (guild <id>)`,
`registered 1 guild command(s)`, per command `choice ok|score ok|noul ok: interaction=<id> ...`
(counts, numbers, input tokens and latency) or `... failed: interaction=<id> error=...`,
`discord reply failed: ...`, and on stop `shutdown: SIGTERM received` then
`shutdown: clean`. Logs never contain tokens, keys or the users' text.

## Failure behaviour

- Bad input is answered privately and never reaches TypeSafe.
- A TypeSafe timeout, non-2xx or malformed answer becomes a user-visible
  "Jev ... failed: ..." reply; the process keeps running. There are no
  retries, so one command is at most one TypeSafe call.
- A Discord reply failure (defer or edit) is logged as `discord reply failed`
  and dropped. If the defer fails, TypeSafe is not called.
- serenity handles each Discord event in its own task, so even an unexpected
  panic ends that one interaction, not the bot.

## Results

- Offline: fixed commits and evidence are on the board (#1-#4).
- Live Jev contract: see "Live evidence" above.
- Live Discord smoke: pending `DISCORD_GUILD_ID`.
