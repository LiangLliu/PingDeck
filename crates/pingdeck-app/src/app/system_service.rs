//! System integration use-cases: opening a link in the default browser.

use std::process::{Command, Stdio};

use crate::error::AppError;

/// Checks that `url` is an absolute `http`, `https` or `mailto` URL and returns it normalised.
pub fn open_external_validate(url: &str) -> Result<String, AppError> {
    let parsed = url::Url::parse(url.trim())
        .map_err(|_| AppError::validation("the link is not a valid absolute URL"))?;
    match parsed.scheme() {
        "http" | "https" if parsed.has_host() => Ok(parsed.into()),
        // `mailto:?attach=/path` makes some mail clients attach a local file (data exfiltration).
        "mailto"
            if parsed
                .query_pairs()
                .any(|(key, _)| key.trim().to_ascii_lowercase().starts_with("attach")) =>
        {
            Err(AppError::validation(
                "mailto links with attachments cannot be opened",
            ))
        }
        "mailto" => Ok(parsed.into()),
        _ => Err(AppError::validation(
            "only http, https and mailto links can be opened",
        )),
    }
}

/// The program that opens a URL on this platform and its leading arguments.
const fn opener() -> (&'static str, &'static [&'static str]) {
    if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        ("rundll32", &["url.dll,FileProtocolHandler"])
    } else {
        ("xdg-open", &[])
    }
}

/// Opens `url` in the default browser or mail client. Blocking: call it from a blocking pool.
pub fn system_open_external_impl(url: &str) -> Result<(), AppError> {
    let url = open_external_validate(url)?;
    let (program, args) = opener();
    let mut child = Command::new(program)
        .args(args)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| AppError::io(format!("could not start {program}: {error}")))?;
    // Reap the launcher in the background so it never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::open_external_validate;

    #[test]
    fn only_web_and_mail_links_pass() {
        for ok in [
            "https://example.com/a?b=1",
            "http://localhost:3000",
            "mailto:a@b.co",
            "mailto:a@b.co?subject=attached%20file&body=attach",
        ] {
            assert!(open_external_validate(ok).is_ok(), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "example.com",
            "",
            "https://",
            "ftp://x.y",
        ] {
            assert!(open_external_validate(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mailto_attachments_are_refused() {
        for bad in [
            "mailto:a@b.co?attach=/etc/passwd",
            "mailto:a@b.co?subject=hi&Attachment=~/.ssh/id_rsa",
            "mailto:a@b.co?ATTACH=x",
            "mailto:a@b.co?%61ttach=/etc/hosts",
            "mailto:?to=a@b.co&attachments=/tmp/x",
        ] {
            assert!(open_external_validate(bad).is_err(), "{bad}");
        }
    }
}
