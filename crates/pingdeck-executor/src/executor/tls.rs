//! TLS material shared by every client a request builds: the send itself, its OAuth 2.0 token
//! fetch and `pm.sendRequest`. One place reads the trusted roots (Settings › Network's CA file and
//! the workspace CA certificates) and the client identity (a PEM certificate with a plain or
//! passphrase-protected PKCS#8 key, or a PKCS#12 bundle), so the three clients cannot disagree.

use std::sync::Arc;

use pingdeck_base::secrets::SecretStore;
use pingdeck_model::models::{TlsCertificate, TlsCertificateKind};
use pkcs8::der::pem::{self, LineEnding};

/// Reads the key passphrase of a client certificate (`TlsCertificate::passphrase_ref`) from the
/// secret store. The default reads none, so a protected key fails with a message naming the gap.
#[derive(Clone, Default)]
pub struct CertificatePassphrases(Option<Arc<dyn SecretStore>>);

impl CertificatePassphrases {
    /// Passphrases read from `secrets`.
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self(Some(secrets))
    }

    /// The stored passphrase of `certificate`; `None` when it names none. Blocking.
    fn get(&self, certificate: &TlsCertificate) -> Result<Option<String>, String> {
        let Some(secret_ref) = certificate.passphrase_ref.as_deref() else {
            return Ok(None);
        };
        let Some(secrets) = self.0.as_ref() else {
            return Ok(None);
        };
        secrets
            .get(secret_ref)
            .map_err(|error| format!("cannot read the key passphrase: {}", error.message()))
    }
}

impl std::fmt::Debug for CertificatePassphrases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CertificatePassphrases(..)")
    }
}

impl PartialEq for CertificatePassphrases {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl Eq for CertificatePassphrases {}

/// The workspace certificates and their passphrases, for the clients PingDeck builds besides the
/// request's own (the OAuth 2.0 token fetch, `pm.sendRequest`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsMaterial {
    pub certificates: Vec<TlsCertificate>,
    pub passphrases: CertificatePassphrases,
}

/// Whether `path` names a PKCS#12 bundle (`.p12`, `.pfx`), which carries its own key.
#[must_use]
pub fn is_pkcs12_path(path: &str) -> bool {
    let lower = path.trim().to_ascii_lowercase();
    lower.ends_with(".p12") || lower.ends_with(".pfx")
}

/// The extra roots every client trusts: the CA file of Settings › Network (empty for none) and
/// each enabled workspace CA certificate. Blocking: reads the files.
pub(crate) fn trust_roots(
    ca_bundle_path: &str,
    certificates: &[TlsCertificate],
) -> Result<Vec<reqwest::Certificate>, String> {
    let mut roots = Vec::new();
    if !ca_bundle_path.is_empty() {
        roots.extend(read_roots(ca_bundle_path, "CA file")?);
    }
    for certificate in certificates
        .iter()
        .filter(|certificate| certificate.enabled && certificate.kind == TlsCertificateKind::Ca)
    {
        roots.extend(read_roots(&certificate.cert_path, "CA certificate")?);
    }
    Ok(roots)
}

fn read_roots(path: &str, label: &str) -> Result<Vec<reqwest::Certificate>, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("failed to read the {label} {path}: {error}"))?;
    reqwest::Certificate::from_pem_bundle(&bytes)
        .map_err(|error| format!("invalid {label} {path}: {error}"))
}

/// The identity a client presents for `certificate`: a PKCS#12 bundle (by its `.p12`/`.pfx`
/// extension, opened with the stored passphrase or an empty one), or a PEM certificate plus its
/// key file, whose `ENCRYPTED PRIVATE KEY` is decrypted with the stored passphrase. Blocking.
///
/// # Errors
/// A message naming the file (never the passphrase) when a file is unreadable, the passphrase is
/// missing or wrong, or the key format is one PingDeck does not read.
pub(crate) fn client_identity(
    certificate: &TlsCertificate,
    passphrases: &CertificatePassphrases,
) -> Result<reqwest::Identity, String> {
    let passphrase = passphrases.get(certificate)?;
    let cert_path = certificate.cert_path.as_str();
    if is_pkcs12_path(cert_path) {
        let bytes = std::fs::read(cert_path)
            .map_err(|error| format!("failed to read client certificate {cert_path}: {error}"))?;
        let pem = pkcs12_to_pem(&bytes, passphrase.as_deref().unwrap_or_default())
            .map_err(|message| format!("client certificate {cert_path}: {message}"))?;
        return reqwest::Identity::from_pem(pem.as_bytes())
            .map_err(|error| format!("invalid client certificate {cert_path}: {error}"));
    }
    let key_path = certificate
        .key_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| format!("client certificate {cert_path} has no private key path"))?;
    let mut pem = std::fs::read(cert_path)
        .map_err(|error| format!("failed to read client certificate {cert_path}: {error}"))?;
    let key = std::fs::read(key_path)
        .map_err(|error| format!("failed to read client certificate key {key_path}: {error}"))?;
    let key = String::from_utf8(key)
        .map_err(|_| format!("client certificate key {key_path} is not a PEM file"))?;
    let key = plain_key_pem(&key, passphrase.as_deref())
        .map_err(|message| format!("client certificate key {key_path}: {message}"))?;
    pem.push(b'\n');
    pem.extend_from_slice(key.as_bytes());
    reqwest::Identity::from_pem(&pem)
        .map_err(|error| format!("invalid client certificate {cert_path}: {error}"))
}

const ENCRYPTED_BEGIN: &str = "-----BEGIN ENCRYPTED PRIVATE KEY-----";
const ENCRYPTED_END: &str = "-----END ENCRYPTED PRIVATE KEY-----";

/// The key PEM `reqwest` can read: unchanged when it is not encrypted, the decrypted PKCS#8
/// `PRIVATE KEY` block when it is an `ENCRYPTED PRIVATE KEY`.
fn plain_key_pem(text: &str, passphrase: Option<&str>) -> Result<String, String> {
    if text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(
            "the key uses the legacy OpenSSL encryption; convert it with \
             `openssl pkcs8 -topk8 -in old.key -out new.key` and choose the new file"
                .to_owned(),
        );
    }
    let Some(start) = text.find(ENCRYPTED_BEGIN) else {
        return Ok(text.to_owned());
    };
    let end = text[start..]
        .find(ENCRYPTED_END)
        .map(|offset| start + offset + ENCRYPTED_END.len())
        .ok_or_else(|| "the encrypted key block has no END line".to_owned())?;
    let passphrase = passphrase
        .filter(|passphrase| !passphrase.is_empty())
        .ok_or_else(|| {
            "the key is protected by a passphrase; store it on the certificate".to_owned()
        })?;
    let (_, der) = pem::decode_vec(&text.as_bytes()[start..end])
        .map_err(|_| "the encrypted key block is not valid PEM".to_owned())?;
    let encrypted = pkcs8::EncryptedPrivateKeyInfo::try_from(der.as_slice())
        .map_err(|_| "the encrypted key is not a PKCS#8 key".to_owned())?;
    let document = encrypted
        .decrypt(passphrase)
        .map_err(|_| "the passphrase does not open the key".to_owned())?;
    document
        .to_pem("PRIVATE KEY", LineEnding::LF)
        .map(|pem| pem.to_string())
        .map_err(|error| format!("cannot re-encode the key: {error}"))
}

/// The leaf certificate, its chain and its key of a PKCS#12 bundle as one PEM text.
fn pkcs12_to_pem(bytes: &[u8], passphrase: &str) -> Result<String, String> {
    let store =
        p12_keystore::KeyStore::from_pkcs12(bytes, passphrase).map_err(|error| match error {
            p12_keystore::error::Error::MacError(_) => {
                "the passphrase does not open the PKCS#12 file".to_owned()
            }
            other => format!("cannot read the PKCS#12 file: {other}"),
        })?;
    let (_, chain) = store
        .private_key_chain()
        .ok_or_else(|| "the PKCS#12 file holds no private key".to_owned())?;
    let mut out = String::new();
    for certificate in chain.chain() {
        out.push_str(
            &pem::encode_string("CERTIFICATE", LineEnding::LF, certificate.as_der())
                .map_err(|error| format!("cannot encode a certificate: {error}"))?,
        );
    }
    if out.is_empty() {
        return Err("the PKCS#12 file holds no certificate for its key".to_owned());
    }
    out.push_str(
        &pem::encode_string("PRIVATE KEY", LineEnding::LF, chain.key())
            .map_err(|error| format!("cannot encode the key: {error}"))?,
    );
    Ok(out)
}
