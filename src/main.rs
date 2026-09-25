//! Discord Gateway bot: registers `/jev choice` in one test guild and answers
//! it through Jev. Choice logic lives in `choice/`; this file is only glue.

mod choice;
mod config;

use serenity::all::{
    CommandDataOptionValue, CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse, EventHandler,
    GatewayIntents, GuildId, Interaction, Ready,
};
use serenity::async_trait;
use serenity::Client;

use choice::{render, ChoiceRequest, JevClient};
use config::Config;

struct Handler {
    guild: GuildId,
    jev: JevClient,
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
    CreateCommand::new("jev").description("Ask Jev for a typed decision").add_option(choice)
}

/// String sub-options of `/jev choice`, by name.
fn choice_args(cmd: &CommandInteraction) -> Option<(String, String, Option<String>)> {
    let sub = cmd.data.options.iter().find(|o| o.name == "choice")?;
    let CommandDataOptionValue::SubCommand(opts) = &sub.value else {
        return None;
    };
    let get = |name: &str| {
        opts.iter()
            .find(|o| o.name == name)
            .and_then(|o| o.value.as_str())
            .map(str::to_string)
    };
    Some((get("question")?, get("options")?, get("context")))
}

impl Handler {
    async fn handle_choice(&self, ctx: &Context, cmd: &CommandInteraction) -> serenity::Result<()> {
        let Some((question, options, context)) = choice_args(cmd) else {
            return Ok(());
        };
        // Bad input never reaches Jev: answer privately and stop.
        let req = match ChoiceRequest::parse(&question, &options, context.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                let msg = CreateInteractionResponseMessage::new().content(render::input_error(&e)).ephemeral(true);
                return cmd.create_response(&ctx.http, CreateInteractionResponse::Message(msg)).await;
            }
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
        if let Err(e) = self.handle_choice(&ctx, &cmd).await {
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
    let handler = Handler { guild: GuildId::new(cfg.guild_id), jev };
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
