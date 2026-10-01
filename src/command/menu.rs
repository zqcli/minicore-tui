//! Finite command navigation metadata. Execution stays in the existing parser.
use super::{COMMANDS, CommandArgs, CommandIssue, command_spec, fuzzy_score};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    Session,
    Workspace,
    Conversation,
    App,
}
impl CommandGroup {
    pub const ALL: [Self; 4] = [
        Self::Session,
        Self::Workspace,
        Self::Conversation,
        Self::App,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Conversation => "conversation",
            Self::App => "app",
        }
    }
    pub fn summary(self) -> &'static str {
        match self {
            Self::Session => "new, open and manage sessions",
            Self::Workspace => "files, text search and changes",
            Self::Conversation => "search, copy, export and context",
            Self::App => "preferences, help and application",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|g| g.name().eq_ignore_ascii_case(name))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Group(CommandGroup),
    Command(&'static str),
    ArgumentChoice(&'static str),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub kind: MenuKind,
    pub text: String,
    pub summary: &'static str,
    pub breadcrumb: &'static str,
}
impl MenuEntry {
    pub fn group(group: CommandGroup) -> Self {
        Self {
            kind: MenuKind::Group(group),
            text: format!("/{}", group.name()),
            summary: group.summary(),
            breadcrumb: "Commands",
        }
    }
    pub fn command(name: &'static str, text: String) -> Self {
        let spec = command_spec(name);
        Self {
            kind: MenuKind::Command(name),
            text,
            summary: spec.map_or("", |s| s.menu_summary),
            breadcrumb: spec
                .and_then(|s| s.group)
                .map_or("Commands", CommandGroup::name),
        }
    }
    pub fn needs_input(&self) -> bool {
        match self.kind {
            MenuKind::Group(_) => true,
            MenuKind::ArgumentChoice(_) => false,
            MenuKind::Command(name) => {
                command_spec(name)
                    .is_some_and(|s| matches!(s.args, CommandArgs::Theme | CommandArgs::ToolRef))
                    || self.text.starts_with("/skill:")
            }
        }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
}
// Useful for public fixture builders; real candidates are constructed from metadata.
impl From<String> for MenuEntry {
    fn from(text: String) -> Self {
        let name = text
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        let canonical = command_spec(name).map_or("", |s| s.name);
        Self::command(canonical, text)
    }
}
impl From<&str> for MenuEntry {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuPage {
    pub group: Option<CommandGroup>,
    pub argument_command: Option<&'static str>,
    pub filter: String,
    pub entries: Vec<MenuEntry>,
}

fn child_name(name: &str) -> &str {
    if name == "sessions" { "list" } else { name }
}
fn children(group: CommandGroup) -> Vec<MenuEntry> {
    let mut entries = COMMANDS
        .iter()
        .filter(|s| s.group == Some(group))
        .map(|s| MenuEntry::command(s.name, format!("/{} {}", group.name(), child_name(s.name))))
        .collect::<Vec<_>>();
    if group == CommandGroup::Session {
        let mut entry = MenuEntry::command("new", "/session configure".into());
        entry.summary = "configure a new session";
        entries.insert(1, entry);
    }
    entries
}
/// Resolve exactly one qualified command prefix. The rest is opaque argument text.
pub fn qualified(group: CommandGroup, rest: &str) -> Result<String, CommandIssue> {
    let (child, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(a, b)| (a, b.trim_start()));
    if group == CommandGroup::Session && child.eq_ignore_ascii_case("configure") {
        return if args.trim().is_empty() {
            Ok("/new form".into())
        } else {
            Err(CommandIssue::InvalidArgs(
                "usage: /session configure".into(),
            ))
        };
    }
    let spec = COMMANDS
        .iter()
        .find(|s| s.group == Some(group) && child_name(s.name).eq_ignore_ascii_case(child))
        .ok_or_else(|| {
            CommandIssue::InvalidArgs(format!(
                "unknown /{} action; open /{} to choose",
                group.name(),
                group.name()
            ))
        })?;
    Ok(format!(
        "/{}{}{}",
        spec.name,
        if args.is_empty() { "" } else { " " },
        args
    ))
}
fn filtered(mut entries: Vec<MenuEntry>, query: &str) -> Vec<MenuEntry> {
    let query = query.to_ascii_lowercase();
    entries.sort_by_key(|e| !e.text.trim_start_matches('/').eq_ignore_ascii_case(&query));
    let mut scored = entries
        .into_iter()
        .filter_map(|entry| {
            let label = entry
                .text
                .trim_start_matches('/')
                .rsplit(' ')
                .next()
                .unwrap_or("");
            fuzzy_score(&query, label)
                .or_else(|| {
                    fuzzy_score(&query, &entry.summary.to_ascii_lowercase()).map(|s| s + 100.0)
                })
                .map(|score| (score, entry))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    scored.into_iter().map(|(_, entry)| entry).collect()
}
fn arguments(
    name: &'static str,
    prefix: &str,
    value: &str,
    models: &[String],
    reasoning: &[String],
) -> Option<Vec<MenuEntry>> {
    let value = value.trim();
    let mut choices: Vec<String> = match name {
        "theme" => vec!["dark".into(), "light".into()],
        "model" => models.to_vec(),
        "reasoning" => reasoning.to_vec(),
        _ => return None,
    };
    choices.sort_by_key(|choice| choice != value);
    Some(
        choices
            .into_iter()
            .filter(|choice| {
                choice
                    .to_ascii_lowercase()
                    .starts_with(&value.to_ascii_lowercase())
            })
            .map(|choice| MenuEntry {
                kind: MenuKind::ArgumentChoice(name),
                text: format!("{prefix} {choice}"),
                summary: command_spec(name).map_or("", |s| s.menu_summary),
                breadcrumb: name,
            })
            .collect(),
    )
}
/// None means the cursor is editing a free-form argument, not navigating commands.
pub fn page(query: &str, models: &[String], reasoning: &[String]) -> Option<MenuPage> {
    if let Some((head, tail)) = query.split_once(char::is_whitespace) {
        if let Some(group) = CommandGroup::parse(head) {
            let tail = tail.trim_start();
            if let Some((child, value)) = tail.split_once(char::is_whitespace) {
                let canonical = qualified(group, child).ok()?;
                let name = canonical.trim_start_matches('/');
                let spec = command_spec(name)?;
                let entries = arguments(
                    spec.name,
                    &format!("/{} {}", group.name(), child),
                    value.trim_start(),
                    models,
                    reasoning,
                )?;
                return Some(MenuPage {
                    group: Some(group),
                    argument_command: Some(spec.name),
                    filter: tail.into(),
                    entries,
                });
            }
            return Some(MenuPage {
                group: Some(group),
                argument_command: None,
                filter: tail.into(),
                entries: filtered(children(group), tail),
            });
        }
        let spec = command_spec(&head.to_ascii_lowercase())?;
        let entries = arguments(
            spec.name,
            &format!("/{}", spec.name),
            tail.trim_start(),
            models,
            reasoning,
        )?;
        return Some(MenuPage {
            group: None,
            argument_command: Some(spec.name),
            filter: query.into(),
            entries,
        });
    }
    let entries = if query.is_empty() {
        vec![
            MenuEntry::command("model", "/model".into()),
            MenuEntry::command("reasoning", "/reasoning".into()),
        ]
        .into_iter()
        .chain(CommandGroup::ALL.into_iter().map(MenuEntry::group))
        .collect()
    } else {
        let mut entries = COMMANDS
            .iter()
            .map(|s| MenuEntry::command(s.name, format!("/{}", s.name)))
            .collect::<Vec<_>>();
        entries.push(MenuEntry::command("new", "/session configure".into()));
        entries.extend(CommandGroup::ALL.into_iter().map(MenuEntry::group));
        filtered(entries, query)
    };
    Some(MenuPage {
        group: None,
        argument_command: None,
        filter: query.into(),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_is_six_entries_and_all_commands_stay_searchable() {
        assert_eq!(page("", &[], &[]).unwrap().entries.len(), 6);
        for s in COMMANDS {
            assert!(
                page(s.name, &[], &[])
                    .unwrap()
                    .entries
                    .iter()
                    .any(|e| e.text == format!("/{}", s.name))
            );
        }
    }
    #[test]
    fn qualified_tails_are_not_reparsed() {
        assert_eq!(
            qualified(CommandGroup::Conversation, "search session new 中文").unwrap(),
            "/search session new 中文"
        );
        assert_eq!(
            qualified(CommandGroup::Workspace, "files a b/Case.rs").unwrap(),
            "/files a b/Case.rs"
        );
        assert_eq!(
            qualified(CommandGroup::Session, "configure").unwrap(),
            "/new form"
        );
        assert!(qualified(CommandGroup::App, "missing").is_err());
    }
    #[test]
    fn exact_group_does_not_shadow_sessions() {
        assert!(matches!(
            page("session", &[], &[]).unwrap().entries[0].kind,
            MenuKind::Group(CommandGroup::Session)
        ));
        assert_eq!(
            page("sessions", &[], &[]).unwrap().entries[0].text,
            "/sessions"
        );
    }
    #[test]
    fn argument_choices_keep_catalog_case() {
        let p = page("model mi", &["Mixed/Model".into()], &[]).unwrap();
        assert_eq!(p.entries[0].text, "/model Mixed/Model");
        assert!(matches!(
            p.entries[0].kind,
            MenuKind::ArgumentChoice("model")
        ));
        assert!(page("conversation search Text Here", &[], &[]).is_none());
    }
}
