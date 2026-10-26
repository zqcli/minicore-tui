//! Prompt templates: `prompt/commands/*.md` in the TUI directory, each a
//! `/name` that expands to the file's text before it is sent.

use std::path::Path;

use super::commands::{COMMANDS, Command};

pub struct Template {
    pub name: String,
    pub description: String,
    /// The front matter's `argument-hint`, shown like a command's arguments.
    pub hint: String,
    body: String,
}

impl Template {
    pub fn parse(name: String, text: &str) -> Self {
        let (meta, body) = split_front_matter(text);
        let field = |key: &str| {
            meta.lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.trim() == key)
                .map(|(_, value)| value.trim().trim_matches(['"', '\'']).to_owned())
        };
        let description = field("description")
            .filter(|description| !description.is_empty())
            .or_else(|| {
                let first = body.lines().map(str::trim).find(|line| !line.is_empty());
                first.map(str::to_owned)
            })
            .unwrap_or_default();
        Self {
            name,
            description,
            hint: field("argument-hint").unwrap_or_default(),
            body: body.to_owned(),
        }
    }

    pub fn command(&self) -> Command<'_> {
        Command {
            name: &self.name,
            args: &self.hint,
            description: &self.description,
        }
    }

    /// The body with `$1`, `$@` (or `$ARGUMENTS`) and `${1:-default}` replaced
    /// by the shell-split `args`.
    pub fn expand(&self, args: &str) -> String {
        expand(self.body.trim_end(), &split_args(args))
    }
}

/// The templates in the TUI directory, by name; unreadable files are skipped.
pub fn load(home: Option<&crate::config::Home>) -> Vec<Template> {
    home.map(|home| load_from(&home.commands_dir()))
        .unwrap_or_default()
}

fn load_from(dir: &Path) -> Vec<Template> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut templates: Vec<Template> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_stem()?.to_str()?.to_owned();
            let usable = path.extension().is_some_and(|ext| ext == "md")
                && !name.contains(char::is_whitespace)
                && !COMMANDS.iter().any(|command| command.name == name);
            let text = usable.then(|| std::fs::read_to_string(&path).ok())??;
            Some(Template::parse(name, &text))
        })
        .collect();
    templates.sort_by(|left, right| left.name.cmp(&right.name));
    templates
}

/// The `---` block at the top of a file, and the text after it.
fn split_front_matter(text: &str) -> (&str, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return ("", text);
    };
    match rest.split_once("\n---") {
        Some((meta, after)) => (meta, after.strip_prefix('\n').unwrap_or(after)),
        None => ("", text),
    }
}

/// Splits on whitespace; a quoted run is one argument.
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in args.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

fn expand(body: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (value, used) = substitution(rest, args).unwrap_or(("$".to_owned(), 1));
        out.push_str(&value);
        rest = &rest[used..];
    }
    out.push_str(rest);
    out
}

/// The replacement for the `$…` at the start of `text` and the bytes it spans.
fn substitution(text: &str, args: &[String]) -> Option<(String, usize)> {
    let lookup = |key: &str| match key {
        "@" => Some(args.join(" ")),
        digits if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            let index: usize = digits.parse().ok()?;
            Some(
                index
                    .checked_sub(1)
                    .and_then(|at| args.get(at))
                    .cloned()
                    .unwrap_or_default(),
            )
        }
        _ => None,
    };
    if let Some(inner) = text.strip_prefix("${") {
        let end = inner.find('}')?;
        let (key, default) = inner[..end].split_once(":-").unwrap_or((&inner[..end], ""));
        let value = lookup(key)?;
        let value = if value.is_empty() {
            default.to_owned()
        } else {
            value
        };
        return Some((value, end + 3));
    }
    if text.starts_with("$ARGUMENTS") {
        return Some((args.join(" "), "$ARGUMENTS".len()));
    }
    let key = &text[1..];
    let len = if key.starts_with('@') {
        1
    } else {
        key.bytes().take_while(u8::is_ascii_digit).count()
    };
    Some((lookup(&key[..len])?, 1 + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(text: &str) -> Template {
        Template::parse("t".into(), text)
    }

    #[test]
    fn positional_and_joined_arguments_expand() {
        let template = template("fix $1 then $2; all: $@ / $ARGUMENTS; cost $5 \\$");
        assert_eq!(
            template.expand("a \"b c\" d"),
            "fix a then b c; all: a b c d / a b c d; cost  \\$"
        );
    }

    #[test]
    fn a_default_fills_a_missing_or_empty_argument() {
        let template = template("Focus on ${1:-correctness, safety} and ${2:-tests}.");
        assert_eq!(
            template.expand(""),
            "Focus on correctness, safety and tests."
        );
        assert_eq!(template.expand("speed"), "Focus on speed and tests.");
        assert_eq!(
            template.expand("'' docs"),
            "Focus on correctness, safety and docs."
        );
        let all = self::template("all: ${@:-none}");
        assert_eq!(all.expand(""), "all: none");
    }

    #[test]
    fn a_dollar_that_names_no_argument_is_kept() {
        let template = template("price $ 5, ${x}, $HOME, $");
        assert_eq!(template.expand("a"), "price $ 5, ${x}, $HOME, $");
    }

    #[test]
    fn front_matter_gives_the_description_and_hint_and_is_not_sent() {
        let template = template(
            "---\ndescription: Review staged changes\nargument-hint: \"[focus]\"\n---\nReview now.\n",
        );
        assert_eq!(template.description, "Review staged changes");
        assert_eq!(template.hint, "[focus]");
        assert_eq!(template.expand(""), "Review now.");
    }

    #[test]
    fn without_front_matter_the_first_line_describes_the_template() {
        let template = template("\nExplain this code\nin detail\n");
        assert_eq!(template.description, "Explain this code");
        assert_eq!(template.expand(""), "\nExplain this code\nin detail");
    }

    #[test]
    fn the_directory_loads_markdown_files_that_do_not_shadow_commands() {
        let dir = std::env::temp_dir().join(format!("mct-templates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("review.md"), "Review $1").unwrap();
        std::fs::write(dir.join("model.md"), "shadow").unwrap();
        std::fs::write(dir.join("notes.txt"), "not markdown").unwrap();
        let names: Vec<String> = load_from(&dir).into_iter().map(|t| t.name).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["review"]);
    }
}
