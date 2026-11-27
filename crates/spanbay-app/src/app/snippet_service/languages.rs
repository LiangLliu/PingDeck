//! Per-language rendering of a [`Prepared`] request: string quoting plus one emitter per client.

use super::{Body, PROVIDER_KEY_VARIABLE, Part, Prepared, SnippetLanguage};

/// Renders `prepared` as `language`.
pub(super) fn render(prepared: &Prepared, language: SnippetLanguage) -> String {
    match language {
        SnippetLanguage::Curl => curl(prepared),
        SnippetLanguage::Fetch => fetch(prepared),
        SnippetLanguage::Axios => axios(prepared),
        SnippetLanguage::Python => python(prepared),
        SnippetLanguage::Go => go(prepared),
        SnippetLanguage::Java => java(prepared),
        SnippetLanguage::Csharp => csharp(prepared),
        SnippetLanguage::Ruby => ruby(prepared),
        SnippetLanguage::Php => php(prepared),
    }
}

/// `JSON.stringify` of a string: also a valid string literal in JS, Python, Go, Java and C#.
pub(super) fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            control if control < ' ' => output.push_str(&format!("\\u{:04x}", u32::from(control))),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

/// A POSIX single-quoted shell word.
fn shell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A shell double-quoted word, so `$PROVIDER_API_KEY` expands. Only the generated
/// credential header of a provider-target snippet uses it; nothing else may interpolate.
fn shell_expanding(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A single-quoted shell word in which each of `vars` (`$TOKEN`) stands outside the quotes, so
/// the shell expands those and nothing else: `'Authorization: Bearer '"$TOKEN"`.
fn shell_with_vars(value: &str, vars: &[String]) -> String {
    let next = |text: &str| {
        vars.iter()
            .filter_map(|var| {
                text.match_indices(var.as_str())
                    .find(|(at, _)| {
                        !text[at + var.len()..]
                            .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                    })
                    .map(|(at, _)| (at, var.len()))
            })
            .min()
    };
    let mut out = String::new();
    let mut rest = value;
    while let Some((at, len)) = next(rest) {
        if at > 0 {
            out.push_str(&shell(&rest[..at]));
        }
        out.push_str(&format!("\"{}\"", &rest[at..at + len]));
        rest = &rest[at + len..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push_str(&shell(rest));
    }
    out
}

/// A Ruby double-quoted literal: the JSON escapes plus the `#{}` / `#$` / `#@` interpolations.
fn ruby_string(value: &str) -> String {
    json_string(value)
        .replace("#{", "\\#{")
        .replace("#$", "\\#$")
        .replace("#@", "\\#@")
}

/// A PHP double-quoted literal: `$` would interpolate and `\b`/`\uXXXX` are not PHP escapes.
fn php_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '$' => output.push_str("\\$"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{c}' => output.push_str("\\f"),
            control if control < ' ' => output.push_str(&format!("\\x{:02x}", u32::from(control))),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

/// Headers as `Object.fromEntries` would keep them: a repeated name keeps its first position
/// and its last value. shortcut: JS objects and Python dicts cannot send a header twice.
fn unique_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    let mut unique: Vec<(String, String)> = Vec::with_capacity(headers.len());
    for (key, value) in headers {
        match unique.iter_mut().find(|(existing, _)| existing == key) {
            Some(entry) => entry.1.clone_from(value),
            None => unique.push((key.clone(), value.clone())),
        }
    }
    unique
}

/// `JSON.stringify(Object.fromEntries(headers), null, indent)` for a non-empty header list.
fn json_headers(headers: &[(String, String)], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let rows = unique_headers(headers)
        .iter()
        .map(|(key, value)| format!("{pad}{}: {}", json_string(key), json_string(value)))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("{{\n{rows}\n}}")
}

/// Whether the body reads a local file.
fn reads_file(body: &Body) -> bool {
    match body {
        Body::File(_) => true,
        Body::Multipart(parts) => parts.iter().any(|part| matches!(part, Part::File { .. })),
        Body::None | Body::Text(_) => false,
    }
}

/// `Get`, `Post`, … as C# (`HttpMethod.Get`) and Ruby (`Net::HTTP::Get`) spell them.
fn title_case_method(method: &str) -> String {
    let lower = method.to_ascii_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

fn curl(p: &Prepared) -> String {
    let flags: String = p.curl_flags.iter().map(|flag| format!(" {flag}")).collect();
    let mut lines = vec![format!(
        "curl{flags} -X {} {}",
        p.method,
        shell_with_vars(&p.url, &p.shell_vars)
    )];
    lines.extend(p.curl_options.iter().map(|option| format!("  {option}")));
    if let Some((user, password)) = &p.digest {
        lines.push(format!(
            "  --digest -u {}",
            shell(&format!("{user}:{password}"))
        ));
    }
    for (key, value) in &p.headers {
        let header = format!("{key}: {value}");
        // The provider key is an environment variable in a deliberate export, so its
        // header stays double-quoted; every other value keeps the literal single quotes.
        let header = if value.contains(PROVIDER_KEY_VARIABLE) {
            shell_expanding(&header)
        } else {
            shell_with_vars(&header, &p.shell_vars)
        };
        lines.push(format!("  -H {header}"));
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => lines.push(format!("  --data {}", shell(text))),
        Body::File(path) => lines.push(format!("  --data-binary {}", shell(&format!("@{path}")))),
        Body::Multipart(parts) => {
            for part in parts {
                lines.push(match part {
                    // `--form-string` never reads a value starting with `@` or `<` as a file.
                    Part::Text { key, value } => {
                        format!("  --form-string {}", shell(&format!("{key}={value}")))
                    }
                    Part::File { key, path } => {
                        let file = if path.contains([';', ',', '"']) {
                            format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
                        } else {
                            path.clone()
                        };
                        format!("  -F {}", shell(&format!("{key}=@{file}")))
                    }
                });
            }
        }
    }
    lines.join(" \\\n")
}

/// The statements that build the multipart `FormData` shared by fetch and axios.
fn form_data_lines(parts: &[Part]) -> Vec<String> {
    let mut lines = vec!["const form = new FormData();".to_owned()];
    for part in parts {
        lines.push(match part {
            Part::Text { key, value } => {
                format!("form.append({}, {});", json_string(key), json_string(value))
            }
            Part::File { key, path } => format!(
                "form.append({}, new Blob([await readFile({})]), {});",
                json_string(key),
                json_string(path),
                json_string(Part::file_name(path))
            ),
        });
    }
    lines
}

const READ_FILE_IMPORT: &str = "import { readFile } from \"node:fs/promises\";";

fn fetch(p: &Prepared) -> String {
    let mut lines = Vec::new();
    if reads_file(&p.body) {
        lines.push(READ_FILE_IMPORT.to_owned());
        lines.push(String::new());
    }
    if let Body::Multipart(parts) = &p.body {
        lines.extend(form_data_lines(parts));
    }
    let mut options = vec![format!("  method: {},", json_string(&p.method))];
    if !p.headers.is_empty() {
        options.push(format!(
            "  headers: {},",
            json_headers(&p.headers, 2).replace('\n', "\n  ")
        ));
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => options.push(format!("  body: {},", json_string(text))),
        Body::File(path) => options.push(format!("  body: await readFile({}),", json_string(path))),
        Body::Multipart(_) => options.push("  body: form,".to_owned()),
    }
    lines.push(format!(
        "const response = await fetch({}, {{\n{}\n}});",
        json_string(&p.url),
        options.join("\n")
    ));
    lines.push("const data = await response.text();".to_owned());
    lines.join("\n")
}

fn axios(p: &Prepared) -> String {
    let mut lines = vec!["import axios from \"axios\";".to_owned()];
    if reads_file(&p.body) {
        lines.push(READ_FILE_IMPORT.to_owned());
    }
    lines.push(String::new());
    if let Body::Multipart(parts) = &p.body {
        lines.extend(form_data_lines(parts));
    }
    lines.push("const response = await axios.request({".to_owned());
    lines.push(format!("  method: {},", json_string(&p.method)));
    lines.push(format!("  url: {},", json_string(&p.url)));
    if !p.headers.is_empty() {
        lines.push(format!("  headers: {},", json_headers(&p.headers, 2)));
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => lines.push(format!("  data: {},", json_string(text))),
        Body::File(path) => lines.push(format!("  data: await readFile({}),", json_string(path))),
        Body::Multipart(_) => lines.push("  data: form,".to_owned()),
    }
    lines.push("});".to_owned());
    lines.push("console.log(response.status, response.data);".to_owned());
    lines.join("\n")
}

fn python(p: &Prepared) -> String {
    let mut lines = vec!["import requests".to_owned(), String::new()];
    if !p.headers.is_empty() {
        lines.push(format!("headers = {}", json_headers(&p.headers, 4)));
    }
    let mut args = vec![json_string(&p.method), json_string(&p.url)];
    if !p.headers.is_empty() {
        args.push("headers=headers".to_owned());
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => args.push(format!("data={}", json_string(text))),
        Body::File(path) => args.push(format!("data=open({}, \"rb\")", json_string(path))),
        Body::Multipart(parts) => {
            lines.push("files = [".to_owned());
            for part in parts {
                lines.push(match part {
                    Part::Text { key, value } => {
                        format!(
                            "    ({}, (None, {})),",
                            json_string(key),
                            json_string(value)
                        )
                    }
                    Part::File { key, path } => format!(
                        "    ({}, ({}, open({}, \"rb\"))),",
                        json_string(key),
                        json_string(Part::file_name(path)),
                        json_string(path)
                    ),
                });
            }
            lines.push("]".to_owned());
            args.push("files=files".to_owned());
        }
    }
    lines.push(format!("response = requests.request({})", args.join(", ")));
    lines.push("print(response.status_code, response.text)".to_owned());
    lines.join("\n")
}

fn go(p: &Prepared) -> String {
    let mut imports = vec!["fmt", "io", "net/http"];
    match &p.body {
        Body::None => {}
        Body::Text(_) => imports.push("strings"),
        Body::File(_) => imports.push("os"),
        Body::Multipart(_) => imports.extend(["bytes", "mime/multipart", "os"]),
    }
    imports.sort_unstable();
    let mut lines = vec![
        "package main".to_owned(),
        String::new(),
        "import (".to_owned(),
    ];
    lines.extend(imports.iter().map(|path| format!("  \"{path}\"")));
    lines.extend([")".to_owned(), String::new(), "func main() {".to_owned()]);

    let method = json_string(&p.method);
    let url = json_string(&p.url);
    let mut content_type = None;
    let request = match &p.body {
        Body::None => format!("  req, err := http.NewRequest({method}, {url}, nil)"),
        Body::Text(text) => {
            lines.push(format!(
                "  body := strings.NewReader({})",
                json_string(text)
            ));
            format!("  req, err := http.NewRequest({method}, {url}, body)")
        }
        Body::File(path) => {
            lines.extend([
                format!("  body, err := os.Open({})", json_string(path)),
                "  if err != nil { panic(err) }".to_owned(),
                "  defer body.Close()".to_owned(),
            ]);
            format!("  req, err := http.NewRequest({method}, {url}, body)")
        }
        Body::Multipart(parts) => {
            lines.extend([
                "  body := &bytes.Buffer{}".to_owned(),
                "  writer := multipart.NewWriter(body)".to_owned(),
            ]);
            for part in parts {
                match part {
                    Part::Text { key, value } => lines.push(format!(
                        "  writer.WriteField({}, {})",
                        json_string(key),
                        json_string(value)
                    )),
                    Part::File { key, path } => lines.extend([
                        "  {".to_owned(),
                        format!("    file, err := os.Open({})", json_string(path)),
                        "    if err != nil { panic(err) }".to_owned(),
                        "    defer file.Close()".to_owned(),
                        format!(
                            "    part, err := writer.CreateFormFile({}, {})",
                            json_string(key),
                            json_string(Part::file_name(path))
                        ),
                        "    if err != nil { panic(err) }".to_owned(),
                        "    io.Copy(part, file)".to_owned(),
                        "  }".to_owned(),
                    ]),
                }
            }
            lines.push("  writer.Close()".to_owned());
            content_type = Some("  req.Header.Set(\"Content-Type\", writer.FormDataContentType())");
            format!("  req, err := http.NewRequest({method}, {url}, body)")
        }
    };
    lines.push(request);
    lines.push("  if err != nil { panic(err) }".to_owned());
    for (key, value) in &p.headers {
        lines.push(format!(
            "  req.Header.Set({}, {})",
            json_string(key),
            json_string(value)
        ));
    }
    lines.extend(content_type.map(str::to_owned));
    lines.extend(
        [
            "  res, err := http.DefaultClient.Do(req)",
            "  if err != nil { panic(err) }",
            "  defer res.Body.Close()",
            "  data, _ := io.ReadAll(res.Body)",
            "  fmt.Println(res.Status, string(data))",
            "}",
        ]
        .map(str::to_owned),
    );
    lines.join("\n")
}

fn java(p: &Prepared) -> String {
    let mut lines = [
        "import java.net.URI;",
        "import java.net.http.HttpClient;",
        "import java.net.http.HttpRequest;",
        "import java.net.http.HttpResponse;",
        "",
    ]
    .map(str::to_owned)
    .to_vec();
    let mut multipart = false;
    let publisher = match &p.body {
        Body::None => "HttpRequest.BodyPublishers.noBody()".to_owned(),
        Body::Text(text) => format!("HttpRequest.BodyPublishers.ofString({})", json_string(text)),
        Body::File(path) => format!(
            "HttpRequest.BodyPublishers.ofFile(java.nio.file.Path.of({}))",
            json_string(path)
        ),
        Body::Multipart(parts) => {
            // The JDK client has no multipart support, so the body is assembled by hand.
            lines.extend([
                "var boundary = \"SpanBayBoundary\";".to_owned(),
                "var parts = new java.util.ArrayList<byte[]>();".to_owned(),
            ]);
            let utf8 = "java.nio.charset.StandardCharsets.UTF_8";
            for part in parts {
                match part {
                    Part::Text { key, value } => {
                        let text = format!(
                            "\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n",
                            key.replace('"', "%22")
                        );
                        lines.push(format!(
                            "parts.add((\"--\" + boundary + {}).getBytes({utf8}));",
                            json_string(&text)
                        ));
                    }
                    Part::File { key, path } => {
                        let head = format!(
                            "\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
                            key.replace('"', "%22"),
                            Part::file_name(path).replace('"', "%22")
                        );
                        lines.push(format!(
                            "parts.add((\"--\" + boundary + {}).getBytes({utf8}));",
                            json_string(&head)
                        ));
                        lines.push(format!(
                            "parts.add(java.nio.file.Files.readAllBytes(java.nio.file.Path.of({})));",
                            json_string(path)
                        ));
                        lines.push(format!("parts.add(\"\\r\\n\".getBytes({utf8}));"));
                    }
                }
            }
            lines.push(format!(
                "parts.add((\"--\" + boundary + \"--\\r\\n\").getBytes({utf8}));"
            ));
            multipart = true;
            "HttpRequest.BodyPublishers.ofByteArrays(parts)".to_owned()
        }
    };
    lines.extend([
        "var client = HttpClient.newHttpClient();".to_owned(),
        "var request = HttpRequest.newBuilder()".to_owned(),
        format!("    .uri(URI.create({}))", json_string(&p.url)),
    ]);
    for (key, value) in &p.headers {
        lines.push(format!(
            "    .header({}, {})",
            json_string(key),
            json_string(value)
        ));
    }
    if multipart {
        lines.push(
            "    .header(\"Content-Type\", \"multipart/form-data; boundary=\" + boundary)"
                .to_owned(),
        );
    }
    lines.push(format!(
        "    .method({}, {publisher})",
        json_string(&p.method)
    ));
    lines.extend(
        [
            "    .build();",
            "var response = client.send(request, HttpResponse.BodyHandlers.ofString());",
            "System.out.println(response.statusCode() + \" \" + response.body());",
        ]
        .map(str::to_owned),
    );
    lines.join("\n")
}

fn csharp(p: &Prepared) -> String {
    let method = title_case_method(&p.method);
    let mut lines = [
        "using System.Net.Http;",
        "using System.Text;",
        "",
        "using var client = new HttpClient();",
    ]
    .map(str::to_owned)
    .to_vec();
    lines.push(format!(
        "using var request = new HttpRequestMessage(HttpMethod.{method}, {});",
        json_string(&p.url)
    ));
    // `Content-Type` is a content header: `TryAddWithoutValidation` on the message drops it.
    for (key, value) in p
        .headers
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("content-type"))
    {
        lines.push(format!(
            "request.Headers.TryAddWithoutValidation({}, {});",
            json_string(key),
            json_string(value)
        ));
    }
    let content_type = p.content_type().unwrap_or("text/plain");
    match &p.body {
        Body::None => {}
        Body::Text(text) => {
            let media_type = content_type.split(';').next().unwrap_or_default().trim();
            lines.push(format!(
                "request.Content = new StringContent({}, Encoding.UTF8, {});",
                json_string(text),
                json_string(media_type)
            ));
        }
        Body::File(path) => {
            lines.push(format!(
                "request.Content = new ByteArrayContent(System.IO.File.ReadAllBytes({}));",
                json_string(path)
            ));
            lines.push(format!(
                "request.Content.Headers.TryAddWithoutValidation(\"Content-Type\", {});",
                json_string(content_type)
            ));
        }
        Body::Multipart(parts) => {
            lines.push("using var content = new MultipartFormDataContent();".to_owned());
            for part in parts {
                lines.push(match part {
                    Part::Text { key, value } => format!(
                        "content.Add(new StringContent({}), {});",
                        json_string(value),
                        json_string(key)
                    ),
                    Part::File { key, path } => format!(
                        "content.Add(new ByteArrayContent(System.IO.File.ReadAllBytes({})), {}, {});",
                        json_string(path),
                        json_string(key),
                        json_string(Part::file_name(path))
                    ),
                });
            }
            lines.push("request.Content = content;".to_owned());
        }
    }
    lines.push("using var response = await client.SendAsync(request);".to_owned());
    lines.push(
        "Console.WriteLine((int)response.StatusCode + \" \" + await response.Content.ReadAsStringAsync());"
            .to_owned(),
    );
    lines.join("\n")
}

fn ruby(p: &Prepared) -> String {
    let mut lines = vec![
        "require \"net/http\"".to_owned(),
        "require \"uri\"".to_owned(),
        String::new(),
        format!("uri = URI({})", ruby_string(&p.url)),
        format!(
            "request = Net::HTTP::{}.new(uri)",
            title_case_method(&p.method)
        ),
    ];
    for (key, value) in &p.headers {
        lines.push(format!(
            "request[{}] = {}",
            ruby_string(key),
            ruby_string(value)
        ));
    }
    match &p.body {
        Body::None => {}
        Body::Text(text) => lines.push(format!("request.body = {}", ruby_string(text))),
        Body::File(path) => lines.push(format!(
            "request.body = File.binread({})",
            ruby_string(path)
        )),
        Body::Multipart(parts) => {
            let fields = parts
                .iter()
                .map(|part| match part {
                    Part::Text { key, value } => {
                        format!("[{}, {}]", ruby_string(key), ruby_string(value))
                    }
                    Part::File { key, path } => {
                        format!("[{}, File.open({})]", ruby_string(key), ruby_string(path))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "request.set_form([{fields}], \"multipart/form-data\")"
            ));
        }
    }
    lines.push(
        "response = Net::HTTP.start(uri.hostname, uri.port, use_ssl: uri.scheme == \"https\") { |http| http.request(request) }"
            .to_owned(),
    );
    lines.push("puts response.code, response.body".to_owned());
    lines.join("\n")
}

fn php(p: &Prepared) -> String {
    let mut lines = vec![
        "<?php".to_owned(),
        "$ch = curl_init();".to_owned(),
        format!("curl_setopt($ch, CURLOPT_URL, {});", php_string(&p.url)),
    ];
    if !p.headers.is_empty() {
        let rows = p
            .headers
            .iter()
            .map(|(key, value)| format!("  {}", php_string(&format!("{key}: {value}"))))
            .collect::<Vec<_>>()
            .join(",\n");
        lines.push(format!(
            "curl_setopt($ch, CURLOPT_HTTPHEADER, [\n{rows}\n]);"
        ));
    }
    lines.push(format!(
        "curl_setopt($ch, CURLOPT_CUSTOMREQUEST, {});",
        php_string(&p.method)
    ));
    match &p.body {
        Body::None => {}
        Body::Text(text) => lines.push(format!(
            "curl_setopt($ch, CURLOPT_POSTFIELDS, {});",
            php_string(text)
        )),
        Body::File(path) => lines.push(format!(
            "curl_setopt($ch, CURLOPT_POSTFIELDS, file_get_contents({}));",
            php_string(path)
        )),
        Body::Multipart(parts) => {
            let rows = parts
                .iter()
                .map(|part| match part {
                    Part::Text { key, value } => {
                        format!("  {} => {}", php_string(key), php_string(value))
                    }
                    Part::File { key, path } => format!(
                        "  {} => new CURLFile({}, \"\", {})",
                        php_string(key),
                        php_string(path),
                        php_string(Part::file_name(path))
                    ),
                })
                .collect::<Vec<_>>()
                .join(",\n");
            lines.push(format!(
                "curl_setopt($ch, CURLOPT_POSTFIELDS, [\n{rows}\n]);"
            ));
        }
    }
    lines.extend(
        [
            "curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);",
            "$body = curl_exec($ch);",
            "echo curl_getinfo($ch, CURLINFO_HTTP_CODE), \" \", $body;",
            "curl_close($ch);",
        ]
        .map(str::to_owned),
    );
    lines.join("\n")
}
