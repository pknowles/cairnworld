mod llm;
mod mistralrs_backend;
mod recording_backend;
mod settings;
mod store;

use std::{io::Write, path::Path};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use llm::{Backend, Message, Request, Role, Sampling};
use mistralrs_backend::MistralRsBackend;
use recording_backend::RecordingBackend;
use settings::Settings;
use store::Store;

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

async fn recording_backend(
    model: &str,
    database: &Path,
) -> Result<RecordingBackend<MistralRsBackend>> {
    let store = Store::open(database)
        .await
        .context("opening inference store")?;
    eprintln!("Loading model from {model}...");
    let backend = MistralRsBackend::load(model)
        .await
        .context("failed to load model")?;
    Ok(RecordingBackend::new(backend, store, model.to_string()))
}

async fn run_chat(
    model: &str,
    temperature: f32,
    system: Option<String>,
    database: &Path,
) -> Result<()> {
    let backend = recording_backend(model, database).await?;
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
    println!(
        "Recorded output:\n{}",
        serde_json::to_string_pretty(&recorded.response)?
    );
    println!("Replayed output:");
    let backend = recording_backend(model, database).await?;
    let response = backend
        .complete(recorded.request, |token| {
            print!("{token}");
            let _ = std::io::stdout().flush();
        })
        .await
        .context("replaying recorded inference")?;
    println!(
        "\nReplay usage: {} input tokens, {} output tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );
    Ok(())
}
