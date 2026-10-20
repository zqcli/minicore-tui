//! The starter files written when the TUI directory has no settings yet.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::Home;

const CONFIG: &str = "\
# minicore-tui system settings. Providers and models are in models.toml.
# Every path here is relative to this directory unless it is absolute.

# The profile a new session uses; --profile picks another.
default_profile = \"coding\"

[profiles.coding]
model = \"main\"          # a model id from models.toml
reasoning = \"medium\"    # must be in that model's `reasoning` list
approval = \"ask\"        # ask | auto | read_only
# tools = [\"read\", \"write\", \"edit\", \"apply_patch\", \"bash\"]   # the default
# prompt = \"prompt/other.md\"    # default: prompt/SYSTEM_PROMPT.md

# [ui]
# theme = \"dark\"                  # dark | light
# bell = true
# editor = [\"code\", \"--wait\"]     # default: $VISUAL, then $EDITOR

# [agent]
# binary = \"bin/minicore-agent\"   # default: that file if present, else PATH

# [compaction]                    # Agent defaults shown
# enabled = true
# trigger_percent = 95
# target_percent = 50

# [loop]                          # seconds unless the name says otherwise
# model_timeout_seconds = 600
# tool_timeout_seconds = 1800
# model_retry_attempts = 2
";

/// The example is active text; the file carries it commented out.
pub(super) const MODELS_EXAMPLE: &str = "\
[providers.openai]
api = \"open_ai_responses\"            # the only API the Agent speaks
base_url = \"https://api.openai.com/v1\"
api_key_env = \"OPENAI_API_KEY\"       # the NAME of the variable, never the key

[models.main]                        # the id profiles refer to
provider = \"openai\"
model = \"your-model-id\"              # the name sent to the API; default: the id
context_window = 200000
max_output_tokens = 24000
reasoning = [\"auto\", \"low\", \"medium\", \"high\"]";

const MODELS_HEADER: &str = "\
# Providers and models. Secrets never go here: api_key_env names an
# environment variable. Uncomment the example and fill in the model id,
# base_url and api_key_env, then start minicore-tui again.
# Optional per model: safety_margin_tokens, tools, request_timeout_seconds.

";

const SYSTEM_PROMPT: &str = "\
You are a coding agent working in the user's workspace.

Inspect the code before changing it. Use the tools when they help, make focused
changes, run the relevant checks, and explain the result briefly.
";

/// Writes whichever starter files are missing and returns the ones it created.
pub(super) fn write(home: &Home) -> io::Result<Vec<PathBuf>> {
    let models = format!("{MODELS_HEADER}{}\n", commented(MODELS_EXAMPLE));
    let files = [
        (home.config_file(), CONFIG),
        (home.models_file(), models.as_str()),
        (home.system_prompt_file(), SYSTEM_PROMPT),
    ];
    let mut created = Vec::new();
    for (path, text) in files {
        if create_new(&path, text)? {
            created.push(path);
        }
    }
    Ok(created)
}

fn commented(text: &str) -> String {
    text.lines()
        .map(|line| match line.is_empty() {
            true => "#".to_owned(),
            false => format!("# {line}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn create_new(path: &Path, text: &str) -> io::Result<bool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file.write_all(text.as_bytes()).map(|()| true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// What the first run tells the user before it exits.
pub(super) fn first_run_message(home: &Home, created: &[PathBuf]) -> String {
    let files: String = created
        .iter()
        .map(|path| format!("  {}\n", path.display()))
        .collect();
    format!(
        "minicore-tui: first run, wrote starter files in {}\n{files}\n\
         Next: edit models.toml (uncomment the example; set the model id, base_url and\n\
         api_key_env), export the variable api_key_env names, then run minicore-tui again.\n\
         Profiles are in config.toml, the system prompt in prompt/SYSTEM_PROMPT.md.\n",
        home.dir().display()
    )
}
