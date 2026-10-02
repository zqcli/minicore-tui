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
    /// The compact row's object, independent of its currently selected action.
    pub fn object_group(&self) -> Option<CommandGroup> {
        match self.kind {
            MenuKind::Group(group) => Some(group),
            MenuKind::Command(name) | MenuKind::ArgumentChoice(name) => {
                command_spec(name).and_then(|spec| spec.group)
            }
        }
    }
    pub fn object_name(&self) -> &'static str {
        match self.object_group() {
            Some(CommandGroup::Session) => "Session",
            Some(CommandGroup::Workspace) => "Workspace",
            Some(CommandGroup::Conversation) => "Conversation",
            Some(CommandGroup::App) => "App",
            None => match self.kind {
                MenuKind::Command("model") | MenuKind::ArgumentChoice("model") => "Model",
                MenuKind::Command("reasoning") | MenuKind::ArgumentChoice("reasoning") => {
                    "Reasoning"
                }
                _ => "Commands",
            },
        }
    }
    pub fn action_name(&self) -> &'static str {
        match self.kind {
            MenuKind::Group(CommandGroup::Session) => "list",
            MenuKind::Group(CommandGroup::Workspace) => "files",
            MenuKind::Group(CommandGroup::Conversation) => "search",
            MenuKind::Group(CommandGroup::App) => "settings",
            MenuKind::Command("new")
                if self
                    .text
                    .split_whitespace()
                    .next()
                    .is_some_and(|head| head.eq_ignore_ascii_case("/session"))
                    && self
                        .text
                        .split_whitespace()
                        .nth(1)
                        .is_some_and(|action| action.eq_ignore_ascii_case("configure")) =>
            {
                "configure"
            }
            MenuKind::Command(name) | MenuKind::ArgumentChoice(name) => child_name(name),
        }
    }
    /// Parameter syntax is kept separate from the canonical replacement text.
    pub fn argument_hint(&self) -> Option<&'static str> {
        if self.action_name() == "configure" {
            return None;
        }
        let name = match self.kind {
            MenuKind::Command(name) | MenuKind::ArgumentChoice(name) => name,
            MenuKind::Group(_) => return None,
        };
        command_spec(name)?
            .usage
            .split_once(' ')
            .map(|(_, hint)| hint)
    }
    /// Finite choices retain catalog spelling, including case-sensitive IDs.
    pub fn value(&self) -> Option<&str> {
        let name = match self.kind {
            MenuKind::Command(name) | MenuKind::ArgumentChoice(name)
                if matches!(name, "model" | "reasoning" | "theme") =>
            {
                name
            }
            _ => return None,
        };
        let (head, tail) = self
            .text
            .trim_start_matches('/')
            .split_once(char::is_whitespace)?;
        if head.eq_ignore_ascii_case(name) {
            return Some(tail.trim());
        }
        let group = self.object_group()?;
        let (action, value) = tail.trim_start().split_once(char::is_whitespace)?;
        (head.eq_ignore_ascii_case(group.name()) && action.eq_ignore_ascii_case(child_name(name)))
            .then(|| value.trim())
    }
    pub fn choice_label(&self) -> &str {
        self.value()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.action_name())
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
fn search_name(entry: &MenuEntry) -> &'static str {
    match entry.kind {
        MenuKind::Group(group) => group.name(),
        MenuKind::Command(_) if entry.action_name() == "configure" => "configure",
        MenuKind::Command(name) | MenuKind::ArgumentChoice(name) => name,
    }
}
fn filtered(entries: Vec<MenuEntry>, query: &str) -> Vec<MenuEntry> {
    let query = query.to_ascii_lowercase();
    let mut scored = entries
        .into_iter()
        .filter_map(|entry| {
            let name_score = fuzzy_score(&query, search_name(&entry));
            let action_score = fuzzy_score(&query, entry.action_name());
            let score = match (name_score, action_score) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            score
                .or_else(|| {
                    fuzzy_score(&query, &entry.summary.to_ascii_lowercase()).map(|s| s + 100.0)
                })
                .map(|score| (score, entry))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    scored.into_iter().map(|(_, entry)| entry).collect()
}
/// Prefer a typed prefix before broader fuzzy matches ("ren" means rename,
/// rather than also matching reasoning). Choosers deliberately stay fuzzy.
fn compact_matches(entries: Vec<MenuEntry>, query: &str) -> Vec<MenuEntry> {
    let query = query.to_ascii_lowercase();
    let has_prefix = entries.iter().any(|entry| {
        search_name(entry).starts_with(&query) || entry.action_name().starts_with(&query)
    });
    let entries = if has_prefix {
        entries
            .into_iter()
            .filter(|entry| {
                search_name(entry).starts_with(&query) || entry.action_name().starts_with(&query)
            })
            .collect()
    } else {
        entries
    };
    let mut objects = Vec::new();
    filtered(entries, &query)
        .into_iter()
        .filter(|entry| {
            let object = entry.object_name();
            if objects.contains(&object) {
                false
            } else {
                objects.push(object);
                true
            }
        })
        .collect()
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
fn finite_arguments(name: &str) -> bool {
    matches!(name, "theme" | "model" | "reasoning")
}
fn compact_argument(
    name: &'static str,
    prefix: &str,
    value: &str,
    query: &str,
    models: &[String],
    reasoning: &[String],
) -> MenuEntry {
    arguments(name, prefix, value, models, reasoning)
        .and_then(|entries| entries.into_iter().next())
        .unwrap_or_else(|| MenuEntry::command(name, format!("/{query}")))
}
/// Explicitly activated control contents. Ordinary completion never expands
/// these children or catalog values into multiple rows for the same object.
pub fn choice_page(
    entry: &MenuEntry,
    filter: &str,
    models: &[String],
    reasoning: &[String],
) -> Option<MenuPage> {
    if let MenuKind::Command(name) | MenuKind::ArgumentChoice(name) = entry.kind {
        if finite_arguments(name) {
            let qualified_theme = name == "theme"
                && entry
                    .text
                    .split_whitespace()
                    .next()
                    .is_some_and(|head| head.eq_ignore_ascii_case("/app"));
            let prefix = if qualified_theme {
                "/app theme".to_owned()
            } else {
                format!("/{name}")
            };
            return Some(MenuPage {
                group: entry.object_group(),
                argument_command: Some(name),
                filter: filter.into(),
                entries: arguments(name, &prefix, filter, models, reasoning)?,
            });
        }
    }
    let group = entry.object_group()?;
    Some(MenuPage {
        group: Some(group),
        argument_command: None,
        filter: filter.into(),
        entries: filtered(children(group), filter),
    })
}
/// Compact projection: at most one row per object. Free-form argument text is
/// opaque and retained byte-for-byte; only an explicit fill replaces it.
pub fn page(query: &str, models: &[String], reasoning: &[String]) -> Option<MenuPage> {
    if let Some((head, tail)) = query.split_once(char::is_whitespace) {
        if let Some(group) = CommandGroup::parse(head) {
            let tail = tail.trim_start();
            if tail.is_empty() {
                return Some(MenuPage {
                    group: Some(group),
                    argument_command: None,
                    filter: String::new(),
                    entries: vec![MenuEntry::group(group)],
                });
            }
            if let Some((child, value)) = tail.split_once(char::is_whitespace) {
                let canonical = qualified(group, child).ok()?;
                let name = canonical
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()?;
                let spec = command_spec(name)?;
                let entry = compact_argument(
                    spec.name,
                    &format!("/{} {}", group.name(), child_name(spec.name)),
                    value.trim_start(),
                    query,
                    models,
                    reasoning,
                );
                return Some(MenuPage {
                    group: Some(group),
                    argument_command: finite_arguments(spec.name).then_some(spec.name),
                    filter: tail.into(),
                    entries: vec![entry],
                });
            }
            return Some(MenuPage {
                group: Some(group),
                argument_command: None,
                filter: tail.into(),
                entries: compact_matches(children(group), tail),
            });
        }
        let spec = command_spec(&head.to_ascii_lowercase())?;
        let entry = compact_argument(
            spec.name,
            &format!("/{}", spec.name),
            tail.trim_start(),
            query,
            models,
            reasoning,
        );
        return Some(MenuPage {
            group: None,
            argument_command: finite_arguments(spec.name).then_some(spec.name),
            filter: query.into(),
            entries: vec![entry],
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
    } else if let Some(group) = CommandGroup::parse(query) {
        vec![MenuEntry::group(group)]
    } else {
        let mut entries = COMMANDS
            .iter()
            .map(|s| MenuEntry::command(s.name, format!("/{}", s.name)))
            .collect::<Vec<_>>();
        entries.push(MenuEntry::command("new", "/session configure".into()));
        entries.extend(CommandGroup::ALL.into_iter().map(MenuEntry::group));
        compact_matches(entries, query)
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
        let root = page("", &[], &[]).unwrap();
        assert_eq!(
            root.entries
                .iter()
                .map(MenuEntry::object_name)
                .collect::<Vec<_>>(),
            [
                "Model",
                "Reasoning",
                "Session",
                "Workspace",
                "Conversation",
                "App"
            ]
        );
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
        assert_eq!(page("session", &[], &[]).unwrap().entries.len(), 1);
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
        assert_eq!(p.entries[0].value(), Some("Mixed/Model"));
        assert_eq!(p.entries[0].choice_label(), "Mixed/Model");
    }
    #[test]
    fn ordinary_search_has_one_best_action_per_object() {
        for query in ["ren", "session ren", "session rename", "session "] {
            let p = page(query, &[], &[]).unwrap();
            assert_eq!(p.entries.len(), 1, "{query}");
            assert_eq!(p.entries[0].object_name(), "Session");
        }
        let renamed = page("ren", &[], &[]).unwrap();
        assert_eq!(renamed.entries[0].text, "/rename");
        assert_eq!(renamed.entries[0].action_name(), "rename");
        assert_eq!(renamed.entries[0].argument_hint(), Some("[title]"));
        let scoped = page("session ren", &[], &[]).unwrap();
        assert_eq!(scoped.entries[0].text, "/session rename");
        for query in ["e", "re", "session", "space", "ctxt", "new", "choose"] {
            let p = page(query, &[], &[]).unwrap();
            let mut objects = p
                .entries
                .iter()
                .map(MenuEntry::object_name)
                .collect::<Vec<_>>();
            let count = objects.len();
            objects.sort_unstable();
            objects.dedup();
            assert_eq!(objects.len(), count, "duplicate objects for {query}");
        }
    }
    #[test]
    fn activated_group_exposes_children_and_fuzzy_action_filter() {
        let group = MenuEntry::group(CommandGroup::Session);
        let all = choice_page(&group, "", &[], &[]).unwrap();
        assert_eq!(all.entries.len(), 7);
        assert_eq!(all.entries[1].action_name(), "configure");
        assert_eq!(all.entries[1].argument_hint(), None);
        assert!(
            all.entries
                .iter()
                .any(|entry| entry.action_name() == "list")
        );
        let p = choice_page(&group, "rnm", &[], &[]).unwrap();
        assert_eq!(p.entries[0].text, "/session rename");
        let command = MenuEntry::command("rename", "/rename".into());
        assert_eq!(
            choice_page(&command, "", &[], &[]).unwrap().entries,
            all.entries
        );
    }
    #[test]
    fn finite_controls_are_compact_until_explicitly_activated() {
        let models = ["Mixed/Model".into(), "Mixed/Mini".into(), "Other".into()];
        let p = page("model mi", &models, &[]).unwrap();
        assert_eq!(p.entries.len(), 1);
        let choices = choice_page(&p.entries[0], "mi", &models, &[]).unwrap();
        assert_eq!(choices.entries.len(), 2);
        assert_eq!(choices.entries[0].text, "/model Mixed/Model");
        assert_eq!(choices.entries[1].text, "/model Mixed/Mini");
        assert!(
            choice_page(&p.entries[0], "mm", &models, &[])
                .unwrap()
                .entries
                .is_empty()
        );
        let theme = page("app theme l", &[], &[]).unwrap();
        assert_eq!(theme.entries.len(), 1);
        assert_eq!(theme.entries[0].object_name(), "App");
        assert_eq!(theme.entries[0].choice_label(), "light");
        let colors = choice_page(&theme.entries[0], "", &[], &[]).unwrap();
        assert_eq!(colors.entries.len(), 2);
        assert_eq!(colors.entries[0].text, "/app theme dark");
        let invalid = page("model unavailable", &models, &[]).unwrap();
        assert_eq!(invalid.entries.len(), 1);
        assert_eq!(invalid.entries[0].text, "/model unavailable");
    }
    #[test]
    fn exact_catalog_values_win_over_earlier_prefix_matches() {
        let models = ["Model/Long".into(), "Model".into(), "model".into()];
        let reasoning = ["high-plus".into(), "high".into()];
        for (query, expected) in [("model Model", "Model"), ("model model", "model")] {
            let p = page(query, &models, &reasoning).unwrap();
            assert_eq!(p.entries.len(), 1);
            assert_eq!(p.entries[0].value(), Some(expected));
        }
        let p = page("reasoning high", &models, &reasoning).unwrap();
        assert_eq!(p.entries[0].value(), Some("high"));
        let choices = choice_page(&p.entries[0], "high", &models, &reasoning).unwrap();
        assert_eq!(choices.entries[0].value(), Some("high"));
        assert_eq!(choices.entries[1].value(), Some("high-plus"));
    }

    #[test]
    fn every_action_choice_belongs_to_its_single_compact_object() {
        for group in CommandGroup::ALL {
            let row = MenuEntry::group(group);
            let choices = choice_page(&row, "", &[], &[]).unwrap();
            for choice in choices.entries {
                assert_eq!(choice.object_name(), row.object_name());
                assert_eq!(choice.object_group(), Some(group));
                let query = choice.text.trim_start_matches('/');
                let compact = page(query, &[], &[]).unwrap();
                assert_eq!(compact.entries.len(), 1, "{query}");
                assert_eq!(compact.entries[0].action_name(), choice.action_name());
                assert_eq!(compact.entries[0].text, choice.text);
            }
        }
    }

    #[test]
    fn argument_tails_are_opaque_in_compact_rows() {
        for query in [
            "conversation search Text  Here 中文",
            "workspace files a b/Case.rs ",
            "session rename  Mixed Case  Title ",
            "export ./My Notes/session.md",
            "tool ses_1 loop_1 2 call_2",
        ] {
            let p = page(query, &[], &[]).unwrap();
            assert_eq!(p.entries.len(), 1);
            assert_eq!(p.entries[0].text, format!("/{query}"));
            assert!(p.entries[0].argument_hint().is_some());
        }
    }
    #[test]
    fn exact_groups_have_stable_default_action_labels() {
        for (group, action) in [
            ("session", "list"),
            ("workspace", "files"),
            ("conversation", "search"),
            ("app", "settings"),
        ] {
            let p = page(group, &[], &[]).unwrap();
            assert_eq!(p.entries.len(), 1);
            assert_eq!(p.entries[0].action_name(), action);
            assert_eq!(p.entries[0].text, format!("/{group}"));
        }
    }
}
