mod agent;
mod compaction;
mod context;
mod inference;
mod llm;
mod mistralrs_backend;
mod settings;
mod store;
mod tools;

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rustyline::{DefaultEditor, error::ReadlineError};

use inference::InferenceScheduler;
use llm::{Backend, Message, Role, Sampling};
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
        /// Configured model name, e.g. llama, hermes, qwen. A GGUF path also
        /// works. Falls back to the `model` key in local.toml/default.toml.
        #[arg(long)]
        model: Option<String>,
        /// Sampling temperature.
        #[arg(long, default_value_t = 1.0)]
        temperature: f32,
        /// Enable the model's reasoning mode when its template supports it.
        #[arg(long)]
        enable_thinking: bool,
        /// Optional system prompt.
        #[arg(long)]
        system: Option<String>,
        /// SQLite database used to record every completion.
        #[arg(long, default_value = "cairnworld.sqlite")]
        database: String,
        /// Chat template overriding both the GGUF's and the configured one.
        #[arg(long)]
        chat_template: Option<PathBuf>,
    },
    /// Re-run one recorded inference after validating its reconstructed input.
    Replay {
        /// Database containing the inference record.
        #[arg(long, default_value = "cairnworld.sqlite")]
        database: String,
        /// Path to a local GGUF model file. Falls back to `model` in settings.
        #[arg(long)]
        model: Option<String>,
        /// Chat template overriding the one in the GGUF.
        #[arg(long)]
        chat_template: Option<PathBuf>,
        /// Inference record ID to reconstruct and re-run.
        inference_id: i64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let settings = Settings::load()?;
    match cli.command {
        Command::Chat {
            model,
            temperature,
            enable_thinking,
            system,
            database,
            chat_template,
        } => {
            run_chat(
                resolve_model(model.as_deref(), chat_template, &settings)?,
                temperature,
                enable_thinking,
                system,
                Path::new(&database),
                settings.limits,
            )
            .await
        }
        Command::Replay {
            database,
            model,
            chat_template,
            inference_id,
        } => {
            run_replay(
                resolve_model(model.as_deref(), chat_template, &settings)?,
                Path::new(&database),
                inference_id,
                settings.limits,
            )
            .await
        }
    }
}

/// A `--chat-template` on the command line wins over the configured one, so a
/// template can be tried against any model without editing configuration.
fn resolve_model(
    requested: Option<&str>,
    chat_template: Option<PathBuf>,
    settings: &Settings,
) -> Result<settings::Model> {
    let mut model = settings.model(requested)?;
    if chat_template.is_some() {
        model.chat_template = chat_template;
    }
    Ok(model)
}

async fn backend(model: &settings::Model, limits: settings::Limits) -> Result<MistralRsBackend> {
    eprintln!("Loading model from {}...", model.path);
    MistralRsBackend::load(
        &model.path,
        model.chat_template.as_deref(),
        limits.max_concurrent_inferences,
    )
    .await
    .context("failed to load model")
}

async fn run_chat(
    model: settings::Model,
    temperature: f32,
    enable_thinking: bool,
    system: Option<String>,
    database: &Path,
    limits: settings::Limits,
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
    let scheduler = InferenceScheduler::new(backend(&model, limits).await?, limits)
        .context("configuring inference scheduling")?;
    scheduler
        .resume(&store)
        .await
        .context("resuming deferred compactions")?;
    let backend = scheduler.foreground();
    eprintln!("Model loaded. Type a message, or /quit to exit.");

    let static_messages = system
        .into_iter()
        .map(|content| Message::text(Role::System, content))
        .collect::<Vec<_>>();
    let tools = [tools::save()];

    let mut editor = DefaultEditor::new().context("starting chat line editor")?;
    loop {
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(error) => return Err(error).context("reading chat message"),
        };
        let text = line.as_str();
        if text.is_empty() {
            continue;
        }
        if text == "/quit" {
            break;
        }
        editor
            .add_history_entry(text)
            .context("saving chat input in line-editor history")?;

        // Do this before appending the next user row: an earlier compaction
        // must never see a newer chat message in the history it summarizes.
        backend
            .before_agent(&store, agent)
            .await
            .context("finishing earlier deferred work for this chat")?;

        store
            .append_message(agent, &Message::text(Role::User, text))
            .await
            .context("storing chat message")?;

        eprintln!("[model active]");
        // Each REPL turn is one external trigger, so it gets its own budget.
        let mut budget = agent::Budget::new(limits);
        let response = agent::complete(
            &store,
            &backend,
            &mut budget,
            agent,
            &static_messages,
            &tools,
            Sampling {
                temperature,
                enable_thinking,
            },
            &model.path,
            |token| {
                print!("{token}");
                let _ = std::io::stdout().flush();
            },
            |activity| eprintln!("\n[developer] {activity}"),
        )
        .await
        .context("resolving chat turn")?;
        // A turn may run several inferences, so streamed output spans all of
        // them while `response` is only the last; the store holds the record.
        let llm::Content::Text(_) = response.content else {
            anyhow::bail!("agent loop returned tool calls as its final response");
        };
        println!();
    }

    Ok(())
}

async fn run_replay(
    model: settings::Model,
    database: &Path,
    inference_id: i64,
    limits: settings::Limits,
) -> Result<()> {
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
    let backend = backend(&model, limits).await?;
    let response = context::complete_recipe(
        &store,
        &backend,
        recorded.agent_id,
        &recorded.segments,
        recorded.request.sampling,
        &model.path,
        |token| {
            print!("{token}");
            let _ = std::io::stdout().flush();
        },
    )
    .await
    .context("replaying recorded inference")?;
    let response = response.response;
    println!(
        "\nReplay usage: {} input tokens, {} output tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );
    Ok(())
}
