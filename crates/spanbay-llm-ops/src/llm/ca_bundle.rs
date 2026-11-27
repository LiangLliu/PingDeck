//! The `network.ca_bundle_path` PEM bundle as extra trusted roots of the LLM HTTP clients.
//!
//! Single responsibility: read the file once into [`reqwest::Certificate`]s. The shared upstream
//! clients (`LlmRuntime::http_for`) and the safe transport (`ReqwestSafeHttp`, used by balance,
//! model refresh, key test and the sniffer) both trust the same bundle.

use spanbay_base::error::AppError;

/// The certificates of the PEM file at `path`; a blank path is no bundle.
///
/// # Errors
/// [`AppError::Validation`] when the file cannot be read or holds no certificate.
pub fn load_ca_roots(path: &str) -> Result<Vec<reqwest::Certificate>, AppError> {
    let path = path.trim();
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path).map_err(|error| {
        AppError::validation(format!(
            "cannot read the CA bundle (network.ca_bundle_path): {}",
            error.kind()
        ))
    })?;
    reqwest::Certificate::from_pem_bundle(&bytes)
        .ok()
        .filter(|roots| !roots.is_empty())
        .ok_or_else(|| {
            AppError::validation("the CA bundle (network.ca_bundle_path) holds no certificate")
        })
}

/// A self-signed CA certificate for the bundle tests.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
    MIIBbDCCARKgAwIBAgIUbULgTdgKYCXtb5wwubnW/Qe8m/QwCgYIKoZIzj0EAwIw\n\
    GzEZMBcGA1UEAwwQUGluZ0RlY2sgVGVzdCBDQTAgFw0yNjExMDcwMzE5NDBaGA8y\n\
    MTI2MTAxNDAzMTk0MFowGzEZMBcGA1UEAwwQUGluZ0RlY2sgVGVzdCBDQTBZMBMG\n\
    ByqGSM49AgEGCCqGSM49AwEHA0IABLh3RvPPORoiGPEZxBstSzPPUuK6EQ/j9SED\n\
    aJfhq1ECR8wMiMOfJRZdk9GyCSOO1ZiskXPrNUjRmJAfIcPZUN6jMjAwMB0GA1Ud\n\
    DgQWBBS4NkpL3tMKMOR7CVQENr9wYVIYlDAPBgNVHRMBAf8EBTADAQH/MAoGCCqG\n\
    SM49BAMCA0gAMEUCIHE5xb5q/IDl2D3H0VEiuGjCT2MdDNHzGS+hpB67pYZHAiEA\n\
    5dUWvwluv2JTKEbWs+grsNRfav5Pv6D2zUlgafX7Ih8=\n\
    -----END CERTIFICATE-----\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pem(name: &str, contents: &str) -> String {
        let path = std::env::temp_dir().join(format!("pd-ca-{name}-{}.pem", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_blank_path_is_no_bundle_and_a_good_file_yields_its_roots() {
        assert!(load_ca_roots("  ").unwrap().is_empty());
        assert_eq!(
            load_ca_roots(&write_pem("good", TEST_CA_PEM))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_missing_or_certificate_free_file_is_a_validation_error() {
        for path in [
            "/nonexistent/pd-ca.pem".to_owned(),
            write_pem("empty", "no certificate here"),
        ] {
            let error = load_ca_roots(&path).unwrap_err();
            assert!(matches!(error, AppError::Validation { .. }), "{path}");
            assert!(error.message().contains("ca_bundle_path"), "{path}");
        }
    }
}
