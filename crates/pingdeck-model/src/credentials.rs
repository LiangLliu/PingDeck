//! Where a saved request or a flow step carries a credential: the position rule set of the
//! outlets that must not let a literal credential out (copied code, Postman export, and folder
//! sync of requests and flows). Each outlet decides what to write in a position (a placeholder,
//! an empty string, a `{{secret:…}}` slot); none keeps its own list of positions. The Mock
//! export reads the URL positions through [`url_credentials`]; the mock documents of folder sync
//! do not use it yet: they mask credential header rows by name only, so a synced mock URL keeps
//! its userinfo and query (the cost recorded with this rule set in docs/decisions.md).
//!
//! The positions: URL userinfo, a URL query value or query row whose name is a credential name,
//! a credential header row (`is_secret_header_name`), a credential-named field of a
//! form-urlencoded or multipart text body (for a flow step, a credential-named line of a
//! form-urlencoded `body_override`), and the secret leaves of the auth config. A value holding a
//! `{{variable}}` is a reference, not a literal ([`is_literal`]).

use std::ops::Range;

use pingdeck_base::secrets::{REDACT_QUERY_KEYS, is_secret_header_name};

use crate::items::{FlowBodyKind, FlowStep};
use crate::models::{AuthConfig, KeyValue, Request, RequestBody};

/// Whether a query parameter, form field or variable named `name` carries a credential (`key`,
/// `signature`, `client_secret`, `x-api-key`, `db_password`).
#[must_use]
pub fn is_credential_key(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    REDACT_QUERY_KEYS.contains(&lower.as_str()) || is_secret_header_name(&lower)
}

/// Whether `value` is a literal credential: not blank and holding no `{{variable}}` reference.
#[must_use]
pub fn is_literal(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains("{{")
}

/// A credential-named field inside the `key=value` lines of a form text (a flow step's
/// form-urlencoded `body_override`): its name and the byte range of its value in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormLineCredential {
    pub name: String,
    pub range: Range<usize>,
}

/// The credential-named fields of a form text, read the way the flow runner reads it: one
/// `key=value` per line, blank lines skipped, a `# ` prefix disables the field (it is still
/// stored, so it is still found), the first `=` separates and both sides are trimmed. Empty
/// values are included, so a caller sees the position.
#[must_use]
pub fn form_line_credentials(text: &str) -> Vec<FormLineCredential> {
    let mut found = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line_start = offset;
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        let lead = line.len() - line.trim_start().len();
        let mut body = line.trim();
        let mut start = line_start + lead;
        if let Some(rest) = body.strip_prefix('#')
            && (rest.is_empty() || rest.starts_with(' '))
        {
            let inner = rest.trim_start();
            start += body.len() - inner.len();
            body = inner;
        }
        let Some(eq) = body.find('=') else { continue };
        let name = body[..eq].trim();
        if name.is_empty() || !is_credential_key(name) {
            continue;
        }
        let after = &body[eq + 1..];
        let value = after.trim();
        let value_start = start + eq + 1 + (after.len() - after.trim_start().len());
        found.push(FormLineCredential {
            name: name.to_owned(),
            range: value_start..value_start + value.len(),
        });
    }
    found
}

/// Which part of a URL carries a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlPart {
    /// `user:password` before the host.
    Userinfo,
    /// The value of the query pair whose name (as written in the URL) is this.
    Query(String),
}

/// A credential inside a URL text: its part and its byte range in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlCredential {
    pub part: UrlPart,
    pub range: Range<usize>,
}

/// The name of a query pair as the server reads it (`+` is a space, `%xx` decoded).
fn decoded_name(raw: &str) -> String {
    url::form_urlencoded::parse(raw.as_bytes())
        .next()
        .map(|(name, _)| name.into_owned())
        .unwrap_or_default()
}

/// The credentials of a URL text in ascending order: the userinfo, then each credential-named
/// query value (empty values included, so a caller sees the position).
#[must_use]
pub fn url_credentials(url: &str) -> Vec<UrlCredential> {
    let mut found = Vec::new();
    let mut authority_end = 0;
    if let Some(scheme_end) = url.find("://") {
        let start = scheme_end + 3;
        authority_end = url[start..]
            .find(['/', '?', '#'])
            .map_or(url.len(), |at| start + at);
        if let Some(at) = url[start..authority_end].rfind('@')
            && at > 0
        {
            found.push(UrlCredential {
                part: UrlPart::Userinfo,
                range: start..start + at,
            });
        }
    }
    let Some(question) = url[authority_end..].find('?').map(|at| authority_end + at) else {
        return found;
    };
    let end = url[question..]
        .find('#')
        .map_or(url.len(), |at| question + at);
    let mut offset = question + 1;
    for pair in url[question + 1..end].split('&') {
        if let Some((name, _)) = pair.split_once('=')
            && !name.is_empty()
            && is_credential_key(&decoded_name(name))
        {
            found.push(UrlCredential {
                part: UrlPart::Query(name.to_owned()),
                range: offset + name.len() + 1..offset + pair.len(),
            });
        }
        offset += pair.len() + 1;
    }
    found
}

/// A secret leaf of an auth config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSecret {
    /// Bearer `token_ref`.
    Token,
    /// Basic, Digest or OAuth 2.0 `password_ref`.
    Password,
    /// API key `value_ref`.
    ApiKeyValue,
    /// OAuth 2.0 `client_secret_ref`.
    ClientSecret,
    /// AWS Signature V4 `secret_key_ref`.
    SecretKey,
    /// AWS Signature V4 `session_token_ref`.
    SessionToken,
}

impl AuthSecret {
    /// Every secret leaf, for callers that walk a serialized auth config.
    pub const ALL: [Self; 6] = [
        Self::Token,
        Self::Password,
        Self::ApiKeyValue,
        Self::ClientSecret,
        Self::SecretKey,
        Self::SessionToken,
    ];

    /// The serialized field name inside the auth variant (`token_ref`).
    #[must_use]
    pub fn field(self) -> &'static str {
        match self {
            Self::Token => "token_ref",
            Self::Password => "password_ref",
            Self::ApiKeyValue => "value_ref",
            Self::ClientSecret => "client_secret_ref",
            Self::SecretKey => "secret_key_ref",
            Self::SessionToken => "session_token_ref",
        }
    }

    /// A short name for a placeholder (`token`, `api_key`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::Password => "password",
            Self::ApiKeyValue => "api_key",
            Self::ClientSecret => "client_secret",
            Self::SecretKey => "secret_key",
            Self::SessionToken => "session_token",
        }
    }
}

/// The secret leaves `auth` holds (an absent optional leaf is left out).
#[must_use]
pub fn auth_secrets(auth: &AuthConfig) -> Vec<AuthSecret> {
    match auth {
        AuthConfig::Bearer { .. } => vec![AuthSecret::Token],
        AuthConfig::Basic { .. } => vec![AuthSecret::Password],
        AuthConfig::ApiKey { .. } => vec![AuthSecret::ApiKeyValue],
        AuthConfig::OAuth2 { .. } => vec![AuthSecret::ClientSecret, AuthSecret::Password],
        AuthConfig::Digest { password_ref, .. } => password_ref
            .as_ref()
            .map(|_| AuthSecret::Password)
            .into_iter()
            .collect(),
        AuthConfig::AwsSignatureV4 {
            secret_key_ref,
            session_token_ref,
            ..
        } => [
            secret_key_ref.as_ref().map(|_| AuthSecret::SecretKey),
            session_token_ref.as_ref().map(|_| AuthSecret::SessionToken),
        ]
        .into_iter()
        .flatten()
        .collect(),
        AuthConfig::Inherit | AuthConfig::None => Vec::new(),
    }
}

/// The value of the secret leaf `which` of `auth`, if it has one.
pub fn auth_secret_mut(auth: &mut AuthConfig, which: AuthSecret) -> Option<&mut String> {
    match (auth, which) {
        (AuthConfig::Bearer { token_ref }, AuthSecret::Token) => Some(token_ref),
        (AuthConfig::Basic { password_ref, .. }, AuthSecret::Password)
        | (AuthConfig::OAuth2 { password_ref, .. }, AuthSecret::Password) => Some(password_ref),
        (AuthConfig::ApiKey { value_ref, .. }, AuthSecret::ApiKeyValue) => Some(value_ref),
        (
            AuthConfig::OAuth2 {
                client_secret_ref, ..
            },
            AuthSecret::ClientSecret,
        ) => Some(client_secret_ref),
        (AuthConfig::Digest { password_ref, .. }, AuthSecret::Password) => password_ref.as_mut(),
        (AuthConfig::AwsSignatureV4 { secret_key_ref, .. }, AuthSecret::SecretKey) => {
            secret_key_ref.as_mut()
        }
        (
            AuthConfig::AwsSignatureV4 {
                session_token_ref, ..
            },
            AuthSecret::SessionToken,
        ) => session_token_ref.as_mut(),
        _ => None,
    }
}

/// One place of a request (or flow step) that carries a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialPosition {
    /// A part of the URL text.
    Url(UrlCredential),
    /// The value of the query row at this index.
    QueryParam(usize),
    /// The value of the header row at this index.
    Header(usize),
    /// The value of the form-urlencoded field at this index.
    FormField(usize),
    /// The value of the multipart text part at this index.
    MultipartField(usize),
    /// A credential-named line of a flow step's form-urlencoded `body_override` text.
    FormLine(FormLineCredential),
    /// A secret leaf of the auth config.
    Auth(AuthSecret),
}

fn rows(
    rows: &[KeyValue],
    rule: fn(&str) -> bool,
    position: fn(usize) -> CredentialPosition,
) -> impl Iterator<Item = CredentialPosition> + '_ {
    rows.iter()
        .enumerate()
        .filter(move |(_, row)| rule(&row.key))
        .map(move |(index, _)| position(index))
}

/// Every credential position of `request`, the URL parts first (in text order).
#[must_use]
pub fn credential_positions(request: &Request) -> Vec<CredentialPosition> {
    let mut out: Vec<CredentialPosition> = url_credentials(&request.url)
        .into_iter()
        .map(CredentialPosition::Url)
        .collect();
    out.extend(rows(
        &request.query_params,
        is_credential_key,
        CredentialPosition::QueryParam,
    ));
    out.extend(rows(
        &request.headers,
        is_secret_header_name,
        CredentialPosition::Header,
    ));
    match &request.body {
        RequestBody::FormUrlEncoded { fields } => {
            out.extend(rows(
                fields,
                is_credential_key,
                CredentialPosition::FormField,
            ));
        }
        RequestBody::Multipart { parts } => out.extend(
            parts
                .iter()
                .enumerate()
                .filter(|(_, part)| part.file_path.is_none() && is_credential_key(&part.key))
                .map(|(index, _)| CredentialPosition::MultipartField(index)),
        ),
        _ => {}
    }
    out.extend(
        auth_secrets(&request.auth)
            .into_iter()
            .map(CredentialPosition::Auth),
    );
    out
}

/// The credential positions of a flow step's overrides: its URL override, its query override
/// rows ([`CredentialPosition::QueryParam`]), its header override rows
/// ([`CredentialPosition::Header`]) and, when the step sends its `body_override` as
/// form-urlencoded, the credential-named lines of it ([`CredentialPosition::FormLine`]). A step
/// without a body kind that overrides a saved form body is not read as a form: which type the
/// text takes depends on the saved request, which the step does not carry.
#[must_use]
pub fn flow_step_credential_positions(step: &FlowStep) -> Vec<CredentialPosition> {
    let mut out: Vec<CredentialPosition> = step
        .url_override
        .as_deref()
        .map(url_credentials)
        .unwrap_or_default()
        .into_iter()
        .map(CredentialPosition::Url)
        .collect();
    out.extend(rows(
        &step.query_override,
        is_credential_key,
        CredentialPosition::QueryParam,
    ));
    out.extend(rows(
        &step.headers_override,
        is_secret_header_name,
        CredentialPosition::Header,
    ));
    if step.body_override_kind == Some(FlowBodyKind::FormUrlencoded)
        && let Some(body) = step.body_override.as_deref()
    {
        out.extend(
            form_line_credentials(body)
                .into_iter()
                .map(CredentialPosition::FormLine),
        );
    }
    out
}

/// The name and the value of a non-URL position of `request` (`None` for a URL part, whose
/// text the caller rewrites by range, or for an index that is out of range).
pub fn credential_value_mut<'a>(
    request: &'a mut Request,
    position: &CredentialPosition,
) -> Option<(String, &'a mut String)> {
    let row = |row: Option<&'a mut KeyValue>| row.map(|row| (row.key.clone(), &mut row.value));
    match position {
        CredentialPosition::Url(_) | CredentialPosition::FormLine(_) => None,
        CredentialPosition::QueryParam(index) => row(request.query_params.get_mut(*index)),
        CredentialPosition::Header(index) => row(request.headers.get_mut(*index)),
        CredentialPosition::FormField(index) => match &mut request.body {
            RequestBody::FormUrlEncoded { fields } => row(fields.get_mut(*index)),
            _ => None,
        },
        CredentialPosition::MultipartField(index) => match &mut request.body {
            RequestBody::Multipart { parts } => parts
                .get_mut(*index)
                .map(|part| (part.key.clone(), &mut part.value)),
            _ => None,
        },
        CredentialPosition::Auth(which) => auth_secret_mut(&mut request.auth, *which)
            .map(|value| (which.label().to_owned(), value)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::models::FormPart;

    fn kv(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: value.to_owned(),
            enabled: true,
            description: None,
        }
    }

    #[test]
    fn url_credentials_find_userinfo_and_credential_query_values_only() {
        let url = "https://bob:pw@api.example.com/v1?page=2&api%5Fkey=AIza123&sig=&x=1#key=frag";
        let found = url_credentials(url);
        let texts: Vec<(&UrlPart, &str)> = found
            .iter()
            .map(|c| (&c.part, &url[c.range.clone()]))
            .collect();
        assert_eq!(
            texts,
            vec![
                (&UrlPart::Userinfo, "bob:pw"),
                (&UrlPart::Query("api%5Fkey".to_owned()), "AIza123"),
                (&UrlPart::Query("sig".to_owned()), ""),
            ]
        );
        // An `@` in the path is no userinfo; a fragment is no query.
        assert!(url_credentials("https://host/a@b#token=x").is_empty());
    }

    #[test]
    fn request_positions_cover_every_credential_place_and_nothing_else() {
        let mut request = Request::new("r1", "w1", "R", "https://u:p@h/x?token=t&q=1").unwrap();
        request.query_params = vec![kv("page", "1"), kv("client_secret", "cs")];
        request.headers = vec![kv("Accept", "*/*"), kv("X-Api-Key", "k")];
        request.body = RequestBody::Multipart {
            parts: vec![
                FormPart {
                    key: "password".into(),
                    value: "pw".into(),
                    file_path: None,
                    enabled: true,
                },
                FormPart {
                    key: "token".into(),
                    value: String::new(),
                    file_path: Some("/tmp/f".into()),
                    enabled: true,
                },
            ],
        };
        request.auth = AuthConfig::AwsSignatureV4 {
            access_key: "AK".into(),
            secret_key_ref: Some("sk".into()),
            region: "r".into(),
            service: "s".into(),
            session_token_ref: None,
        };
        let positions = credential_positions(&request);
        assert_eq!(positions.len(), 6, "{positions:?}");
        let mut values: Vec<(String, String)> = positions
            .iter()
            .filter_map(|position| {
                credential_value_mut(&mut request, position).map(|(n, v)| (n, v.clone()))
            })
            .collect();
        values.sort();
        assert_eq!(
            values,
            vec![
                ("X-Api-Key".to_owned(), "k".to_owned()),
                ("client_secret".to_owned(), "cs".to_owned()),
                ("password".to_owned(), "pw".to_owned()),
                ("secret_key".to_owned(), "sk".to_owned()),
            ]
        );
    }

    #[test]
    fn form_lines_find_credential_values_as_the_flow_runner_reads_them() {
        let text = "grant_type=client_credentials\r\n  client_secret = s3=cr&et \n\n# password=old\n#token=x\napi_key=\nnoeq\n";
        let credentials = form_line_credentials(text);
        let found: Vec<(&str, &str)> = credentials
            .iter()
            .map(|c| (c.name.as_str(), &text[c.range.clone()]))
            .collect();
        // `#token` has no space after `#`: the runner sends it as an enabled field `#token`.
        assert_eq!(
            found,
            vec![
                ("client_secret", "s3=cr&et"),
                ("password", "old"),
                ("#token", "x"),
                ("api_key", ""),
            ]
        );
    }

    #[test]
    fn flow_step_form_lines_count_only_for_a_form_body_kind() {
        let mut step: FlowStep = serde_json::from_value(serde_json::json!({
            "id": "s1",
            "name": "Token",
            "delay_ms": 0,
            "continue_on_error": false,
            "position": 0,
            "body_override": "client_secret=cs"
        }))
        .unwrap();
        assert!(flow_step_credential_positions(&step).is_empty());
        step.body_override_kind = Some(FlowBodyKind::FormUrlencoded);
        assert!(matches!(
            flow_step_credential_positions(&step).as_slice(),
            [CredentialPosition::FormLine(found)] if found.name == "client_secret"
        ));
        step.body_override_kind = Some(FlowBodyKind::Json);
        assert!(flow_step_credential_positions(&step).is_empty());
    }
}
