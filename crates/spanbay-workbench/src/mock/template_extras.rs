//! Value generators behind the template helpers: random numbers, dates in date-fns patterns and
//! the supported `faker` methods (mapped onto the request variables' `{{$random...}}` set).

use std::fmt::Write;

use chrono::{DateTime, NaiveDate, Utc};

use crate::workspace::variables::next_random;
use crate::workspace::{VariableScopes, resolve_template_lenient};

/// A random integer between `min` and `max` inclusive (either order).
pub(super) fn random_between(min: i64, max: i64) -> i64 {
    let (low, high) = if min <= max { (min, max) } else { (max, min) };
    let span = high.saturating_sub(low).saturating_add(1).max(1);
    low.saturating_add(i64::try_from(next_random() % span.unsigned_abs()).unwrap_or(0))
}

/// `faker` methods (Faker.js names, old and new namespaces) and the variable that generates them.
const FAKER: [(&str, &str); 44] = [
    ("person.firstName", "$randomFirstName"),
    ("name.firstName", "$randomFirstName"),
    ("person.lastName", "$randomLastName"),
    ("name.lastName", "$randomLastName"),
    ("person.fullName", "$randomFullName"),
    ("name.fullName", "$randomFullName"),
    ("person.jobTitle", "$randomJobTitle"),
    ("name.jobTitle", "$randomJobTitle"),
    ("internet.email", "$randomEmail"),
    ("internet.userName", "$randomUserName"),
    ("internet.username", "$randomUserName"),
    ("internet.password", "$randomPassword"),
    ("internet.url", "$randomUrl"),
    ("internet.domainName", "$randomDomainName"),
    ("internet.ip", "$randomIP"),
    ("internet.ipv4", "$randomIP"),
    ("internet.mac", "$randomMACAddress"),
    ("internet.color", "$randomHexColor"),
    ("color.rgb", "$randomHexColor"),
    ("color.human", "$randomColor"),
    ("string.uuid", "$guid"),
    ("datatype.uuid", "$guid"),
    ("string.alphanumeric", "$randomAlphaNumeric"),
    ("datatype.boolean", "$randomBoolean"),
    ("phone.number", "$randomPhoneNumber"),
    ("phone.phoneNumber", "$randomPhoneNumber"),
    ("location.city", "$randomCity"),
    ("address.city", "$randomCity"),
    ("location.street", "$randomStreetName"),
    ("address.streetName", "$randomStreetName"),
    ("location.streetAddress", "$randomStreetAddress"),
    ("address.streetAddress", "$randomStreetAddress"),
    ("location.country", "$randomCountry"),
    ("address.country", "$randomCountry"),
    ("location.countryCode", "$randomCountryCode"),
    ("address.countryCode", "$randomCountryCode"),
    ("location.zipCode", "$randomZipCode"),
    ("address.zipCode", "$randomZipCode"),
    ("company.name", "$randomCompanyName"),
    ("company.companyName", "$randomCompanyName"),
    ("system.semver", "$randomSemver"),
    ("commerce.price", "$randomPrice"),
    ("date.weekday", "$randomWeekday"),
    ("date.month", "$randomMonth"),
];

/// A value for `{{faker 'method' ...}}`, or `None` for a method SpanBay does not generate.
/// `number.int`/`datatype.number` take optional `min` and `max`; `lorem.word(s)` give words.
pub(super) fn faker_value(method: &str, args: &[String]) -> Option<String> {
    let number = |index: usize| {
        args.get(index)
            .and_then(|value| value.trim().parse::<i64>().ok())
    };
    match method {
        "number.int" | "datatype.number" | "random.number" => {
            let min = number(0).unwrap_or(0);
            let max = number(1).unwrap_or(if args.is_empty() {
                99_999
            } else {
                min.saturating_add(100)
            });
            return Some(random_between(min, max).to_string());
        }
        "lorem.word" => {
            return dynamic("$randomWords")
                .and_then(|words| words.split_whitespace().next().map(str::to_owned));
        }
        "lorem.words" | "lorem.sentence" => return dynamic("$randomWords"),
        _ => {}
    }
    let variable = FAKER
        .iter()
        .find(|(name, _)| *name == method)
        .map(|(_, variable)| *variable)?;
    dynamic(variable)
}

fn dynamic(variable: &str) -> Option<String> {
    let literal = format!("{{{{{variable}}}}}");
    let value = resolve_template_lenient(&literal, &VariableScopes::new());
    (value != literal).then_some(value)
}

/// `value` as an instant: RFC 3339, or a `YYYY-MM-DD` date at midnight UTC.
pub(super) fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|date| date.and_utc())
        })
}

/// date-fns pattern tokens and their `strftime` equivalents, longest first.
const DATE_TOKENS: [(&str, &str); 22] = [
    ("yyyy", "%Y"),
    ("yy", "%y"),
    ("MMMM", "%B"),
    ("MMM", "%b"),
    ("MM", "%m"),
    ("M", "%-m"),
    ("dd", "%d"),
    ("d", "%-d"),
    ("EEEE", "%A"),
    ("EEE", "%a"),
    ("HH", "%H"),
    ("H", "%-H"),
    ("hh", "%I"),
    ("h", "%-I"),
    ("mm", "%M"),
    ("m", "%-M"),
    ("ss", "%S"),
    ("s", "%-S"),
    ("SSS", "%3f"),
    ("a", "%p"),
    ("xxx", "%:z"),
    ("t", "%s"),
];

/// Formats `date` with a date-fns pattern (`yyyy-MM-dd'T'HH:mm:ss`): the tokens of
/// [`DATE_TOKENS`], `'quoted'` literal text (`''` is a quote), anything else as written.
pub(super) fn format_date(date: DateTime<Utc>, pattern: &str) -> String {
    let mut spec = String::with_capacity(pattern.len() * 2);
    let mut rest = pattern;
    while let Some(character) = rest.chars().next() {
        if character == '\'' {
            let body = &rest[1..];
            if let Some(stripped) = body.strip_prefix('\'') {
                spec.push('\'');
                rest = stripped;
                continue;
            }
            let end = body.find('\'').unwrap_or(body.len());
            spec.push_str(&body[..end].replace('%', "%%"));
            rest = body.get(end + 1..).unwrap_or_default();
            continue;
        }
        if let Some((token, strftime)) = DATE_TOKENS
            .iter()
            .find(|(token, _)| rest.starts_with(token))
        {
            spec.push_str(strftime);
            rest = &rest[token.len()..];
            continue;
        }
        if character == '%' {
            spec.push_str("%%");
        } else {
            spec.push(character);
        }
        rest = &rest[character.len_utf8()..];
    }
    let mut out = String::new();
    if write!(out, "{}", date.format(&spec)).is_err() {
        return date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    }
    out
}
