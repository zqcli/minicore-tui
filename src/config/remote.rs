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
    /// Set when `tls = true`; the validated certificate and key paths.
    pub tls: Option<Tls>,
}

/// The expanded paths of the PEM certificate chain and private key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tls {
    pub cert: PathBuf,
    pub key: PathBuf,
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
    tls: Option<Spanned<bool>>,
    tls_cert: Option<Spanned<String>>,
    tls_key: Option<Spanned<String>>,
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

/// `C:`, `c:/` or `C:\\` as the drive root `C:\`. A bare `C:` would otherwise
/// canonicalize to drive C's current directory.
fn drive_root(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let drive = bytes
        .first()
        .filter(|letter| letter.is_ascii_alphabetic())?;
    (bytes.get(1) == Some(&b':') && value[2..].chars().all(|c| matches!(c, '/' | '\\')))
        .then(|| PathBuf::from(format!("{}:\\", drive.to_ascii_uppercase() as char)))
}

impl RemoteSection {
    pub(super) fn build(&self, source: &Source) -> Result<Remote, ConfigError> {
        self.build_with_home(source, super::home_dir())
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
        let tls_on = self.tls.as_ref().is_some_and(|tls| *tls.get_ref());
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
            if !address.ip().is_loopback() && !tls_on && !self.allow_insecure {
                return Err(error(
                    "bind",
                    bind_span.clone(),
                    format!(
                        "{address}: a non-loopback bind sends traffic unencrypted; set tls = true or allow_insecure = true"
                    ),
                ));
            }
            bind.push(address);
        }
        let url = match &self.url {
            Some(value) => {
                let url = value.get_ref();
                if !(url.starts_with("http://") || url.starts_with("https://"))
                    || url.ends_with('/')
                {
                    return Err(error(
                        "url",
                        Some(value.span()),
                        "must start with http:// or https:// and have no trailing /".into(),
                    ));
                }
                Some(url.clone())
            }
            None => None,
        };
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
        let roots = roots
            .iter()
            .map(|value| {
                let path = match drive_root(value).filter(|_| cfg!(windows)) {
                    Some(drive) => drive,
                    None => expanded("roots", roots_span.clone(), value)?,
                };
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
        let tls = match (&self.tls_cert, &self.tls_key) {
            // Paths may stay while TLS is off, so the switch is one line.
            _ if !tls_on => None,
            (Some(cert), Some(key)) => {
                let tls = Tls {
                    cert: expanded("tls_cert", Some(cert.span()), cert.get_ref())?,
                    key: expanded("tls_key", Some(key.span()), key.get_ref())?,
                };
                crate::remote::tls::load(&tls.cert, &tls.key).map_err(|(name, reason)| {
                    let span = if name == "tls_cert" { cert } else { key }.span();
                    error(name, Some(span), reason)
                })?;
                Some(tls)
            }
            _ => {
                let span = self.tls.as_ref().map(Spanned::span);
                return Err(error("tls", span, "needs tls_cert and tls_key".into()));
            }
        };
        Ok(Remote {
            bind,
            url,
            roots,
            allow_insecure: self.allow_insecure,
            tls,
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
            "config.toml:2: [remote] bind: [::]:8765: a non-loopback bind sends traffic unencrypted; set tls = true or allow_insecure = true"
        );
        assert!(build("bind = '0.0.0.0:8765'\nallow_insecure = true", home.path()).is_ok());
        assert!(build("bind = '[::1]:8765'", home.path()).is_ok());
    }

    #[test]
    fn tls_needs_both_files_and_keeps_their_expanded_paths() {
        let home = tempfile::tempdir().unwrap();
        crate::remote::tls::self_signed(home.path(), "server");
        for text in ["tls = true", "tls = true\ntls_cert = '~/server.crt'"] {
            assert_eq!(
                build(text, home.path()).unwrap_err().to_string(),
                "config.toml:2: [remote] tls: needs tls_cert and tls_key"
            );
        }
        let files = "tls_cert = '~/server.crt'\ntls_key = '~/server.key'";
        let config = build(
            &format!("bind = '0.0.0.0:8765'\ntls = true\n{files}"),
            home.path(),
        )
        .unwrap();
        assert_eq!(
            config.tls,
            Some(Tls {
                cert: home.path().join("server.crt"),
                key: home.path().join("server.key"),
            })
        );
        let off = "tls = false\ntls_cert = '~/missing'\ntls_key = '~/missing'";
        assert_eq!(build(off, home.path()).unwrap().tls, None);
    }

    #[test]
    fn unreadable_unparsable_and_mismatched_tls_files_name_their_key_line_and_path() {
        let home = tempfile::tempdir().unwrap();
        let (cert, key) = crate::remote::tls::self_signed(home.path(), "server");
        let (_, other_key) = crate::remote::tls::self_signed(home.path(), "other");
        std::fs::write(home.path().join("text"), "not a certificate").unwrap();
        let missing = std::fs::read(home.path().join("missing")).unwrap_err();
        for (cert_file, key_file, error) in [
            (
                "missing",
                "server.key",
                format!(
                    "3: [remote] tls_cert: {}: I/O error: {missing}",
                    home.path().join("missing").display()
                ),
            ),
            (
                "text",
                "server.key",
                format!(
                    "3: [remote] tls_cert: {}: no PEM certificate found",
                    home.path().join("text").display()
                ),
            ),
            (
                "server.crt",
                "text",
                format!(
                    "4: [remote] tls_key: {}: no items found",
                    home.path().join("text").display()
                ),
            ),
            (
                "server.crt",
                "other.key",
                format!(
                    "4: [remote] tls_key: {}: does not match the certificate",
                    other_key.display()
                ),
            ),
        ] {
            let text = format!("tls = true\ntls_cert = '~/{cert_file}'\ntls_key = '~/{key_file}'");
            assert_eq!(
                build(&text, home.path()).unwrap_err().to_string(),
                format!("config.toml:{error}")
            );
        }
        assert!(crate::remote::tls::load(&cert, &key).is_ok());
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
    fn every_spelling_of_a_drive_becomes_its_root_and_other_paths_stay() {
        for value in ["C:", "c:", "C:/", "c:\\", "C:\\\\", "d:/\\"] {
            let root = drive_root(value).unwrap();
            let letter = value[..1].to_ascii_uppercase();
            assert_eq!(root, PathBuf::from(format!("{letter}:\\")), "{value}");
        }
        for value in ["", "C", "~", "/", "C:\\Users", "C:/work", "CD:", "1:", "é:"] {
            assert_eq!(drive_root(value), None, "{value}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_root_is_a_root_whatever_the_current_directory_of_that_drive() {
        let home = tempfile::tempdir().unwrap();
        let drive = std::env::var("SystemDrive").unwrap();
        let expected = Path::new(&format!("{drive}\\")).canonicalize().unwrap();
        for spelling in [drive.clone(), drive.to_lowercase(), format!("{drive}/")] {
            let config = build(&format!("roots = ['{spelling}']"), home.path()).unwrap();
            assert_eq!(config.roots, std::slice::from_ref(&expected), "{spelling}");
        }
        assert_eq!(expected.parent(), None);
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
