use super::helpers::{
    Ids, RequestDraft, body_from_text, build_request, collection_name_from_url, header_value,
    http_method, method_label, non_empty_or, query_pairs, request_name, set_header,
    split_url_query, take_authorization_auth,
};
use super::{ImportError, ImportedCollection};
use crate::workspace::models::{
    AuthConfig, Collection, FormPart, HttpMethod, HttpVersionPreference, KeyValue,
    MAX_REDIRECT_HOPS, NetworkField, NetworkOptions, RequestBody,
};

/// The network options a cURL command sets; `None` leaves the request's default.
#[derive(Debug, Default)]
struct CurlNetwork {
    follow_redirects: Option<bool>,
    follow_auth_header: Option<bool>,
    max_redirects: Option<u32>,
    verify_tls: Option<bool>,
    total_timeout_ms: Option<u64>,
    retry_count: Option<u8>,
    proxy_url: Option<String>,
    http_version: Option<HttpVersionPreference>,
}

impl CurlNetwork {
    /// Writes the set options as the request's own values (none follows the global setting).
    fn apply(&self, options: &mut NetworkOptions) {
        let mut own = |field: NetworkField| options.inherit.retain(|other| *other != field);
        if self.follow_redirects.is_some() {
            own(NetworkField::FollowRedirects);
        }
        if self.max_redirects.is_some() {
            own(NetworkField::MaxRedirects);
        }
        if self.verify_tls.is_some() {
            own(NetworkField::VerifyTls);
        }
        if self.retry_count.is_some() {
            own(NetworkField::RetryCount);
        }
        if self.proxy_url.is_some() {
            own(NetworkField::ProxyUrl);
        }
        if self.http_version.is_some() {
            own(NetworkField::HttpVersion);
        }
        if let Some(follow) = self.follow_redirects {
            options.follow_redirects = follow;
        }
        if let Some(follow) = self.follow_auth_header {
            options.follow_auth_header = follow;
        }
        if let Some(hops) = self.max_redirects {
            options.max_redirects = hops;
        }
        if let Some(verify) = self.verify_tls {
            options.verify_tls = verify;
        }
        if let Some(ms) = self.total_timeout_ms {
            options.total_timeout_ms = Some(ms);
        }
        if let Some(count) = self.retry_count {
            options.retry_count = count;
        }
        if let Some(proxy) = &self.proxy_url {
            options.proxy_url = Some(proxy.clone());
        }
        if let Some(version) = self.http_version {
            options.http_version = version;
        }
    }
}

/// curl's `-m` seconds (fractions allowed) as whole milliseconds, at least 1; `None` when it is
/// not a positive number.
fn seconds_to_ms(raw: &str) -> Option<u64> {
    let seconds = raw.trim().parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    let ms = (seconds * 1000.0).round();
    // `as` saturates; the store clamps to its own upper bound anyway.
    Some((ms as u64).max(1))
}

/// Imports a single cURL command.
///
/// Supported: `-X/--request`, positional URL, `-H/--header`, `-d/--data`,
/// `--data-raw/--data-binary/--data-ascii/--data-urlencode`, `--json`,
/// `-F/--form`, `--form-string`, `-u/--user`, `--digest`, `--url`, `-G/--get`,
/// `-I/--head`, bash `$'…'` and Chrome's cmd `^"…^"` quoting. Transport flags become the
/// request's network options: `-L` (and `--location-trusted`), `--max-redirs`, `-k`,
/// `-m/--max-time` (the total time limit), `--retry`, `-x/--proxy`, `-0/--http1.0`, `--http1.1`, `--http2`. Output and
/// display flags (`-s`, `-v`, `-o`, ...) change nothing about the request and are skipped;
/// `--compressed` needs nothing because responses are always decompressed. Every other flag,
/// including the ones whose setting lives elsewhere (client certificates, connect timeout), is
/// reported as a warning. Without `-L` the request keeps the default of following redirects,
/// since browsers' "Copy as cURL" never writes it.
pub fn import_curl(text: &str, workspace_id: &str) -> Result<ImportedCollection, ImportError> {
    let tokens = tokenize_curl(text);
    let command = parse_curl_command(&tokens);

    let Some(raw_url) = command.url else {
        return Err(ImportError::MissingUrl);
    };

    let mut warnings = command.warnings;
    let name = non_empty_or(&collection_name_from_url(&raw_url), "cURL import");

    let mut headers = command.headers;
    let content_type = header_value(&headers, "content-type").map(str::to_string);
    let auth = take_authorization_auth(&mut headers)
        .or_else(|| {
            command.username.map(|(username, password)| {
                if command.digest {
                    AuthConfig::Digest {
                        username,
                        password_ref: Some(password),
                    }
                } else {
                    AuthConfig::Basic {
                        username,
                        password_ref: password,
                    }
                }
            })
        })
        .unwrap_or(AuthConfig::None);

    let (base_url, raw_query) = split_url_query(&raw_url);
    let mut query_params = query_pairs(raw_query);

    let mut body = RequestBody::None;
    if command.to_query {
        let data = command.data.join("&");
        query_params.extend(query_pairs(&data));
        if !command.forms.is_empty() {
            warnings.push("curl: -F form parts are ignored in -G query mode".to_string());
        }
    } else if !command.forms.is_empty() {
        body = RequestBody::Multipart {
            parts: command.forms,
        };
    } else if !command.json_data.is_empty() {
        if content_type.is_none() {
            set_header(&mut headers, "Content-Type", "application/json");
        }
        body = RequestBody::Json {
            content: command.json_data.join("&"),
        };
    } else if !command.data.is_empty() {
        // curl sends `-d` data as a form unless told otherwise.
        if content_type.is_none() {
            set_header(
                &mut headers,
                "Content-Type",
                "application/x-www-form-urlencoded",
            );
        }
        body = body_from_text(&command.data.join("&"), content_type.as_deref());
    }

    let has_body = !matches!(body, RequestBody::None);
    let method = match command.method.as_deref() {
        Some(raw) => match http_method(raw) {
            Some(method) => method,
            None => {
                warnings.push(format!(
                    "curl: unsupported method '{raw}'; using {}",
                    method_label(if has_body {
                        HttpMethod::Post
                    } else {
                        HttpMethod::Get
                    })
                ));
                if has_body {
                    HttpMethod::Post
                } else {
                    HttpMethod::Get
                }
            }
        },
        None if command.head => HttpMethod::Head,
        None if command.to_query => HttpMethod::Get,
        None if has_body => HttpMethod::Post,
        None => HttpMethod::Get,
    };

    let mut ids = Ids::new(&name);
    let collection = Collection::new(ids.next("col"), workspace_id, &name)?;
    let collection_id = collection.id.clone();

    let request_title = request_name(method, base_url);
    if command.dropped_local_path && !command.to_query {
        warnings.push(format!(
            "request '{request_title}': local file path was not imported; select the file again before sending"
        ));
    }

    let draft = RequestDraft {
        name: request_title,
        method,
        url: base_url.to_string(),
        query_params,
        headers,
        body,
        auth,
        description: String::new(),
        folder_id: None,
    };
    let mut request = build_request(draft, &mut ids, workspace_id, &collection_id, 0)?;
    command.network.apply(&mut request.network_options);

    Ok(ImportedCollection {
        collection,
        folders: Vec::new(),
        requests: vec![request],
        warnings,
    })
}

#[derive(Debug, Default)]
struct CurlCommand {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<KeyValue>,
    data: Vec<String>,
    json_data: Vec<String>,
    forms: Vec<FormPart>,
    username: Option<(String, String)>,
    to_query: bool,
    head: bool,
    digest: bool,
    network: CurlNetwork,
    /// Set when a `-F name=@path` / `-F name=<path` local file reference was discarded.
    dropped_local_path: bool,
    warnings: Vec<String>,
}

fn parse_curl_command(tokens: &[String]) -> CurlCommand {
    let mut command = CurlCommand::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut explicit_urls: Vec<String> = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        let token = &tokens[index];
        index += 1;

        if token == "--" {
            positionals.extend(tokens[index..].iter().cloned());
            break;
        }
        if !token.starts_with('-') || token == "-" {
            positionals.push(token.clone());
            continue;
        }

        let (flag, inline) = split_flag(token);
        let value = |index: &mut usize, warnings: &mut Vec<String>| {
            take_flag_value(tokens, index, inline.as_deref(), &flag, warnings)
        };

        match flag.as_str() {
            "-X" | "--request" => {
                if let Some(method) = value(&mut index, &mut command.warnings) {
                    command.method = Some(method.trim().to_ascii_uppercase());
                }
            }
            "-H" | "--header" => {
                if let Some(header) = value(&mut index, &mut command.warnings) {
                    match header.split_once(':') {
                        Some((key, header_value)) if !key.trim().is_empty() => {
                            command.headers.push(KeyValue {
                                key: key.trim().to_string(),
                                value: header_value.trim_start().to_string(),
                                enabled: true,
                                description: None,
                            });
                        }
                        _ => command
                            .warnings
                            .push(format!("curl: malformed header '{header}' ignored")),
                    }
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-ascii" | "--data-binary" => {
                if let Some(data) = value(&mut index, &mut command.warnings) {
                    if data.starts_with('@') || data.starts_with('<') {
                        command.warnings.push(format!(
                            "curl: {flag} file reference '{data}' is not imported"
                        ));
                    } else {
                        command.data.push(data);
                    }
                }
            }
            "--data-urlencode" => {
                if let Some(data) = value(&mut index, &mut command.warnings) {
                    match data_urlencode(&data) {
                        Some(encoded) => command.data.push(encoded),
                        None => command.warnings.push(format!(
                            "curl: {flag} file reference '{data}' is not imported"
                        )),
                    }
                }
            }
            "--json" => {
                if let Some(data) = value(&mut index, &mut command.warnings) {
                    if data.starts_with('@') || data.starts_with('<') {
                        command.warnings.push(format!(
                            "curl: {flag} file reference '{data}' is not imported"
                        ));
                    } else {
                        command.json_data.push(data);
                    }
                }
            }
            "-F" | "--form" => {
                if let Some(spec) = value(&mut index, &mut command.warnings)
                    && let Some(part) = form_part_from_spec(
                        &spec,
                        &mut command.warnings,
                        &mut command.dropped_local_path,
                    )
                {
                    command.forms.push(part);
                }
            }
            "--form-string" => {
                if let Some(spec) = value(&mut index, &mut command.warnings) {
                    match spec.split_once('=') {
                        Some((key, text)) if !key.trim().is_empty() => {
                            command.forms.push(FormPart {
                                key: key.trim().to_string(),
                                value: text.to_string(),
                                file_path: None,
                                enabled: true,
                            });
                        }
                        _ => command
                            .warnings
                            .push(format!("curl: malformed form field '{spec}' ignored")),
                    }
                }
            }
            "--digest" => command.digest = true,
            "-u" | "--user" => {
                if let Some(credentials) = value(&mut index, &mut command.warnings) {
                    let (username, password) = credentials
                        .split_once(':')
                        .map_or((credentials.as_str(), ""), |(user, pass)| (user, pass));
                    command.username = Some((username.to_string(), password.to_string()));
                }
            }
            "--url" => {
                if let Some(url) = value(&mut index, &mut command.warnings) {
                    explicit_urls.push(url);
                }
            }
            "-L" | "--location" => command.network.follow_redirects = Some(true),
            "--location-trusted" => {
                command.network.follow_redirects = Some(true);
                command.network.follow_auth_header = Some(true);
            }
            "--max-redirs" => {
                if let Some(raw) = value(&mut index, &mut command.warnings) {
                    match raw.trim().parse::<i64>() {
                        // curl's -1 means "no limit": the most SpanBay follows.
                        Ok(hops) if hops < 0 => {
                            command.network.max_redirects = Some(MAX_REDIRECT_HOPS);
                        }
                        Ok(hops) => {
                            command.network.max_redirects = Some(
                                u32::try_from(hops)
                                    .unwrap_or(MAX_REDIRECT_HOPS)
                                    .min(MAX_REDIRECT_HOPS),
                            );
                        }
                        Err(_) => command
                            .warnings
                            .push(format!("curl: --max-redirs '{raw}' is not a number; ignored")),
                    }
                }
            }
            "-k" | "--insecure" => command.network.verify_tls = Some(false),
            "-m" | "--max-time" => {
                if let Some(raw) = value(&mut index, &mut command.warnings) {
                    match seconds_to_ms(&raw) {
                        Some(ms) => command.network.total_timeout_ms = Some(ms),
                        None => command
                            .warnings
                            .push(format!("curl: {flag} '{raw}' is not a number of seconds; ignored")),
                    }
                }
            }
            "--retry" => {
                if let Some(raw) = value(&mut index, &mut command.warnings) {
                    match raw.trim().parse::<u32>() {
                        Ok(count) => {
                            command.network.retry_count =
                                Some(u8::try_from(count).unwrap_or(u8::MAX));
                        }
                        Err(_) => command
                            .warnings
                            .push(format!("curl: --retry '{raw}' is not a number; ignored")),
                    }
                }
            }
            "-x" | "--proxy" => {
                if let Some(proxy) = value(&mut index, &mut command.warnings) {
                    let proxy = proxy.trim();
                    if proxy.is_empty() {
                        command.network.proxy_url = None;
                    } else if proxy.contains("://") {
                        command.network.proxy_url = Some(proxy.to_owned());
                    } else {
                        // curl reads a bare host:port as an HTTP proxy.
                        command.network.proxy_url = Some(format!("http://{proxy}"));
                    }
                }
            }
            "-0" | "--http1.0" => command.network.http_version = Some(HttpVersionPreference::Http10),
            "--http1.1" => command.network.http_version = Some(HttpVersionPreference::Http11),
            "--http2" | "--http2-prior-knowledge" => {
                command.network.http_version = Some(HttpVersionPreference::Http2);
            }
            "-G" | "--get" => command.to_query = true,
            "-I" | "--head" => command.head = true,
            "-A" | "--user-agent" => {
                if let Some(agent) = value(&mut index, &mut command.warnings) {
                    set_header(&mut command.headers, "User-Agent", &agent);
                }
            }
            "-e" | "--referer" => {
                if let Some(referer) = value(&mut index, &mut command.warnings) {
                    set_header(&mut command.headers, "Referer", &referer);
                }
            }
            "-b" | "--cookie" => {
                if let Some(cookie) = value(&mut index, &mut command.warnings) {
                    set_header(&mut command.headers, "Cookie", &cookie);
                }
            }
            // Where the response goes, not what is sent.
            "-o" | "--output" | "-w" | "--write-out" | "--output-dir" | "-c" | "--cookie-jar" => {
                value(&mut index, &mut command.warnings);
            }
            "--cacert" | "--capath" | "--cert" | "--key" | "-E" | "--cert-type" | "--key-type"
            | "--pass" => {
                value(&mut index, &mut command.warnings);
                command.warnings.push(format!(
                    "curl: flag '{flag}' is not imported; add certificates in Settings › Certificates"
                ));
            }
            // A local file upload: the path never leaves the command.
            "-T" | "--upload-file" => {
                value(&mut index, &mut command.warnings);
                command
                    .warnings
                    .push(format!("curl: flag '{flag}' is not imported; ignored"));
            }
            "--connect-timeout" | "--resolve" | "--interface" | "--limit-rate" | "--connect-to"
            | "--ciphers" | "--dns-servers" | "--retry-delay" | "--retry-max-time"
            | "--proxy-user" | "-U" => {
                value(&mut index, &mut command.warnings);
                command
                    .warnings
                    .push(format!("curl: flag '{flag}' has no SpanBay setting; ignored"));
            }
            "--http3" | "--http3-only" => command.warnings.push(format!(
                "curl: flag '{flag}' has no SpanBay setting; the HTTP version is negotiated"
            )),
            "-s"
            | "--silent"
            | "-S"
            | "--show-error"
            | "-i"
            | "--include"
            | "-v"
            | "--verbose"
            // Responses are always decompressed, so `--compressed` needs nothing.
            | "--compressed"
            | "-f"
            | "--fail"
            | "--fail-with-body"
            | "-g"
            | "--globoff"
            | "-N"
            | "--no-buffer"
            | "--no-progress-meter"
            | "--path-as-is"
            | "-4"
            | "-6"
            | "--tcp-nodelay"
            | "-q"
            | "-j"
            | "--junk-session-cookies"
            | "--raw" => {}
            other => command
                .warnings
                .push(format!("curl: unsupported flag '{other}' ignored")),
        }
    }

    command.url = explicit_urls.into_iter().next().or_else(|| {
        positionals
            .iter()
            .find(|candidate| candidate.contains("://"))
            .cloned()
            .or_else(|| positionals.first().cloned())
    });

    command
}

/// curl's `--data-urlencode` forms: `content`, `=content` and `name=content` encode the content;
/// `@file` and `name@file` read a file and yield `None`. The first `=` or `@` decides.
fn data_urlencode(spec: &str) -> Option<String> {
    match spec.find(['=', '@']) {
        Some(at) if spec[at..].starts_with('@') => None,
        Some(0) => Some(spanbay_base::url_encode::encode_path_segment(&spec[1..])),
        Some(at) => Some(format!(
            "{}={}",
            &spec[..at],
            spanbay_base::url_encode::encode_path_segment(&spec[at + 1..])
        )),
        None => Some(spanbay_base::url_encode::encode_path_segment(spec)),
    }
}

fn split_flag(token: &str) -> (String, Option<String>) {
    if token.starts_with("--") {
        return match token.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (token.to_string(), None),
        };
    }
    if let (Some(flag), Some(rest)) = (token.get(..2), token.get(2..))
        && !rest.is_empty()
        && matches!(
            flag,
            "-H" | "-d"
                | "-X"
                | "-F"
                | "-u"
                | "-o"
                | "-A"
                | "-e"
                | "-b"
                | "-m"
                | "-x"
                | "-w"
                | "-c"
                | "-T"
                | "-E"
        )
    {
        return (flag.to_string(), Some(rest.to_string()));
    }
    (token.to_string(), None)
}

fn take_flag_value(
    tokens: &[String],
    index: &mut usize,
    inline: Option<&str>,
    flag: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if let Some(value) = inline {
        return Some(value.to_string());
    }
    if let Some(value) = tokens.get(*index) {
        *index += 1;
        return Some(value.clone());
    }
    warnings.push(format!("curl: flag '{flag}' expects a value; ignored"));
    None
}

fn form_part_from_spec(
    spec: &str,
    warnings: &mut Vec<String>,
    dropped_local_path: &mut bool,
) -> Option<FormPart> {
    let Some((key, rest)) = spec.split_once('=') else {
        warnings.push(format!("curl: malformed form field '{spec}' ignored"));
        return None;
    };
    let key = key.trim();
    if key.is_empty() {
        warnings.push(format!("curl: malformed form field '{spec}' ignored"));
        return None;
    }
    // `;type=...` and `;filename=...` parameters cannot be represented.
    let (value, parameters) = match rest.split_once(';') {
        Some((value, parameters)) => (value, Some(parameters)),
        None => (rest, None),
    };
    if parameters.is_some() {
        warnings.push(format!(
            "curl: form field '{key}' parameters (type/filename) are not imported"
        ));
    }
    if let Some(path) = value.strip_prefix('@') {
        if path.trim().is_empty() {
            warnings.push(format!(
                "curl: form field '{key}' has no file path; ignored"
            ));
            return None;
        }
        *dropped_local_path = true;
        return Some(FormPart {
            key: key.to_string(),
            value: String::new(),
            file_path: None,
            enabled: true,
        });
    }
    if value.starts_with('<') {
        *dropped_local_path = true;
        return Some(FormPart {
            key: key.to_string(),
            value: String::new(),
            file_path: None,
            enabled: true,
        });
    }
    Some(FormPart {
        key: key.to_string(),
        value: value.to_string(),
        file_path: None,
        enabled: true,
    })
}

pub(super) fn tokenize_curl(input: &str) -> Vec<String> {
    // Chrome's "Copy as cURL (cmd)" wraps every argument in `^"…^"`.
    let mut tokens = if input.contains("^\"") {
        tokenize_cmd(input)
    } else {
        tokenize_posix(input)
    };
    let prefix = tokens.iter().position(|token| {
        let token = token.trim();
        token.eq_ignore_ascii_case("curl")
            || token.eq_ignore_ascii_case("curl.exe")
            || token.eq_ignore_ascii_case("curl.cmd")
            || token.eq_ignore_ascii_case("curl.bat")
            || token.ends_with("/curl")
    });
    if let Some(prefix) = prefix {
        tokens.drain(..=prefix);
    }
    tokens
}

/// Windows `cmd` then C-runtime splitting, inverting Chrome's `escapeStringWin`: outside a
/// `"…"` region `^` keeps the next character (after a line break, the character after it),
/// then words split on blanks outside `"…"`, where `\"` and `\\` stand for `"` and `\`.
fn tokenize_cmd(input: &str) -> Vec<String> {
    let mut stream = String::with_capacity(input.len());
    let mut chars = input.chars();
    let mut quoted = false;
    while let Some(character) = chars.next() {
        match character {
            '^' if !quoted => match chars.next() {
                Some('\r') => {
                    let mut ahead = chars.clone();
                    if ahead.next() == Some('\n') {
                        chars = ahead;
                    }
                    stream.extend(chars.next());
                }
                Some('\n') => stream.extend(chars.next()),
                Some(next) => stream.push(next),
                None => {}
            },
            '"' => {
                quoted = !quoted;
                stream.push('"');
            }
            other => stream.push(other),
        }
    }
    let mut tokens = Vec::new();
    let mut current = String::new();
    let (mut in_token, mut quoted) = (false, false);
    let mut chars = stream.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' if matches!(chars.peek(), Some('"' | '\\')) => {
                current.extend(chars.next());
                in_token = true;
            }
            '"' => {
                quoted = !quoted;
                in_token = true;
            }
            blank if blank.is_whitespace() && !quoted => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            other => {
                current.push(other);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    tokens
}

/// Bash `$'…'` (ANSI-C quoting); `chars` is just past the opening quote.
fn push_ansi_c(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
    fn digits(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        radix: u32,
        max: usize,
        first: Option<u32>,
    ) -> Option<u32> {
        let mut value = first;
        for _ in usize::from(first.is_some())..max {
            let Some(digit) = chars.peek().and_then(|c| c.to_digit(radix)) else {
                break;
            };
            chars.next();
            value = Some(value.unwrap_or(0) * radix + digit);
        }
        value
    }
    let mut bytes = Vec::new();
    let mut utf8 = [0_u8; 4];
    let mut push = |bytes: &mut Vec<u8>, character: char| {
        bytes.extend_from_slice(character.encode_utf8(&mut utf8).as_bytes());
    };
    while let Some(character) = chars.next() {
        if character == '\'' {
            break;
        }
        if character != '\\' {
            push(&mut bytes, character);
            continue;
        }
        let Some(escaped) = chars.next() else {
            bytes.push(b'\\');
            break;
        };
        let byte = match escaped {
            'a' => 0x07,
            'b' => 0x08,
            'e' | 'E' => 0x1b,
            'f' => 0x0c,
            'n' => b'\n',
            'r' => b'\r',
            't' => b'\t',
            'v' => 0x0b,
            '\\' | '\'' | '"' | '?' => escaped as u8,
            'c' => match chars.next() {
                Some(control) => (control as u8) & 0x1f,
                None => break,
            },
            '0'..='7' => {
                let value = digits(chars, 8, 3, escaped.to_digit(8)).unwrap_or(0);
                (value & 0xff) as u8
            }
            'x' => match digits(chars, 16, 2, None) {
                Some(value) => value as u8,
                None => {
                    bytes.extend_from_slice(b"\\x");
                    continue;
                }
            },
            'u' | 'U' => {
                let max = if escaped == 'u' { 4 } else { 8 };
                match digits(chars, 16, max, None) {
                    Some(value) => push(&mut bytes, char::from_u32(value).unwrap_or('\u{fffd}')),
                    None => {
                        bytes.push(b'\\');
                        push(&mut bytes, escaped);
                    }
                }
                continue;
            }
            other => {
                bytes.push(b'\\');
                push(&mut bytes, other);
                continue;
            }
        };
        bytes.push(byte);
    }
    out.push_str(&String::from_utf8_lossy(&bytes));
}

/// POSIX shell words: `'…'`, `"…"`, `$'…'`, backslash escapes and `^` line continuations.
fn tokenize_posix(input: &str) -> Vec<String> {
    let mut chars = input.chars().peekable();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    while let Some(character) = chars.next() {
        // Windows `cmd` line continuations only apply outside quoted arguments.
        if character == '^' {
            let mut ahead = chars.clone();
            if ahead.next() == Some('\n')
                || (chars.peek() == Some(&'\r') && ahead.next() == Some('\n'))
            {
                chars = ahead;
                continue;
            }
        }
        match character {
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                in_token = true;
                push_ansi_c(&mut chars, &mut current);
            }
            '\'' => {
                in_token = true;
                for character in chars.by_ref() {
                    if character == '\'' {
                        break;
                    }
                    current.push(character);
                }
            }
            '"' => {
                in_token = true;
                while let Some(character) = chars.next() {
                    if character == '"' {
                        break;
                    }
                    if character != '\\' {
                        current.push(character);
                        continue;
                    }
                    match chars.next() {
                        Some('\n') => {}
                        Some('\r') => {
                            if chars.peek() == Some(&'\n') {
                                chars.next();
                            }
                        }
                        Some(escaped @ ('"' | '\\' | '$' | '`')) => current.push(escaped),
                        Some(escaped) => {
                            current.push('\\');
                            current.push(escaped);
                        }
                        None => current.push('\\'),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some('\r') => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(next) => {
                    current.push(next);
                    in_token = true;
                }
            },
            character if character.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            character => {
                current.push(character);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    tokens
}
