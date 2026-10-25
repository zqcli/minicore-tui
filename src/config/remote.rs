//! Validated listener settings and directory-picker roots.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use super::{ConfigError, Source};

#[derive(Clone, Debug)]
pub struct Remote {
    /// Addresses to listen on, in configured order.
    pub bind: Vec<SocketAddr>,
    pub url: Option<String>,
    pub roots: Vec<PathBuf>,
    pub allow_insecure: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Bind {
    One(String),
    Many(Vec<String>),
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RemoteSection {
    bind: Option<Spanned<Bind>>,
    url: Option<Spanned<String>>,
    roots: Option<Spanned<Vec<String>>>,
    allow_insecure: bool,
}

/// `value` with a leading `~` replaced by `home`; `None` when that needs an unset home.
fn expand(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    if value == "~" {
        return home.map(Path::to_path_buf);
    }
    match value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        Some(rest) => home.map(|home| home.join(rest)),
        None => Some(PathBuf::from(value)),
    }
}

impl RemoteSection {
    pub(super) fn build(&self, source: &Source) -> Result<Remote, ConfigError> {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        self.build_with_home(source, home)
    }

    fn build_with_home(
        &self,
        source: &Source,
        home: Option<PathBuf>,
    ) -> Result<Remote, ConfigError> {
        let error = |key: &str, span: Option<std::ops::Range<usize>>, reason: String| {
            source.error(
                Some(span.unwrap_or(0..0)),
                format!("[remote] {key}: {reason}"),
            )
        };
        let bind_span = self.bind.as_ref().map(Spanned::span);
        let defaults = ["127.0.0.1:8765".to_owned(), "[::1]:8765".to_owned()];
        let entries = match self.bind.as_ref().map(Spanned::get_ref) {
            Some(Bind::One(value)) => std::slice::from_ref(value),
            Some(Bind::Many(values)) => values.as_slice(),
            None => &defaults,
        };
        if entries.is_empty() {
            return Err(error("bind", bind_span, "must not be empty".into()));
        }
        let mut bind = Vec::new();
        for value in entries {
            let address = value
                .parse::<SocketAddr>()
                .map_err(|cause| error("bind", bind_span.clone(), format!("{value:?}: {cause}")))?;
            if bind.contains(&address) {
                return Err(error(
                    "bind",
                    bind_span.clone(),
                    format!("{value:?} is listed twice"),
                ));
            }
            if !address.ip().is_loopback() && !self.allow_insecure {
                return Err(error(
                    "bind",
                    bind_span.clone(),
                    format!(
                        "{address}: a non-loopback bind sends traffic unencrypted; set allow_insecure = true"
                    ),
                ));
            }
            bind.push(address);
        }
        let url = self.url.as_ref().map(|value| value.get_ref().clone());
        if let Some(value) = &self.url {
            let url = value.get_ref();
            if !(url.starts_with("http://") || url.starts_with("https://")) || url.ends_with('/') {
                return Err(error(
                    "url",
                    Some(value.span()),
                    "must start with http:// or https:// and have no trailing /".into(),
                ));
            }
        }
        let roots_span = self.roots.as_ref().map(Spanned::span);
        let home = home.as_deref();
        let expanded = |key: &str, span: Option<std::ops::Range<usize>>, value: &str| {
            expand(value, home)
                .ok_or_else(|| error(key, span, "user home directory is not set".into()))
        };
        let roots = match &self.roots {
            Some(values) => values.get_ref().as_slice(),
            None => &["~".to_owned()],
        };
        if roots.is_empty() {
            return Err(error("roots", roots_span, "must not be empty".into()));
        }
        let paths = roots
            .iter()
            .map(|value| expanded("roots", roots_span.clone(), value))
            .collect::<Result<Vec<_>, ConfigError>>()?;
        let roots = paths
            .into_iter()
            .map(|path| {
                let canonical = path.canonicalize().map_err(|cause| {
                    error(
                        "roots",
                        roots_span.clone(),
                        format!("{}: {cause}", path.display()),
                    )
                })?;
                if !canonical.is_dir() {
                    return Err(error(
                        "roots",
                        roots_span.clone(),
                        format!("{}: not a directory", path.display()),
                    ));
                }
                Ok(canonical)
            })
            .collect::<Result<Vec<_>, ConfigError>>()?;
        Ok(Remote {
            bind,
            url,
            roots,
            allow_insecure: self.allow_insecure,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(text: &str, home: &std::path::Path) -> Result<Remote, ConfigError> {
        let source = Source {
            file: "config.toml".into(),
            text: format!("[remote]\n{text}"),
        };
        #[derive(Deserialize)]
        struct File {
            remote: RemoteSection,
        }
        source
            .parse::<File>()?
            .remote
            .build_with_home(&source, Some(home.into()))
    }

    #[test]
    fn omitted_settings_listen_on_both_loopbacks_and_root_at_home() {
        let home = tempfile::tempdir().unwrap();
        let config = build("", home.path()).unwrap();
        let bind: Vec<_> = config.bind.iter().map(ToString::to_string).collect();
        assert_eq!(bind, ["127.0.0.1:8765", "[::1]:8765"]);
        assert_eq!(config.roots, [home.path().canonicalize().unwrap()]);
        assert!(config.url.is_none());
        assert!(!config.allow_insecure);
    }

    #[test]
    fn bind_takes_one_address_or_a_list_in_order() {
        let home = tempfile::tempdir().unwrap();
        for (text, expected) in [
            ("bind = '[::1]:9000'", vec!["[::1]:9000"]),
            (
                "bind = ['[::1]:9000', '127.0.0.1:9000']",
                vec!["[::1]:9000", "127.0.0.1:9000"],
            ),
        ] {
            let bind = build(text, home.path()).unwrap().bind;
            let bind: Vec<_> = bind.iter().map(ToString::to_string).collect();
            assert_eq!(bind, expected, "{text}");
        }
    }

    #[test]
    fn empty_unparsable_and_duplicate_bind_lists_are_errors_at_the_bind_line() {
        let home = tempfile::tempdir().unwrap();
        for (text, reason) in [
            ("bind = []", "must not be empty"),
            (
                "bind = ['127.0.0.1:1', 'x']",
                "\"x\": invalid socket address syntax",
            ),
            (
                "bind = ['127.0.0.1:1', '127.0.0.1:1']",
                "\"127.0.0.1:1\" is listed twice",
            ),
        ] {
            assert_eq!(
                build(text, home.path()).unwrap_err().to_string(),
                format!("config.toml:2: [remote] bind: {reason}")
            );
        }
    }

    #[test]
    fn public_listeners_require_explicit_consent_at_the_bind_line() {
        let home = tempfile::tempdir().unwrap();
        let error = build("bind = ['127.0.0.1:8765', '[::]:8765']", home.path())
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "config.toml:2: [remote] bind: [::]:8765: a non-loopback bind sends traffic unencrypted; set allow_insecure = true"
        );
        assert!(build("bind = '0.0.0.0:8765'\nallow_insecure = true", home.path()).is_ok());
        assert!(build("bind = '[::1]:8765'", home.path()).is_ok());
    }

    #[test]
    fn roots_expand_home_and_reject_missing_paths_files_and_empty_lists() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("work")).unwrap();
        std::fs::write(home.path().join("file"), "text").unwrap();
        let config = build("roots = ['~/work', '~']", home.path()).unwrap();
        assert_eq!(
            config.roots,
            [
                home.path().join("work").canonicalize().unwrap(),
                home.path().canonicalize().unwrap()
            ]
        );
        for roots in ["['~/missing']", "['~/file']", "[]"] {
            assert!(
                build(&format!("roots = {roots}"), home.path())
                    .unwrap_err()
                    .to_string()
                    .starts_with("config.toml:2: [remote] roots:")
            );
        }
    }

    #[test]
    fn invalid_addresses_and_urls_name_their_keys() {
        let home = tempfile::tempdir().unwrap();
        for (text, key) in [
            ("bind = 'localhost:80'", "bind"),
            ("url = 'ftp://host'", "url"),
            ("url = 'https://host/'", "url"),
        ] {
            assert!(
                build(text, home.path())
                    .unwrap_err()
                    .to_string()
                    .starts_with(&format!("config.toml:2: [remote] {key}:"))
            );
        }
        assert!(build("url = 'https://host:8765'", home.path()).is_ok());
    }
}
