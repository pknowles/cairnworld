mod context;
mod llm;
mod mistralrs_backend;
mod settings;
mod store;

use std::{io::Write, path::Path};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use llm::{Message, Role, Sampling};
use mistralrs_backend::MistralRsBackend;
use settings::Settings;
use store::{RecordedOutcome, Store};

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
        /// SQLite database used to record every completion.
        #[arg(long, default_value = "cairnworld.sqlite")]
        database: String,
    },
    /// Re-run one recorded inference after validating its reconstructed input.
    Replay {
        /// Database containing the inference record.
        #[arg(long, default_value = "cairnworld.sqlite")]
        database: String,
        /// Path to a local GGUF model file. Falls back to `model` in settings.
        #[arg(long)]
        model: Option<String>,
        /// Inference record ID to reconstruct and re-run.
        inference_id: i64,
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
            database,
        } => {
            run_chat(
                &resolve_model(model)?,
                temperature,
                system,
                Path::new(&database),
            )
            .await
        }
        Command::Replay {
            database,
            model,
            inference_id,
        } => run_replay(&resolve_model(model)?, Path::new(&database), inference_id).await,
    }
}

fn resolve_model(model: Option<String>) -> Result<String> {
    match model {
        Some(model) => Ok(model),
        None => Settings::load()?
            .model
            .context("no --model given and no `model` key set in local.toml/default.toml"),
    }
}

async fn backend(model: &str) -> Result<MistralRsBackend> {
    eprintln!("Loading model from {model}...");
    MistralRsBackend::load(model)
        .await
        .context("failed to load model")
}

async fn run_chat(
    model: &str,
    temperature: f32,
    system: Option<String>,
    database: &Path,
) -> Result<()> {
    let store = Store::open(database)
        .await
        .context("opening inference store")?;
    let world = store
        .create_world("chat sandbox")
        .await
        .context("creating chat sandbox world")?;
    let agent = store
        .create_agent(world, "sandbox", "chat")
        .await
        .context("creating chat sandbox agent")?;
    let backend = backend(model).await?;
    eprintln!("Model loaded. Type a message, or /quit to exit.");

    let static_messages = system
        .into_iter()
        .map(|content| Message {
            role: Role::System,
            content,
        })
        .collect::<Vec<_>>();

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

        store
            .append_message(
                agent,
                &Message {
                    role: Role::User,
                    content: text.to_string(),
                },
            )
            .await
            .context("storing chat message")?;

        let mut reply = String::new();
        let response = context::complete(
            &store,
            &backend,
            agent,
            &static_messages,
            Sampling { temperature },
            model,
            |token| {
                print!("{token}");
                let _ = std::io::stdout().flush();
                reply.push_str(token);
            },
        )
        .await
        .context("inference failed")?;
        println!();

        let llm::Content::Text(text) = response.content else {
            anyhow::bail!("expected text content; tool calls are not supported in this milestone");
        };
        debug_assert_eq!(text, reply);

        store
            .append_message(
                agent,
                &Message {
                    role: Role::Assistant,
                    content: text,
                },
            )
            .await
            .context("storing assistant response")?;
    }

    Ok(())
}

async fn run_replay(model: &str, database: &Path, inference_id: i64) -> Result<()> {
    let store = Store::open(database)
        .await
        .context("opening inference store")?;
    let recorded = store
        .reconstruct_inference(inference_id)
        .await
        .with_context(|| format!("reconstructing inference {inference_id}"))?;
    println!(
        "Recorded request:\n{}",
        serde_json::to_string_pretty(&recorded.request)?
    );
    match &recorded.outcome {
        RecordedOutcome::Response(response) => println!(
            "Recorded output:\n{}",
            serde_json::to_string_pretty(response)?
        ),
        RecordedOutcome::Error(error) => println!("Recorded error:\n{error}"),
    }
    println!("Replayed output:");
    let backend = backend(model).await?;
    let response = context::complete_recipe(
        &store,
        &backend,
        recorded.agent_id,
        &recorded.segments,
        recorded.request.sampling,
        model,
        |token| {
            print!("{token}");
            let _ = std::io::stdout().flush();
        },
    )
    .await
    .context("replaying recorded inference")?;
    println!(
        "\nReplay usage: {} input tokens, {} output tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );
    Ok(())
}
