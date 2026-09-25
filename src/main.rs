//! Discord Gateway bot: registers `/jev choice`, `/jev score` and `/jev noul`
//! in one test guild and answers them through Jev. Feature logic lives in
//! `choice/`, `score/` and `noul/`; this file is only glue.

mod choice;
mod config;
mod noul;
mod score;

use serenity::all::{
    CommandDataOptionValue, CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse, EventHandler,
    GatewayIntents, GuildId, Interaction, Ready,
};
use serenity::async_trait;
use serenity::Client;

use choice::{render, ChoiceRequest, JevClient};
use config::Config;
use noul::{NoulClient, NoulRequest};
use score::{ScoreClient, ScoreRequest};

struct Handler {
    guild: GuildId,
    jev: JevClient,
    score: ScoreClient,
    noul: NoulClient,
}

fn jev_command() -> CreateCommand {
    let choice = CreateCommandOption::new(CommandOptionType::SubCommand, "choice", "Let Jev pick one of your options")
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "question", "What should be decided?")
                .required(true)
                .max_length(choice::input::MAX_QUESTION_CHARS as u16),
        )
        .add_sub_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "options",
                "2-20 options, separated by commas (or | if options contain commas)",
            )
            .required(true)
            .max_length(choice::input::MAX_CRITERIA_CHARS as u16),
        )
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "context", "Optional background for Jev to weigh")
                .required(false)
                .max_length(4_000),
        );
    let score = CreateCommandOption::new(CommandOptionType::SubCommand, "score", "Let Jev place text on your ordered levels")
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "text", "The text Jev should score")
                .required(true)
                .max_length(4_000),
        )
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "question", "What should be measured?")
                .required(true)
                .max_length(score::input::MAX_QUESTION_CHARS as u16),
        )
        .add_sub_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "levels",
                "2-10 levels, lowest first, separated by commas (or | if levels contain commas)",
            )
            .required(true)
            .max_length(score::input::MAX_CRITERIA_CHARS as u16),
        );
    let noul = CreateCommandOption::new(CommandOptionType::SubCommand, "noul", "Ask Jev for P(yes) on a yes/no question")
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "text", "The text Jev should judge")
                .required(true)
                .max_length(4_000),
        )
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "question", "The yes/no question")
                .required(true)
                .max_length(noul::input::MAX_QUESTION_CHARS as u16),
        )
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "yes_means", "Optional: what counts as yes")
                .required(false)
                .max_length(noul::input::MAX_MEANING_CHARS as u16),
        )
        .add_sub_option(
            CreateCommandOption::new(CommandOptionType::String, "no_means", "Optional: what counts as no")
                .required(false)
                .max_length(noul::input::MAX_MEANING_CHARS as u16),
        );
    CreateCommand::new("jev")
        .description("Ask Jev for a typed decision")
        .add_option(choice)
        .add_option(score)
        .add_option(noul)
}

/// The invoked `/jev` subcommand's name and its string options.
fn sub_args(cmd: &CommandInteraction) -> Option<(&str, Vec<(&str, &str)>)> {
    let sub = cmd.data.options.first()?;
    let CommandDataOptionValue::SubCommand(opts) = &sub.value else {
        return None;
    };
    let strings = opts.iter().filter_map(|o| Some((o.name.as_str(), o.value.as_str()?))).collect();
    Some((sub.name.as_str(), strings))
}

fn arg<'a>(args: &[(&str, &'a str)], name: &str) -> Option<&'a str> {
    args.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

async fn reply_private(ctx: &Context, cmd: &CommandInteraction, text: String) -> serenity::Result<()> {
    let msg = CreateInteractionResponseMessage::new().content(text).ephemeral(true);
    cmd.create_response(&ctx.http, CreateInteractionResponse::Message(msg)).await
}

impl Handler {
    async fn handle_choice(&self, ctx: &Context, cmd: &CommandInteraction, args: &[(&str, &str)]) -> serenity::Result<()> {
        let (Some(question), Some(options)) = (arg(args, "question"), arg(args, "options")) else {
            return Ok(());
        };
        // Bad input never reaches Jev: answer privately and stop.
        let req = match ChoiceRequest::parse(question, options, arg(args, "context")) {
            Ok(r) => r,
            Err(e) => return reply_private(ctx, cmd, render::input_error(&e)).await,
        };
        // Acknowledge inside Discord's 3 s window before the network call.
        cmd.defer(&ctx.http).await?;
        let started = std::time::Instant::now();
        let text = match self.jev.choose(&req, Some(&format!("discord-{}", cmd.id))).await {
            Ok(out) => {
                eprintln!(
                    "choice ok: interaction={} options={} choice_index={:?} p={:.3} confidence={:.3} input_tokens={:?} ms={}",
                    cmd.id,
                    req.options.len(),
                    req.options.iter().position(|o| *o == out.choice),
                    out.probability,
                    out.confidence,
                    out.input_tokens,
                    started.elapsed().as_millis()
                );
                render::outcome(&req, &out)
            }
            Err(e) => {
                eprintln!("choice failed: interaction={} error={} ms={}", cmd.id, e, started.elapsed().as_millis());
                render::jev_error(&e)
            }
        };
        cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(text)).await?;
        Ok(())
    }

    async fn handle_score(&self, ctx: &Context, cmd: &CommandInteraction, args: &[(&str, &str)]) -> serenity::Result<()> {
        let (Some(text), Some(question), Some(levels)) = (arg(args, "text"), arg(args, "question"), arg(args, "levels"))
        else {
            return Ok(());
        };
        // Bad input never reaches Jev: answer privately and stop.
        let req = match ScoreRequest::parse(text, question, levels) {
            Ok(r) => r,
            Err(e) => return reply_private(ctx, cmd, score::render::input_error(&e)).await,
        };
        // Acknowledge inside Discord's 3 s window before the network call.
        cmd.defer(&ctx.http).await?;
        let started = std::time::Instant::now();
        let reply = match self.score.score(&req, Some(&format!("discord-{}", cmd.id))).await {
            Ok(out) => {
                eprintln!(
                    "score ok: interaction={} levels={} score={} confidence={:.3} input_tokens={:?} ms={}",
                    cmd.id,
                    req.levels.len(),
                    out.score,
                    out.confidence,
                    out.input_tokens,
                    started.elapsed().as_millis()
                );
                score::render::outcome(&req, &out)
            }
            Err(e) => {
                eprintln!("score failed: interaction={} error={} ms={}", cmd.id, e, started.elapsed().as_millis());
                score::render::jev_error(&e)
            }
        };
        cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(reply)).await?;
        Ok(())
    }

    async fn handle_noul(&self, ctx: &Context, cmd: &CommandInteraction, args: &[(&str, &str)]) -> serenity::Result<()> {
        let (Some(text), Some(question)) = (arg(args, "text"), arg(args, "question")) else {
            return Ok(());
        };
        // Bad input never reaches Jev: answer privately and stop.
        let req = match NoulRequest::parse(text, question, arg(args, "yes_means"), arg(args, "no_means")) {
            Ok(r) => r,
            Err(e) => return reply_private(ctx, cmd, noul::render::input_error(&e)).await,
        };
        // Acknowledge inside Discord's 3 s window before the network call.
        cmd.defer(&ctx.http).await?;
        let started = std::time::Instant::now();
        let reply = match self.noul.ask(&req, Some(&format!("discord-{}", cmd.id))).await {
            Ok(out) => {
                eprintln!(
                    "noul ok: interaction={} criteria={} p_yes={} input_tokens={:?} ms={}",
                    cmd.id,
                    req.criteria().is_some(),
                    out.p_yes,
                    out.input_tokens,
                    started.elapsed().as_millis()
                );
                noul::render::outcome(&req, &out)
            }
            Err(e) => {
                eprintln!("noul failed: interaction={} error={} ms={}", cmd.id, e, started.elapsed().as_millis());
                noul::render::jev_error(&e)
            }
        };
        cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(reply)).await?;
        Ok(())
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        eprintln!("connected as {} (guild {})", ready.user.name, self.guild);
        match self.guild.set_commands(&ctx.http, vec![jev_command()]).await {
            Ok(cmds) => eprintln!("registered {} guild command(s)", cmds.len()),
            Err(e) => eprintln!("command registration failed: {e}"),
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(cmd) = interaction else {
            return;
        };
        if cmd.data.name != "jev" || cmd.guild_id != Some(self.guild) {
            return;
        }
        let Some((sub, args)) = sub_args(&cmd) else {
            return;
        };
        let result = match sub {
            "choice" => self.handle_choice(&ctx, &cmd, &args).await,
            "score" => self.handle_score(&ctx, &cmd, &args).await,
            "noul" => self.handle_noul(&ctx, &cmd, &args).await,
            _ => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("discord reply failed: interaction={} error={e}", cmd.id);
        }
    }
}

#[tokio::main]
async fn main() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    eprintln!("config: {cfg:?}");
    let jev = JevClient::new(&cfg.jev_base_url, &cfg.jev_api_key, cfg.jev_timeout).unwrap_or_else(|e| {
        eprintln!("jev client error: {e}");
        std::process::exit(2);
    });
    let score = ScoreClient::new(&cfg.jev_base_url, &cfg.jev_api_key, cfg.jev_timeout).unwrap_or_else(|e| {
        eprintln!("jev client error: {e}");
        std::process::exit(2);
    });
    let noul = NoulClient::new(&cfg.jev_base_url, &cfg.jev_api_key, cfg.jev_timeout).unwrap_or_else(|e| {
        eprintln!("jev client error: {e}");
        std::process::exit(2);
    });
    let handler = Handler { guild: GuildId::new(cfg.guild_id), jev, score, noul };
    // Slash commands arrive without privileged or message intents.
    let mut client = Client::builder(&cfg.discord_token, GatewayIntents::empty())
        .event_handler(handler)
        .await
        .unwrap_or_else(|e| {
            eprintln!("discord client error: {e}");
            std::process::exit(1);
        });
    if let Err(e) = client.start().await {
        eprintln!("gateway stopped: {e}");
        std::process::exit(1);
    }
}
