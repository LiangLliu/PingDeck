use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::validation::{WorkspaceError, validate_id, validate_url};

use super::{CookieJarEntryId, RequestId, WorkspaceId};

/// Largest name + value + domain + path (bytes) of a cookie a response may store (RFC 6265 §6.1
/// asks for at least 4096); a larger one is reported and not kept.
pub const COOKIE_MAX_BYTES: usize = 4096;

/// A cookie stored in a workspace cookie jar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJarEntry {
    pub id: CookieJarEntryId,
    pub workspace_id: WorkspaceId,
    pub source_request_id: Option<RequestId>,
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub host_only: bool,
    pub expires: Option<String>,
    pub max_age: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Whether the cookie was expired when this entry was returned by a use-case (computed with
    /// [`CookieJarEntry::is_expired_at`]; never stored). Omitted from JSON when false, so exports
    /// and stored rows never carry it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expired: bool,
}

impl CookieJarEntry {
    /// Returns the entry with `expired` evaluated at `now`.
    #[must_use]
    pub fn with_expiry_flag(mut self, now: SystemTime) -> Self {
        self.expired = self.is_expired_at(now);
        self
    }

    /// Builds an entry from a `Set-Cookie` response, resolving domain and path against `final_url`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_response_cookie(
        workspace_id: impl AsRef<str>,
        source_request_id: Option<&str>,
        final_url: impl AsRef<str>,
        name: impl AsRef<str>,
        value: impl Into<String>,
        domain: Option<&str>,
        path: Option<&str>,
        expires: Option<&str>,
        max_age: Option<&str>,
        secure: bool,
        http_only: bool,
        same_site: Option<&str>,
        timestamp: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        let workspace_id = WorkspaceId::new(workspace_id)?;
        let source_request_id = source_request_id.map(RequestId::new).transpose()?;
        let final_url = validate_url(final_url.as_ref())?;
        let parsed_url =
            Url::parse(&final_url).map_err(|_| WorkspaceError::InvalidCookieSourceUrl {
                url: final_url.clone(),
            })?;
        let source_host = parsed_url
            .host_str()
            .ok_or_else(|| WorkspaceError::InvalidCookieSourceUrl {
                url: final_url.clone(),
            })?
            .to_ascii_lowercase();
        let name = validate_id("cookie name", name.as_ref())?;
        let (domain, host_only) = effective_cookie_domain(domain, &source_host)?;
        let path = effective_cookie_path(path, parsed_url.path());

        Self::from_resolved_cookie(
            workspace_id.as_str(),
            source_request_id.as_ref().map(RequestId::as_str),
            name,
            value,
            domain,
            path,
            host_only,
            expires,
            max_age,
            secure,
            http_only,
            same_site,
            timestamp,
        )
    }

    /// Builds an entry from a cookie whose scope was already resolved against the hop URL
    /// that set it. Unlike [`Self::from_response_cookie`], the domain is not re-validated
    /// against a single response host, so cookies captured before a cross-host redirect
    /// are stored under the host that actually set them.
    #[allow(clippy::too_many_arguments)]
    pub fn from_resolved_cookie(
        workspace_id: impl AsRef<str>,
        source_request_id: Option<&str>,
        name: impl AsRef<str>,
        value: impl Into<String>,
        domain: impl AsRef<str>,
        path: impl AsRef<str>,
        host_only: bool,
        expires: Option<&str>,
        max_age: Option<&str>,
        secure: bool,
        http_only: bool,
        same_site: Option<&str>,
        timestamp: impl AsRef<str>,
    ) -> Result<Self, WorkspaceError> {
        let workspace_id = WorkspaceId::new(workspace_id)?;
        let source_request_id = source_request_id.map(RequestId::new).transpose()?;
        let name = validate_id("cookie name", name.as_ref())?;
        let domain = validate_id("cookie domain", domain.as_ref())?.to_ascii_lowercase();
        if !host_only && cookie_domain_is_public_suffix(&domain) {
            return Err(WorkspaceError::CookiePublicSuffixDomain { domain });
        }
        let path = normalize_resolved_cookie_path(path.as_ref())?;
        let timestamp = validate_id("cookie timestamp", timestamp.as_ref())?;
        // The expiry is fixed when the cookie arrives (RFC 6265 §5.3): `Max-Age` becomes an
        // absolute `Expires`, so later edits (which bump `updated_at`) never extend it.
        let received_at = timestamp
            .parse::<u64>()
            .ok()
            .and_then(|millis| UNIX_EPOCH.checked_add(Duration::from_millis(millis)))
            .unwrap_or_else(SystemTime::now);
        let expires = cookie_expiry_at(expires, max_age, received_at).map(format_cookie_date);
        let id = CookieJarEntryId::new(format!(
            "cookie:{}:{}:{}:{}",
            workspace_id.as_str(),
            domain,
            path,
            name
        ))?;

        Ok(Self {
            id,
            workspace_id,
            source_request_id,
            name,
            value: value.into(),
            domain,
            path,
            host_only,
            expires,
            max_age: None,
            secure,
            http_only,
            same_site: same_site.map(str::to_owned),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            expired: false,
        })
    }

    /// Returns whether `max_age` or `expires` places the cookie in the past at `now`. A cookie
    /// a response set carries its absolute expiry in `expires` (see
    /// [`Self::from_resolved_cookie`]); a `max_age` only comes with an imported row and counts
    /// from its `created_at`, which no edit changes. A value that does not parse never expires.
    #[must_use]
    pub fn is_expired_at(&self, now: SystemTime) -> bool {
        if let Some(max_age) = self.max_age.as_deref() {
            return cookie_is_expired_by_max_age(max_age, &self.created_at, now).unwrap_or(false);
        }

        self.expires
            .as_deref()
            .and_then(parse_cookie_date)
            .is_some_and(|expires_at| expires_at <= now)
    }
}

/// Versioned envelope used to export a workspace cookie jar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJarExport {
    pub format: String,
    pub version: u16,
    pub workspace_id: WorkspaceId,
    pub cookies: Vec<CookieJarEntry>,
}

impl CookieJarExport {
    /// Format identifier written into cookie jar exports.
    pub const FORMAT: &'static str = "pingdeck.cookie_jar";
    /// Cookie jar export format version supported by this build.
    pub const VERSION: u16 = 1;

    /// Wraps `cookies` in an export envelope for the workspace.
    pub fn new(
        workspace_id: impl AsRef<str>,
        cookies: Vec<CookieJarEntry>,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            format: Self::FORMAT.into(),
            version: Self::VERSION,
            workspace_id: WorkspaceId::new(workspace_id)?,
            cookies,
        })
    }
}

/// Outcome of saving a cookie jar export to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJarExportSaveResult {
    pub workspace_id: WorkspaceId,
    pub saved: bool,
    pub path: Option<String>,
    pub cookie_count: usize,
}

impl CookieJarExportSaveResult {
    /// Reports a completed save to `path` of `cookie_count` cookies.
    pub fn saved(
        workspace_id: impl AsRef<str>,
        path: impl Into<String>,
        cookie_count: usize,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            workspace_id: WorkspaceId::new(workspace_id)?,
            saved: true,
            path: Some(path.into()),
            cookie_count,
        })
    }

    /// Reports that the save dialog was canceled.
    pub fn canceled(
        workspace_id: impl AsRef<str>,
        cookie_count: usize,
    ) -> Result<Self, WorkspaceError> {
        Ok(Self {
            workspace_id: WorkspaceId::new(workspace_id)?,
            saved: false,
            path: None,
            cookie_count,
        })
    }
}

/// Versioned envelope of a cookie jar export being imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJarImport {
    pub format: String,
    pub version: u16,
    pub workspace_id: WorkspaceId,
    pub cookies: Vec<CookieJarEntry>,
}

impl CookieJarImport {
    fn validate_envelope(&self) -> Result<(), WorkspaceError> {
        if self.format != CookieJarExport::FORMAT {
            return Err(WorkspaceError::UnsupportedCookieJarFormat {
                format: self.format.clone(),
            });
        }

        if self.version != CookieJarExport::VERSION {
            return Err(WorkspaceError::UnsupportedCookieJarVersion {
                version: self.version,
            });
        }

        Ok(())
    }

    /// Builds an import envelope, rejecting unsupported formats or versions.
    pub fn new(
        format: impl Into<String>,
        version: u16,
        workspace_id: impl AsRef<str>,
        cookies: Vec<CookieJarEntry>,
    ) -> Result<Self, WorkspaceError> {
        let import = Self {
            format: format.into(),
            version,
            workspace_id: WorkspaceId::new(workspace_id)?,
            cookies,
        };
        import.validate_envelope()?;
        Ok(import)
    }

    /// Validates the envelope and workspace match, then normalizes cookies for the target.
    pub fn into_entries_for_workspace(
        self,
        target_workspace_id: impl AsRef<str>,
    ) -> Result<Vec<CookieJarEntry>, WorkspaceError> {
        self.validate_envelope()?;
        let target_workspace_id = WorkspaceId::new(target_workspace_id)?;
        let payload_workspace_id = WorkspaceId::new(self.workspace_id.as_str())?;

        if payload_workspace_id.as_str() != target_workspace_id.as_str() {
            return Err(WorkspaceError::CookieJarWorkspaceMismatch {
                expected: target_workspace_id.as_str().into(),
                actual: payload_workspace_id.as_str().into(),
            });
        }

        self.cookies
            .into_iter()
            .map(|cookie| normalize_imported_cookie(target_workspace_id.as_str(), cookie))
            .collect()
    }
}

/// Summary of cookies imported into a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJarImportResult {
    pub workspace_id: WorkspaceId,
    pub imported_ids: Vec<CookieJarEntryId>,
    pub imported_count: usize,
}

impl CookieJarImportResult {
    /// Builds a result whose count is the number of imported ids.
    pub fn new(
        workspace_id: impl AsRef<str>,
        imported_ids: Vec<CookieJarEntryId>,
    ) -> Result<Self, WorkspaceError> {
        let imported_count = imported_ids.len();

        Ok(Self {
            workspace_id: WorkspaceId::new(workspace_id)?,
            imported_ids,
            imported_count,
        })
    }
}

/// Outcome of choosing and importing a cookie jar file via an open dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieJarImportOpenFileResult {
    pub workspace_id: WorkspaceId,
    pub opened: bool,
    pub path: Option<String>,
    pub imported_ids: Vec<CookieJarEntryId>,
    pub imported_count: usize,
}

impl CookieJarImportOpenFileResult {
    /// Reports a successful import from `path` of the given cookie ids.
    pub fn imported(
        workspace_id: impl AsRef<str>,
        path: impl Into<String>,
        imported_ids: Vec<CookieJarEntryId>,
    ) -> Result<Self, WorkspaceError> {
        let imported_count = imported_ids.len();

        Ok(Self {
            workspace_id: WorkspaceId::new(workspace_id)?,
            opened: true,
            path: Some(path.into()),
            imported_ids,
            imported_count,
        })
    }

    /// Reports that the open dialog was canceled and nothing was imported.
    pub fn canceled(workspace_id: impl AsRef<str>) -> Result<Self, WorkspaceError> {
        Ok(Self {
            workspace_id: WorkspaceId::new(workspace_id)?,
            opened: false,
            path: None,
            imported_ids: Vec::new(),
            imported_count: 0,
        })
    }
}

fn normalize_imported_cookie(
    workspace_id: &str,
    cookie: CookieJarEntry,
) -> Result<CookieJarEntry, WorkspaceError> {
    let workspace_id = WorkspaceId::new(workspace_id)?;
    let name = validate_id("cookie name", &cookie.name)?;
    let domain = normalize_imported_cookie_domain(&cookie.domain)?;
    if !cookie.host_only {
        validate_cookie_domain_is_registrable(&domain)?;
    }
    let path = normalize_imported_cookie_path(&cookie.path)?;
    let same_site = normalize_imported_same_site(cookie.same_site)?;
    let created_at = validate_id("cookie timestamp", &cookie.created_at)?;
    let updated_at = validate_id("cookie timestamp", &cookie.updated_at)?;
    let id = CookieJarEntryId::new(format!(
        "cookie:{}:{}:{}:{}",
        workspace_id.as_str(),
        domain,
        path,
        name
    ))?;

    Ok(CookieJarEntry {
        id,
        workspace_id,
        source_request_id: None,
        name,
        value: cookie.value,
        domain,
        path,
        host_only: cookie.host_only,
        expires: cookie.expires,
        max_age: cookie.max_age,
        secure: cookie.secure,
        http_only: cookie.http_only,
        same_site,
        created_at,
        updated_at,
        expired: false,
    })
}

fn normalize_imported_cookie_domain(domain: &str) -> Result<String, WorkspaceError> {
    let domain = validate_id("cookie domain", domain)?;
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();

    validate_id("cookie domain", &domain)
}

fn normalize_resolved_cookie_path(path: &str) -> Result<String, WorkspaceError> {
    let path = path.trim();

    if path.is_empty() {
        return validate_id("cookie path", "/");
    }

    normalize_imported_cookie_path(path)
}

fn normalize_imported_cookie_path(path: &str) -> Result<String, WorkspaceError> {
    let path = validate_id("cookie path", path)?;

    if path.starts_with('/') {
        Ok(path)
    } else {
        Ok(format!("/{path}"))
    }
}

fn normalize_imported_same_site(
    same_site: Option<String>,
) -> Result<Option<String>, WorkspaceError> {
    let Some(same_site) = same_site else {
        return Ok(None);
    };
    let same_site = same_site.trim();

    if same_site.is_empty() {
        return Ok(None);
    }

    match same_site {
        "Lax" | "Strict" | "None" => Ok(Some(same_site.into())),
        _ => Err(WorkspaceError::InvalidCookieSameSite {
            same_site: same_site.into(),
        }),
    }
}

fn cookie_is_expired_by_max_age(max_age: &str, created_at: &str, now: SystemTime) -> Option<bool> {
    let max_age_seconds = max_age.trim().parse::<i64>().ok()?;
    if max_age_seconds <= 0 {
        return Some(true);
    }

    let created_at_millis = created_at.trim().parse::<u64>().ok()?;
    let created_at = UNIX_EPOCH.checked_add(Duration::from_millis(created_at_millis))?;
    let expires_at =
        created_at.checked_add(Duration::from_secs(u64::try_from(max_age_seconds).ok()?))?;

    Some(expires_at <= now)
}

/// The last second an HTTP date can name (9999-12-31T23:59:59Z); later expiries are capped here.
const LAST_HTTP_DATE_SECS: u64 = 253_402_300_799;

/// The absolute expiry of a cookie received at `received_at` (RFC 6265 §5.3): `Max-Age` wins
/// (zero or negative: already expired), else `Expires`; `None` for a session cookie. An attribute
/// that does not parse is ignored, as RFC 6265 §5.2 says.
#[must_use]
pub fn cookie_expiry_at(
    expires: Option<&str>,
    max_age: Option<&str>,
    received_at: SystemTime,
) -> Option<SystemTime> {
    if let Some(seconds) = max_age.and_then(|value| value.trim().parse::<i64>().ok()) {
        let Ok(seconds) = u64::try_from(seconds) else {
            return Some(UNIX_EPOCH);
        };
        if seconds == 0 {
            return Some(UNIX_EPOCH);
        }
        let last = UNIX_EPOCH + Duration::from_secs(LAST_HTTP_DATE_SECS);
        return Some(
            received_at
                .checked_add(Duration::from_secs(seconds))
                .map_or(last, |at| at.min(last)),
        );
    }
    expires.and_then(parse_cookie_date)
}

/// `at` as an IMF-fixdate (`Wed, 21 Oct 2015 07:28:00 GMT`), clamped to the range HTTP dates
/// can name (1970 to 9999).
#[must_use]
pub fn format_cookie_date(at: SystemTime) -> String {
    let last = UNIX_EPOCH + Duration::from_secs(LAST_HTTP_DATE_SECS);
    httpdate::fmt_http_date(at.clamp(UNIX_EPOCH, last))
}

/// A cookie `Expires` value as an instant. Besides the HTTP date forms (`httpdate`), it reads
/// what servers commonly send and browsers accept (RFC 6265 §5.1.1): `-` between day, month
/// and year (`Wed, 21-Oct-2025 07:28:00 GMT`), a two-digit year (70–99 is 19xx, else 20xx),
/// and a weekday that does not match the date (it is ignored). A date before 1970 is the epoch
/// (already expired). Anything else is `None`.
#[must_use]
pub fn parse_cookie_date(text: &str) -> Option<SystemTime> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let text = text.trim();
    if let Ok(at) = httpdate::parse_http_date(text) {
        return Some(at);
    }
    let rest = text.split_once(',').map_or(text, |(_, rest)| rest);
    let mut parts = rest
        .split([' ', '-'])
        .filter(|part| !part.is_empty() && !part.eq_ignore_ascii_case("GMT"));
    let day = parts
        .next()?
        .parse::<u32>()
        .ok()
        .filter(|day| (1..=31).contains(day))?;
    let month_text = parts.next()?.get(..3)?;
    let month = MONTHS
        .iter()
        .find(|month| month.eq_ignore_ascii_case(month_text))?;
    let year = match parts.next()?.parse::<u32>().ok()? {
        year @ 0..=69 => year + 2000,
        year @ 70..=99 => year + 1900,
        year => year,
    };
    let time = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if year < 1970 {
        return Some(UNIX_EPOCH);
    }
    // `httpdate` checks the weekday against the date, so the one that matches is found.
    WEEKDAYS.iter().find_map(|weekday| {
        httpdate::parse_http_date(&format!("{weekday}, {day:02} {month} {year:04} {time} GMT")).ok()
    })
}

pub fn effective_cookie_domain(
    domain: Option<&str>,
    source_host: &str,
) -> Result<(String, bool), WorkspaceError> {
    match domain {
        Some(domain) => {
            let normalized = validate_id("cookie domain", domain)?
                .trim_start_matches('.')
                .to_ascii_lowercase();
            let normalized = validate_id("cookie domain", &normalized)?;
            // A public suffix (`localhost`, a single-label intranet host) that is the request
            // host itself makes a host-only cookie (RFC 6265 §5.3 step 5); any other is refused.
            if cookie_domain_is_public_suffix(&normalized) {
                return if normalized == source_host {
                    Ok((normalized, true))
                } else {
                    Err(WorkspaceError::CookiePublicSuffixDomain { domain: normalized })
                };
            }

            let domain_matches = if is_ip_literal_domain(source_host) {
                source_host == normalized
            } else {
                source_host == normalized || source_host.ends_with(&format!(".{normalized}"))
            };

            if domain_matches {
                Ok((normalized, false))
            } else {
                Err(WorkspaceError::CookieDomainMismatch {
                    source_host: source_host.to_owned(),
                    cookie_domain: normalized,
                })
            }
        }
        None => Ok((source_host.to_owned(), true)),
    }
}

pub fn effective_cookie_path(path: Option<&str>, source_path: &str) -> String {
    match path {
        Some(path) => {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                default_cookie_path(source_path)
            } else if trimmed.starts_with('/') {
                trimmed.to_owned()
            } else {
                format!("/{trimmed}")
            }
        }
        None => default_cookie_path(source_path),
    }
}

pub(crate) fn default_cookie_path(source_path: &str) -> String {
    if source_path.is_empty() || source_path == "/" {
        return "/".into();
    }

    match source_path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(index) => source_path[..index].to_owned(),
    }
}

fn validate_cookie_domain_is_registrable(domain: &str) -> Result<(), WorkspaceError> {
    if cookie_domain_is_public_suffix(domain) {
        Err(WorkspaceError::CookiePublicSuffixDomain {
            domain: domain.to_owned(),
        })
    } else {
        Ok(())
    }
}

pub fn cookie_domain_is_public_suffix(domain: &str) -> bool {
    !is_ip_literal_domain(domain) && psl::domain_str(domain).is_none()
}

fn is_ip_literal_domain(domain: &str) -> bool {
    domain.parse::<std::net::IpAddr>().is_ok()
}
