//! First-run templates use the Host's native JSON configuration types.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use minicore_host_core::{HostConfig, LogOutput, LogSettings, OpenAiModelApi, OpenAiModelSettings};
use minicore_host_protocol::{AgentConfig, Role, SessionDefaults, ToolBinding};

use super::Home;

const CONFIG: &str = "\
# Terminal preferences. Execution settings use native Host JSON files.
# host-config.json initializes a new Host; saved Host configuration wins later.
# agent-defaults.json supplies Role and defaults only when creating an Agent.

[ui]
theme = \"dark\"                  # dark | light
bell = true
# editor = [\"code\", \"--wait\"]  # default: $VISUAL, then $EDITOR
";

pub(super) fn write(home: &Home) -> io::Result<Vec<PathBuf>> {
    let tools = ["read", "write", "edit", "apply_patch", "bash", "agent"]
        .map(|name| ToolBinding { name: name.into() })
        .to_vec();
    let model: minicore_host_protocol::ModelRef =
        "provider/chat".parse().expect("literal model reference");
    let log_path = home.dir().join("minicore-tui.log");
    let log_path = log_path
        .to_str()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "TUI home must be representable as UTF-8 for the native logging path",
            )
        })?
        .to_owned();
    let host = HostConfig {
        name: "MiniCore TUI".into(),
        models: vec![OpenAiModelSettings {
            model: model.clone(),
            api: OpenAiModelApi::ChatCompletions,
            endpoint: "https://provider.example.invalid/v1/chat/completions".into(),
            provider_model: "replace-with-provider-model".into(),
            api_key_env: "MINICORE_MODEL_KEY".into(),
            context_window: 128_000,
            supported_reasoning: vec![Default::default()],
            supports_tools: true,
            request_timeout_ms: Some(120_000),
        }],
        tools: tools.clone(),
        logging: LogSettings {
            output: LogOutput::File { path: log_path },
            ..LogSettings::default()
        },
        ..HostConfig::default()
    };
    let agent = AgentConfig {
        role: Role {
            name: "Coding assistant".into(),
            description: "Works in the selected workspace".into(),
            instructions: "Inspect the code before changing it. Make focused changes, run relevant checks, and explain the result clearly.".into(),
        },
        session_defaults: SessionDefaults {
            model,
            tools,
            ..SessionDefaults::default()
        },
    };
    let host = json(&host)?;
    let agent = json(&agent)?;
    let mut created = Vec::new();
    for (path, text) in [
        (home.config_file(), CONFIG),
        (home.host_config_file(), host.as_str()),
        (home.agent_defaults_file(), agent.as_str()),
    ] {
        if create_new(&path, text)? {
            created.push(path);
        }
    }
    std::fs::create_dir_all(home.commands_dir())?;
    Ok(created)
}

fn json(value: &impl serde::Serialize) -> io::Result<String> {
    serde_json::to_string_pretty(value)
        .map(|text| format!("{text}\n"))
        .map_err(io::Error::other)
}

fn create_new(path: &Path, text: &str) -> io::Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn first_run_message(home: &Home, created: &[PathBuf]) -> String {
    let files = created
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Created initial configuration:\n{files}\n\n\
Edit {} with your provider's full endpoint, model and declared capabilities.\n\
Set api to chat_completions or responses; the Host never guesses or falls back.\n\
api_key_env names an environment variable: export the key there, never in JSON.\n\
Edit {} for the initial Agent's Role and Session defaults.\n\
A new Session inherits its Agent defaults; explicit Session settings override them.\n\
Run minicore-tui again when ready. Host data is stored in {}.\n\
Saved Host/Agent/Session configuration takes precedence on later launches.\n\
Use an explicit Host reload to publish configuration changes; files are not watched.\n\
The old minicore-agent session store is not imported or modified.",
        home.host_config_file().display(),
        home.agent_defaults_file().display(),
        home.data_root().display()
    )
}
