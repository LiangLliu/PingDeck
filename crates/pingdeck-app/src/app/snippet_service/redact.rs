//! The one credential rule set of copied code: Requests' ⌘⇧C, the code
//! dialog and History's copy all pass through here, after inherited auth is resolved.
//!
//! The positions come from `pingdeck_model::credentials::credential_positions`, the rule set
//! Postman export and folder sync share: a literal credential (a secret-named header, query or
//! form value, URL userinfo, an auth leaf) never reaches a snippet. In template mode it becomes a
//! `{{placeholder}}` and variables stay as written. In values mode it becomes a shell variable
//! (`$TOKEN`), every other `{{variable}}` is expanded to its value in the chosen environment, and
//! a secret variable is written as a shell variable named after it; a credential position is
//! never expanded, whatever its variable holds.

use pingdeck_model::credentials::{
    CredentialPosition, UrlCredential, UrlPart, credential_positions, credential_value_mut,
};

use crate::workspace::{AuthConfig, Request, RequestBody, VariableScopes};

use super::SnippetMode;

/// The rewriting of one snippet: the mode, the scopes of values mode and the shell variables
/// written so far (the cURL renderer keeps them outside single quotes).
pub(super) struct Redactor<'a> {
    mode: SnippetMode,
    scopes: Option<&'a VariableScopes>,
    pub(super) shell_vars: Vec<String>,
}

impl<'a> Redactor<'a> {
    pub(super) fn new(mode: SnippetMode, scopes: Option<&'a VariableScopes>) -> Self {
        Self {
            mode,
            scopes,
            shell_vars: Vec::new(),
        }
    }

    /// `X-Api-Key` → `$X_API_KEY`, recorded for the renderer.
    fn shell_var(&mut self, name: &str) -> String {
        let mut var: String = name
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        if var.is_empty() || var.starts_with(|c: char| c.is_ascii_digit()) {
            var.insert(0, '_');
        }
        let var = format!("${var}");
        if !self.shell_vars.contains(&var) {
            self.shell_vars.push(var.clone());
        }
        var
    }

    /// What a literal credential named `name` is written as.
    fn placeholder(&mut self, name: &str) -> String {
        match self.mode {
            SnippetMode::Template => {
                let snake: String = name
                    .trim()
                    .to_ascii_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                format!("{{{{{snake}}}}}")
            }
            SnippetMode::Values => self.shell_var(name),
        }
    }

    /// A credential position: a literal becomes a placeholder; a value holding variables keeps
    /// them in template mode and turns each into a shell variable in values mode.
    fn credential(&mut self, name: &str, value: &str) -> String {
        if value.trim().is_empty() {
            return value.to_owned();
        }
        if !value.contains("{{") {
            return self.placeholder(name);
        }
        match self.mode {
            SnippetMode::Template => value.to_owned(),
            SnippetMode::Values => {
                self.map_tokens(value, |redactor, token| Some(redactor.shell_var(token)))
            }
        }
    }

    /// Replaces each `{{token}}` of `value` with what `f` returns; `None` keeps the token.
    fn map_tokens(
        &mut self,
        value: &str,
        mut f: impl FnMut(&mut Self, &str) -> Option<String>,
    ) -> String {
        let mut out = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(start) = rest.find("{{") {
            let Some(length) = rest[start + 2..].find("}}") else {
                break;
            };
            let end = start + 2 + length + 2;
            out.push_str(&rest[..start]);
            let name = rest[start + 2..start + 2 + length].trim();
            match f(self, name) {
                Some(replacement) => out.push_str(&replacement),
                None => out.push_str(&rest[start..end]),
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    /// Values mode: a plain variable's value, a secret variable as a shell variable; dynamic
    /// (`$guid`) and unresolved tokens stay. Template mode returns `value` unchanged.
    fn expand(&mut self, value: &str) -> String {
        let Some(scopes) = self.scopes.filter(|_| self.mode == SnippetMode::Values) else {
            return value.to_owned();
        };
        if !value.contains("{{") {
            return value.to_owned();
        }
        self.map_tokens(value, |redactor, name| {
            if name.starts_with('$') {
                return None;
            }
            match scopes.resolve_trace(name) {
                (Some(_), _, true) => Some(redactor.shell_var(name)),
                (Some(found), _, false) => Some(found),
                (None, _, _) => None,
            }
        })
    }

    /// The URL with its credential parts (`found`, ascending) replaced: literal userinfo becomes
    /// `username:password` placeholders, a credential query value goes through
    /// [`Self::credential`]. Replacements are worked out in text order (the shell variables keep
    /// that order) and spliced in from the end.
    fn url(&mut self, url: &str, found: &[UrlCredential]) -> String {
        let mut replacements = Vec::with_capacity(found.len());
        for credential in found {
            let Some(text) = url.get(credential.range.clone()) else {
                continue;
            };
            let replacement = match &credential.part {
                UrlPart::Userinfo if !text.contains("{{") => {
                    let user = self.placeholder("username");
                    let password = self.placeholder("password");
                    format!("{user}:{password}")
                }
                UrlPart::Userinfo => self.credential("userinfo", text),
                UrlPart::Query(name) => self.credential(name, text),
            };
            replacements.push((credential.range.clone(), replacement));
        }
        let mut out = url.to_owned();
        for (range, replacement) in replacements.into_iter().rev() {
            out.replace_range(range, &replacement);
        }
        out
    }

    /// Rewrites `request` (auth already effective) for this snippet: credentials first, then,
    /// in values mode, every remaining `{{variable}}`.
    pub(super) fn apply(&mut self, request: &mut Request) {
        let mut url_parts = Vec::new();
        for position in credential_positions(request) {
            if let CredentialPosition::Url(found) = position {
                url_parts.push(found);
            } else if let Some((name, value)) = credential_value_mut(request, &position) {
                *value = self.credential(&name, value);
            }
        }
        request.url = self.url(&request.url, &url_parts);
        if self.mode == SnippetMode::Template || self.scopes.is_none() {
            return;
        }

        request.url = self.expand(&request.url);
        for row in request
            .query_params
            .iter_mut()
            .chain(request.headers.iter_mut())
            .chain(request.path_variables.iter_mut())
        {
            row.key = self.expand(&row.key);
            row.value = self.expand(&row.value);
        }
        match &mut request.auth {
            AuthConfig::Basic { username, .. } | AuthConfig::Digest { username, .. } => {
                *username = self.expand(username);
            }
            AuthConfig::ApiKey { key, .. } => *key = self.expand(key),
            AuthConfig::AwsSignatureV4 {
                access_key,
                region,
                service,
                ..
            } => {
                *access_key = self.expand(access_key);
                *region = self.expand(region);
                *service = self.expand(service);
            }
            _ => {}
        }
        match &mut request.body {
            RequestBody::Json { content } | RequestBody::RawText { content, .. } => {
                *content = self.expand(content);
            }
            RequestBody::FormUrlEncoded { fields } => {
                for field in fields {
                    field.key = self.expand(&field.key);
                    field.value = self.expand(&field.value);
                }
            }
            RequestBody::Multipart { parts } => {
                for part in parts {
                    part.key = self.expand(&part.key);
                    part.value = self.expand(&part.value);
                    if let Some(path) = part.file_path.as_mut() {
                        *path = self.expand(path);
                    }
                }
            }
            RequestBody::Graphql {
                query,
                variables_json,
                ..
            } => {
                *query = self.expand(query);
                if let Some(variables) = variables_json.as_mut() {
                    *variables = self.expand(variables);
                }
            }
            RequestBody::Binary { file_path, .. } => *file_path = self.expand(file_path),
            RequestBody::None => {}
        }
    }
}
