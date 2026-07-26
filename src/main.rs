mod llm;
mod mistralrs_backend;
mod settings;

use std::io::Write;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use llm::{Backend, Message, Request, Role, Sampling};
use mistralrs_backend::MistralRsBackend;
use settings::Settings;

#[derive(Parser)]
#[command(name = "cairnworld")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive stdio conversation with the model.
    Chat {
        /// Path to a local GGUF model file, e.g. models/model.gguf. Falls
        /// back to the `model` key in local.toml/default.toml if omitted.
        #[arg(long)]
        model: Option<String>,
        /// Sampling temperature.
        #[arg(long, default_value_t = 1.0)]
        temperature: f32,
        /// Optional system prompt.
        #[arg(long)]
        system: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Chat {
            model,
            temperature,
            system,
        } => {
            let model = match model {
                Some(model) => model,
                None => Settings::load()?
                    .model
                    .context("no --model given and no `model` key set in local.toml/default.toml")?,
            };
            run_chat(&model, temperature, system).await
        }
    }
}

async fn run_chat(model: &str, temperature: f32, system: Option<String>) -> Result<()> {
    eprintln!("Loading model from {model}...");
    let backend = MistralRsBackend::load(model)
        .await
        .context("failed to load model")?;
    eprintln!("Model loaded. Type a message, or /quit to exit.");

    let mut history = Vec::new();
    if let Some(system) = system {
        history.push(Message {
            role: Role::System,
            content: system,
        });
    }

    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("> ");
        std::io::stdout().flush().context("flushing stdout")?;

        line.clear();
        let bytes_read = stdin.read_line(&mut line).context("reading from stdin")?;
        if bytes_read == 0 {
            break;
        }
        let text = line.trim_end_matches('\n');
        if text == "/quit" {
            break;
        }

        history.push(Message {
            role: Role::User,
            content: text.to_string(),
        });

        let request = Request {
            messages: history.clone(),
            tools: vec![],
            sampling: Sampling { temperature },
        };

        let mut reply = String::new();
        let response = backend
            .complete(request, |token| {
                print!("{token}");
                let _ = std::io::stdout().flush();
                reply.push_str(token);
            })
            .await
            .context("inference failed")?;
        println!();

        let llm::Content::Text(text) = response.content else {
            anyhow::bail!("expected text content; tool calls are not supported in this milestone");
        };
        debug_assert_eq!(text, reply);

        history.push(Message {
            role: Role::Assistant,
            content: text,
        });
    }

    Ok(())
}
