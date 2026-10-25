//! Validated listener settings and directory-picker roots.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use serde::Deserialize;
use toml::Spanned;

use super::{ConfigError, Source};

#[derive(Clone, Debug)]
pub struct Remote {
    pub bind: SocketAddr,
    pub url: Option<String>,
    pub roots: Vec<PathBuf>,
    pub allow_insecure: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RemoteSection {
    bind: Option<Spanned<String>>,
    url: Option<Spanned<String>>,
    roots: Option<Spanned<Vec<String>>>,
    allow_insecure: bool,
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
        let bind = match &self.bind {
            Some(value) => value
                .get_ref()
                .parse::<SocketAddr>()
                .map_err(|cause| error("bind", bind_span.clone(), cause.to_string()))?,
            None => SocketAddr::from((Ipv4Addr::LOCALHOST, 8765)),
        };
        if !bind.ip().is_loopback() && !self.allow_insecure {
            return Err(error(
                "bind",
                bind_span,
                "a non-loopback bind sends traffic unencrypted; set allow_insecure = true".into(),
            ));
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
        let home = || {
            home.clone().ok_or_else(|| {
                error(
                    "roots",
                    roots_span.clone(),
                    "user home directory is not set".into(),
                )
            })
        };
        let paths = match &self.roots {
            Some(values) => {
                if values.get_ref().is_empty() {
                    return Err(error("roots", roots_span, "must not be empty".into()));
                }
                values
                    .get_ref()
                    .iter()
                    .map(|value| {
                        if value == "~" {
                            home()
                        } else if let Some(rest) = value
                            .strip_prefix("~/")
                            .or_else(|| value.strip_prefix("~\\"))
                        {
                            Ok(home()?.join(rest))
                        } else {
                            Ok(PathBuf::from(value))
                        }
                    })
                    .collect::<Result<Vec<_>, ConfigError>>()?
            }
            None => vec![home()?],
        };
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
    fn omitted_settings_restrict_the_listener_and_roots_to_local_defaults() {
        let home = tempfile::tempdir().unwrap();
        let config = build("", home.path()).unwrap();
        assert_eq!(config.bind.to_string(), "127.0.0.1:8765");
        assert_eq!(config.roots, [home.path().canonicalize().unwrap()]);
        assert!(config.url.is_none());
        assert!(!config.allow_insecure);
    }

    #[test]
    fn public_listeners_require_explicit_consent_at_the_bind_line() {
        let home = tempfile::tempdir().unwrap();
        let error = build("bind = '0.0.0.0:8765'", home.path())
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "config.toml:2: [remote] bind: a non-loopback bind sends traffic unencrypted; set allow_insecure = true"
        );
        assert!(build("bind = '[::]:8765'\nallow_insecure = true", home.path()).is_ok());
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
