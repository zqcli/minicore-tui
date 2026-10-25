//! The listener's TLS configuration from a user's PEM certificate chain and key.

use std::path::Path;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

/// Reads `cert` and `key`; an error names the `[remote]` key at fault and the file.
pub fn load(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>, (&'static str, String)> {
    let cert_error = |reason: String| ("tls_cert", format!("{}: {reason}", cert.display()));
    let key_error = |reason: String| ("tls_key", format!("{}: {reason}", key.display()));
    let chain = CertificateDer::pem_file_iter(cert)
        .and_then(|items| items.collect::<Result<Vec<_>, _>>())
        .map_err(|error| cert_error(error.to_string()))?;
    if chain.is_empty() {
        return Err(cert_error("no PEM certificate found".into()));
    }
    let private =
        PrivateKeyDer::from_pem_file(key).map_err(|error| key_error(error.to_string()))?;
    // The provider is passed in, never installed: the Agent's HTTP client shares the process.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_no_client_auth()
        .with_single_cert(chain, private)
        .map_err(|error| {
            key_error(match error {
                rustls::Error::InconsistentKeys(_) => "does not match the certificate".into(),
                error => error.to_string(),
            })
        })?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Writes a fresh self-signed certificate for `localhost` and `127.0.0.1` and its key.
#[cfg(test)]
pub fn self_signed(dir: &Path, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let generated =
        rcgen::generate_simple_self_signed(["localhost".to_owned(), "127.0.0.1".to_owned()])
            .unwrap();
    let cert = dir.join(format!("{name}.crt"));
    let key = dir.join(format!("{name}.key"));
    std::fs::write(&cert, generated.cert.pem()).unwrap();
    std::fs::write(&key, generated.signing_key.serialize_pem()).unwrap();
    (cert, key)
}
