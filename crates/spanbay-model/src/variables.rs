//! Layered `{{variable}}` scopes and template interpolation for requests.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::models::{EnvironmentVariable, VariableResolutionSource};

/// Failure modes when resolving variables or interpolating a template.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VariableResolutionError {
    #[error("unresolved variables: {keys:?}")]
    UnresolvedVariables { keys: Vec<String> },
}

/// Variable scopes ordered from broadest to narrowest:
/// globals, collection, environment, data, local.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariableScopes {
    globals: Vec<EnvironmentVariable>,
    collection: Vec<EnvironmentVariable>,
    environment: Vec<EnvironmentVariable>,
    data: Vec<EnvironmentVariable>,
    local: Vec<EnvironmentVariable>,
}

impl VariableScopes {
    /// Creates an empty set of scopes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the globals scope; legacy alias of `with_globals`.
    #[must_use]
    pub fn with_workspace_vars(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.globals = variables;
        self
    }

    /// Replaces the globals scope, the broadest one.
    #[must_use]
    pub fn with_globals(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.globals = variables;
        self
    }

    /// Replaces the collection scope.
    #[must_use]
    pub fn with_collection_vars(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.collection = variables;
        self
    }

    /// Replaces the active-environment scope.
    #[must_use]
    pub fn with_environment_vars(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.environment = variables;
        self
    }

    /// Replaces the data scope (e.g. the current iteration row).
    #[must_use]
    pub fn with_data_vars(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.data = variables;
        self
    }

    /// Replaces the local scope, the narrowest and highest-priority one.
    #[must_use]
    pub fn with_local_vars(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.local = variables;
        self
    }

    /// Replaces the local scope; alias of `with_local_vars` for per-request variables.
    #[must_use]
    pub fn with_request_vars(mut self, variables: Vec<EnvironmentVariable>) -> Self {
        self.local = variables;
        self
    }

    /// Resolves a trimmed key from the narrowest enabled scope that defines it.
    pub fn resolve_variable(&self, key: &str) -> Result<String, VariableResolutionError> {
        let key = key.trim();

        self.resolve_plain(key)
            .ok_or_else(|| VariableResolutionError::UnresolvedVariables {
                keys: vec![key.to_owned()],
            })
    }

    /// The scopes narrowest first, each with the source a trace reports for it.
    fn layers(&self) -> [(&[EnvironmentVariable], VariableResolutionSource); 5] {
        [
            (&self.local, VariableResolutionSource::Runtime),
            (&self.data, VariableResolutionSource::Runtime),
            (&self.environment, VariableResolutionSource::Environment),
            (&self.collection, VariableResolutionSource::Collection),
            (&self.globals, VariableResolutionSource::Global),
        ]
    }

    /// The narrowest enabled definition of `key` and its scope. The first enabled row decides,
    /// a secret without a value included ([`EnvironmentVariable::needs_value`]): it shadows the
    /// broader scopes instead of letting a same-named value from another layer through.
    fn binding(&self, key: &str) -> Option<(&EnvironmentVariable, VariableResolutionSource)> {
        self.layers().into_iter().find_map(|(variables, source)| {
            variables
                .iter()
                .find(|variable| variable.enabled && variable.key == key)
                .map(|variable| (variable, source))
        })
    }

    /// Returns the effective value of a variable: the narrowest enabled definition's value, or
    /// none when that definition needs a value (a cleared secret) or no scope defines the key.
    #[must_use]
    pub fn resolve_plain(&self, key: &str) -> Option<String> {
        self.binding(key.trim())
            .filter(|(variable, _)| !variable.needs_value())
            .and_then(|(variable, _)| variable.value.clone())
    }

    /// Resolves `key` the way [`resolve_template`] does, reporting which scope answered and
    /// whether the value is secret. A definition that needs a value reports its scope with no
    /// value, so a send that needs it fails as unresolved.
    #[must_use]
    pub fn resolve_trace(&self, key: &str) -> (Option<String>, VariableResolutionSource, bool) {
        let key = key.trim();
        if let Some(value) = dynamic_variable(key) {
            return (Some(value), VariableResolutionSource::BuiltIn, false);
        }
        match self.binding(key) {
            Some((variable, source)) => (
                variable.value.clone().filter(|_| !variable.needs_value()),
                source,
                variable.is_secret,
            ),
            None => (None, VariableResolutionSource::Unresolved, false),
        }
    }

    /// Merged view of all scopes, narrowest definition winning; a key whose narrowest
    /// definition needs a value is left out (it does not resolve).
    #[must_use]
    pub fn merged_plain_variables(&self) -> BTreeMap<String, String> {
        let mut decided: BTreeMap<&str, Option<&String>> = BTreeMap::new();
        for (variables, _) in self.layers() {
            for variable in variables.iter().filter(|variable| variable.enabled) {
                decided
                    .entry(variable.key.as_str())
                    .or_insert_with(|| variable.value.as_ref().filter(|_| !variable.needs_value()));
            }
        }
        decided
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.clone())))
            .collect()
    }

    /// Plaintext values of enabled secret variables, at least 4 bytes long, so
    /// callers can mask them wherever they would otherwise leave the app.
    #[must_use]
    pub fn secret_values(&self) -> Vec<String> {
        let mut values: Vec<String> = Vec::new();
        for variable in self
            .globals
            .iter()
            .chain(self.collection.iter())
            .chain(self.environment.iter())
            .chain(self.data.iter())
            .chain(self.local.iter())
        {
            let Some(value) = variable.value.as_deref() else {
                continue;
            };
            if variable.enabled
                && variable.is_secret
                && value.len() >= 4
                && !values.iter().any(|existing| existing.as_str() == value)
            {
                values.push(value.to_owned());
            }
        }
        values
    }
}

/// Interpolates `{{key}}` tokens, failing on unresolved keys or secret values.
pub fn resolve_template(
    input: &str,
    scopes: &VariableScopes,
) -> Result<String, VariableResolutionError> {
    resolve_template_inner(input, scopes, true)
}

/// Resolves known variables and leaves unknown `{{tokens}}` untouched. Used by
/// `pm.variables.replaceIn`, which must never throw for a missing key.
#[must_use]
pub fn resolve_template_lenient(input: &str, scopes: &VariableScopes) -> String {
    resolve_template_inner(input, scopes, false).unwrap_or_else(|_| input.to_owned())
}

fn resolve_template_inner(
    input: &str,
    scopes: &VariableScopes,
    strict: bool,
) -> Result<String, VariableResolutionError> {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    let mut unresolved_keys = Vec::new();

    while let Some(start_offset) = input[cursor..].find("{{") {
        let start = cursor + start_offset;
        output.push_str(&input[cursor..start]);

        let token_start = start + 2;
        let Some(end_offset) = input[token_start..].find("}}") else {
            output.push_str(&input[start..]);
            cursor = input.len();
            break;
        };

        let end = token_start + end_offset;
        let key = input[token_start..end].trim();

        if let Some(value) = dynamic_variable(key) {
            output.push_str(&value);
        } else {
            match scopes.resolve_variable(key) {
                Ok(value) => output.push_str(&value),
                Err(VariableResolutionError::UnresolvedVariables { keys }) => {
                    if strict {
                        for key in keys {
                            push_unique(&mut unresolved_keys, key);
                        }
                    } else {
                        output.push_str("{{");
                        output.push_str(key);
                        output.push_str("}}");
                    }
                }
            }
        }

        cursor = end + 2;
    }

    output.push_str(&input[cursor..]);

    if unresolved_keys.is_empty() {
        Ok(output)
    } else {
        Err(VariableResolutionError::UnresolvedVariables {
            keys: unresolved_keys,
        })
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

// ---------------------------------------------------------------------------
// Dynamic variables (Postman `{{$name}}`)
// ---------------------------------------------------------------------------

static RANDOM_STATE: AtomicU64 = AtomicU64::new(0);

pub fn next_random() -> u64 {
    let initial_seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        .max(1);
    let step = |seed: u64| {
        let mut next = if seed == 0 { initial_seed } else { seed };
        next ^= next << 13;
        next ^= next >> 7;
        next ^= next << 17;
        next
    };
    // One atomic step per call: concurrent callers never observe the same state, so ids built
    // from this generator cannot repeat across threads.
    let previous = RANDOM_STATE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |seed| {
            Some(step(seed))
        })
        .unwrap_or_else(|seed| seed);
    step(previous)
}

fn random_index(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (next_random() % len as u64) as usize
    }
}

fn pick(values: &[&str]) -> String {
    values[random_index(values.len())].to_owned()
}

fn digits(count: usize) -> String {
    (0..count)
        .map(|_| char::from(b'0' + (next_random() % 10) as u8))
        .collect()
}

/// Every name [`dynamic_variable`] resolves, for the editor's `{{$` completion. The
/// `dynamic_names_match_the_resolver` test scans the resolver's own arms, so the two cannot drift.
const DYNAMIC_VARIABLE_NAMES: &[&str] = &[
    "$lcurly",
    "$guid",
    "$randomUUID",
    "$timestamp",
    "$isoTimestamp",
    "$randomInt",
    "$randomBoolean",
    "$randomAlphaNumeric",
    "$randomHexColor",
    "$randomColor",
    "$randomFirstName",
    "$randomLastName",
    "$randomFullName",
    "$randomEmail",
    "$randomUserName",
    "$randomPassword",
    "$randomPhoneNumber",
    "$randomCity",
    "$randomStreetName",
    "$randomStreetAddress",
    "$randomCountry",
    "$randomCountryCode",
    "$randomZipCode",
    "$randomUrl",
    "$randomDomainName",
    "$randomIP",
    "$randomMACAddress",
    "$randomSemver",
    "$randomPrice",
    "$randomCompanyName",
    "$randomJobTitle",
    "$randomWeekday",
    "$randomMonth",
    "$randomWords",
];

/// The `{{$name}}` variables the resolver generates.
#[must_use]
pub fn dynamic_variable_names() -> &'static [&'static str] {
    DYNAMIC_VARIABLE_NAMES
}

/// Built-in `{{$name}}` variables. `$lcurly` is the escape for a literal `{`: the resolver emits
/// its value without re-scanning it, so untrusted text (tool arguments) can be inserted into a
/// template with every `{` written as `{{$lcurly}}` and can then never open a token.
fn dynamic_variable(key: &str) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let value = match key {
        "$lcurly" => "{".to_owned(),
        "$guid" | "$randomUUID" => uuid_v4(),
        "$timestamp" => now.as_secs().to_string(),
        "$isoTimestamp" => iso_timestamp(now.as_millis() as u64),
        "$randomInt" => (next_random() % 1001).to_string(),
        "$randomBoolean" => next_random().is_multiple_of(2).to_string(),
        "$randomAlphaNumeric" => {
            const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
            char::from(CHARS[(next_random() % CHARS.len() as u64) as usize]).to_string()
        }
        "$randomHexColor" => format!("#{:06x}", next_random() & 0x00ff_ffff),
        "$randomColor" => pick(&[
            "red", "green", "blue", "yellow", "purple", "orange", "pink", "cyan", "black", "white",
            "gray",
        ]),
        "$randomFirstName" => pick(&[
            "Avery", "Jordan", "Riley", "Morgan", "Casey", "Quinn", "Taylor", "Jamie", "Alex",
            "Robin",
        ]),
        "$randomLastName" => pick(&[
            "Chen", "Patel", "Garcia", "Kim", "Nguyen", "Smith", "Brown", "Wilson", "Lee", "Martin",
        ]),
        "$randomFullName" => format!(
            "{} {}",
            pick(&[
                "Avery", "Jordan", "Riley", "Morgan", "Casey", "Quinn", "Taylor", "Jamie", "Alex",
                "Robin"
            ]),
            pick(&[
                "Chen", "Patel", "Garcia", "Kim", "Nguyen", "Smith", "Brown", "Wilson", "Lee",
                "Martin"
            ])
        ),
        "$randomEmail" => format!(
            "{}.{}@{}",
            pick(&["avery", "jordan", "riley", "morgan", "casey", "quinn"]),
            pick(&["chen", "patel", "garcia", "kim", "nguyen", "smith"]),
            pick(&["example.com", "example.net", "mail.test"])
        ),
        "$randomUserName" => format!(
            "{}{}",
            pick(&["avery", "jordan", "riley", "morgan", "casey", "quinn"]),
            digits(3)
        ),
        "$randomPassword" => format!(
            "{}{}{}",
            pick(&["aB", "xQ", "mZ", "kL"]),
            digits(6),
            pick(&["vR", "nY", "pT"])
        ),
        "$randomPhoneNumber" => format!("{}-{}-{}", digits(3), digits(3), digits(4)),
        "$randomCity" => pick(&[
            "Springfield",
            "Riverton",
            "Lakeview",
            "Fair Oaks",
            "Brookhaven",
            "Cedar Falls",
        ]),
        "$randomStreetName" => pick(&["Maple", "Oak", "Cedar", "Sunset", "Park", "Willow"]),
        "$randomStreetAddress" => {
            format!("{} {} Street", digits(3), pick(&["Maple", "Oak", "Cedar"]))
        }
        "$randomCountry" => pick(&[
            "United States",
            "Canada",
            "Germany",
            "Japan",
            "Australia",
            "Brazil",
            "India",
        ]),
        "$randomCountryCode" => pick(&["US", "CA", "DE", "JP", "AU", "BR", "IN"]),
        "$randomZipCode" => digits(5),
        "$randomUrl" => format!(
            "https://{}.example.com",
            pick(&["api", "app", "shop", "docs", "staging"])
        ),
        "$randomDomainName" => format!(
            "{}.{}",
            pick(&["alpha", "beta", "gamma", "delta"]),
            pick(&["com", "net", "org", "io"])
        ),
        "$randomIP" => format!(
            "{}.{}.{}.{}",
            next_random() % 256,
            next_random() % 256,
            next_random() % 256,
            next_random() % 256
        ),
        "$randomMACAddress" => (0..6)
            .map(|_| format!("{:02x}", next_random() % 256))
            .collect::<Vec<_>>()
            .join(":"),
        "$randomSemver" => format!(
            "{}.{}.{}",
            next_random() % 10,
            next_random() % 20,
            next_random() % 30
        ),
        "$randomPrice" => format!("{}.{}", next_random() % 1000, digits(2)),
        "$randomCompanyName" => format!(
            "{} {}",
            pick(&["Acme", "Globex", "Initech", "Umbrella", "Hooli", "Stark"]),
            pick(&["Labs", "Systems", "Group", "Works", "Industries"])
        ),
        "$randomJobTitle" => pick(&[
            "Software Engineer",
            "Product Manager",
            "Data Analyst",
            "QA Engineer",
            "Designer",
            "DevOps Engineer",
        ]),
        "$randomWeekday" => pick(&[
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ]),
        "$randomMonth" => pick(&[
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ]),
        "$randomWords" => format!(
            "{} {} {}",
            pick(&["alpha", "bravo", "charlie", "delta"]),
            pick(&["red", "green", "blue", "silver"]),
            pick(&["river", "stone", "field", "cloud"])
        ),
        _ => return None,
    };

    Some(value)
}

/// Entity ids and `{{$guid}}` must not collide or be guessable from earlier values, which the
/// clock-seeded [`next_random`] cannot promise.
pub use spanbay_base::id::uuid_v4;

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, always 24 characters for years 0000-9999.
fn iso_timestamp(millis: u64) -> String {
    chrono::DateTime::from_timestamp_millis(i64::try_from(millis).unwrap_or(i64::MAX))
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_narrowest_scope_first() {
        let scopes = VariableScopes::new()
            .with_globals(vec![
                EnvironmentVariable::plain("baseUrl", "https://global").unwrap(),
            ])
            .with_collection_vars(vec![
                EnvironmentVariable::plain("baseUrl", "https://collection").unwrap(),
            ])
            .with_environment_vars(vec![
                EnvironmentVariable::plain("baseUrl", "https://environment").unwrap(),
            ])
            .with_data_vars(vec![
                EnvironmentVariable::plain("baseUrl", "https://data").unwrap(),
            ])
            .with_local_vars(vec![
                EnvironmentVariable::plain("baseUrl", "https://local").unwrap(),
            ]);

        assert_eq!(
            scopes.resolve_variable("baseUrl").unwrap(),
            "https://local".to_owned()
        );
    }

    #[test]
    fn resolves_plain_and_masked_environment_variables() {
        let scopes = VariableScopes::new().with_environment_vars(vec![
            EnvironmentVariable::plain("baseUrl", "https://api.example.com").unwrap(),
            EnvironmentVariable::plain("customerId", "cus_7781").unwrap(),
            EnvironmentVariable::masked("token", "local-secret").unwrap(),
        ]);

        let resolved =
            resolve_template("{{baseUrl}}/customers/{{customerId}}?t={{token}}", &scopes).unwrap();
        assert_eq!(
            resolved,
            "https://api.example.com/customers/cus_7781?t=local-secret"
        );
    }

    #[test]
    fn resolve_template_reports_all_unresolved_variables() {
        let scopes = VariableScopes::new();
        let error = resolve_template("{{baseUrl}}/{{missing}}", &scopes).unwrap_err();
        assert_eq!(
            error,
            VariableResolutionError::UnresolvedVariables {
                keys: vec!["baseUrl".to_owned(), "missing".to_owned()]
            }
        );
    }

    #[test]
    fn lenient_resolution_leaves_unknown_tokens() {
        let scopes = VariableScopes::new()
            .with_environment_vars(vec![EnvironmentVariable::plain("known", "yes").unwrap()]);
        assert_eq!(
            resolve_template_lenient("{{known}}/{{missing}}", &scopes),
            "yes/{{missing}}"
        );
    }

    #[test]
    fn resolution_is_single_pass_and_lcurly_escapes_a_brace() {
        let scopes = VariableScopes::new().with_environment_vars(vec![
            EnvironmentVariable::plain("outer", "{{inner}}").unwrap(),
            EnvironmentVariable::plain("inner", "SECRET").unwrap(),
        ]);

        // A substituted value is never scanned again for tokens.
        assert_eq!(resolve_template("{{outer}}", &scopes).unwrap(), "{{inner}}");
        // `{{$lcurly}}` emits one literal `{`, so escaped text cannot open a token.
        assert_eq!(
            resolve_template("{{$lcurly}}{{$lcurly}}inner}} {{inner}}", &scopes).unwrap(),
            "{{inner}} SECRET"
        );
    }

    #[test]
    fn dynamic_names_match_the_resolver() {
        let source = include_str!("variables.rs");
        let body = source
            .split("fn dynamic_variable(key: &str)")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap();
        let mut arms: Vec<&str> = body
            .match_indices("\"$")
            .filter_map(|(at, _)| {
                let rest = &body[at + 1..];
                let end = rest.find('"')?;
                let after = rest[end + 1..].trim_start();
                (after.starts_with("=>") || after.starts_with('|')).then_some(&rest[..end])
            })
            .collect();
        let mut listed = dynamic_variable_names().to_vec();
        arms.sort_unstable();
        listed.sort_unstable();
        assert_eq!(arms, listed);
    }

    #[test]
    fn iso_timestamp_is_fixed_width_utc_with_milliseconds() {
        for (millis, expected) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (5, "1970-01-01T00:00:00.005Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_709_251_199_999, "2024-02-29T23:59:59.999Z"),
            (4_102_444_800_000, "2100-01-01T00:00:00.000Z"),
        ] {
            assert_eq!(iso_timestamp(millis), expected);
        }
        let live = resolve_template("{{$isoTimestamp}}", &VariableScopes::new()).unwrap();
        let shape: String = live
            .chars()
            .map(|c| if c.is_ascii_digit() { 'd' } else { c })
            .collect();
        assert_eq!(shape, "dddd-dd-ddTdd:dd:dd.dddZ");
    }

    #[test]
    fn dynamic_variables_are_generated() {
        let scopes = VariableScopes::new();
        let guid = resolve_template("{{$guid}}", &scopes).unwrap();
        assert_eq!(guid.len(), 36);
        assert_eq!(
            guid.chars().filter(|character| *character == '-').count(),
            4
        );

        let timestamp: u64 = resolve_template("{{$timestamp}}", &scopes)
            .unwrap()
            .parse()
            .unwrap();
        assert!(timestamp > 1_600_000_000);
        assert!(
            resolve_template("{{$randomInt}}", &scopes)
                .unwrap()
                .parse::<u16>()
                .is_ok()
        );
    }

    #[test]
    fn a_secret_without_a_value_shadows_broader_scopes_and_does_not_resolve() {
        let mut cleared = EnvironmentVariable::masked("token", "").unwrap();
        let scopes = VariableScopes::new()
            .with_globals(vec![
                EnvironmentVariable::plain("token", "dev-token").unwrap(),
            ])
            .with_environment_vars(vec![cleared.clone()]);
        assert_eq!(scopes.resolve_plain("token"), None);
        assert_eq!(
            resolve_template("Bearer {{token}}", &scopes).unwrap_err(),
            VariableResolutionError::UnresolvedVariables {
                keys: vec!["token".to_owned()]
            }
        );
        assert_eq!(
            scopes.resolve_trace("token"),
            (None, VariableResolutionSource::Environment, true)
        );
        assert!(!scopes.merged_plain_variables().contains_key("token"));

        // A disabled cleared secret is not a definition: the broader value resolves.
        cleared.enabled = false;
        let scopes = scopes.with_environment_vars(vec![cleared]);
        assert_eq!(scopes.resolve_plain("token").as_deref(), Some("dev-token"));
    }

    #[test]
    fn merges_plain_variables_across_scopes() {
        let scopes = VariableScopes::new()
            .with_globals(vec![EnvironmentVariable::plain("a", "1").unwrap()])
            .with_environment_vars(vec![EnvironmentVariable::plain("b", "2").unwrap()]);
        let merged = scopes.merged_plain_variables();
        assert_eq!(merged.get("a").map(String::as_str), Some("1"));
        assert_eq!(merged.get("b").map(String::as_str), Some("2"));
    }
}
