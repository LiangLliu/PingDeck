//! Regenerates `fixtures/models-dev-snapshot.json` from a downloaded `models.dev/api.json`.
//! Pure Rust replacement for `scripts/models-snapshot.mjs`, aligned with `pingdeck-llm-domain`.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::{err_line, out_line};

const USAGE: &str = "usage: cargo xtask models-snapshot <api.json> [out.json] [--date YYYY-MM-DD]";
const SOURCE_URL: &str = "https://models.dev/api.json";
const SIZE_LIMIT_BYTES: usize = 400_000;

const FAMILIES: &[&str] = &[
    "openai",
    "anthropic",
    "google",
    "deepseek",
    "alibaba",
    "alibaba-cn",
    "zhipuai",
    "zai",
    "moonshotai",
    "moonshotai-cn",
    "xai",
    "mistral",
    "minimax",
    "minimax-cn",
    "volcengine",
    "cohere",
    "perplexity",
    "groq",
    "siliconflow-cn",
    "siliconflow",
    "togetherai",
    "fireworks-ai",
    "nvidia",
    "openrouter",
];

const INPUTS: &[&str] = &["text", "image", "audio", "video", "pdf"];

#[derive(Serialize)]
struct SnapshotMeta<'a> {
    generated_at: &'a str,
    source: &'a str,
}

#[derive(Serialize)]
struct Modalities {
    input: Vec<&'static str>,
}

#[derive(Serialize)]
struct Limit {
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
}

#[derive(Serialize)]
struct Cost {
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_write: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Value>,
}

#[derive(Serialize)]
struct TrimmedModel<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_date: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modalities: Option<Modalities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<Limit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<Cost>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical_model_id: Option<&'a str>,
}

fn finite_num(v: Option<&Value>) -> Option<Value> {
    let val = v?;
    let n = val.as_f64()?;
    if n.is_finite() {
        Some(val.clone())
    } else {
        None
    }
}

fn trim_model<'a>(model: &'a Value, release_re: &Regex) -> Option<TrimmedModel<'a>> {
    let mtype = model.get("type").and_then(Value::as_str);
    if !matches!(mtype, None | Some("chat") | Some("embedding")) {
        return None;
    }
    let cost = model.get("cost").and_then(|c| {
        let out = Cost {
            input: finite_num(c.get("input")),
            output: finite_num(c.get("output")),
            cache_read: finite_num(c.get("cache_read")),
            cache_write: finite_num(c.get("cache_write")),
            reasoning: finite_num(c.get("reasoning")),
        };
        if out.input.is_some()
            || out.output.is_some()
            || out.cache_read.is_some()
            || out.cache_write.is_some()
            || out.reasoning.is_some()
        {
            Some(out)
        } else {
            None
        }
    });
    let limit = model.get("limit").and_then(|l| {
        let out = Limit {
            context: finite_num(l.get("context")),
            output: finite_num(l.get("output")),
        };
        if out.context.is_some() || out.output.is_some() {
            Some(out)
        } else {
            None
        }
    });
    let inputs: Vec<&'static str> = model
        .get("modalities")
        .and_then(|m| m.get("input"))
        .and_then(Value::as_array)
        .map(|arr| {
            INPUTS
                .iter()
                .copied()
                .filter(|&m| arr.iter().any(|v| v.as_str() == Some(m)))
                .collect()
        })
        .unwrap_or_default();

    Some(TrimmedModel {
        name: model.get("name").and_then(Value::as_str),
        release_date: model
            .get("release_date")
            .and_then(Value::as_str)
            .filter(|s| release_re.is_match(s)),
        reasoning: model
            .get("reasoning")
            .and_then(Value::as_bool)
            .filter(|&b| b),
        tool_call: model
            .get("tool_call")
            .and_then(Value::as_bool)
            .filter(|&b| b),
        modalities: (!inputs.is_empty()).then_some(Modalities { input: inputs }),
        limit,
        cost,
        kind: (mtype == Some("embedding")).then_some("embedding"),
        canonical_model_id: model.get("canonical_model_id").and_then(Value::as_str),
    })
}

pub fn build_snapshot(
    api: &Value,
    date: &str,
    input_label: &str,
    families: &[&str],
) -> Result<(String, Vec<String>), String> {
    let release_re =
        Regex::new(r"^\d{4}-\d{2}(-\d{2})?$").map_err(|e| format!("regex error: {e}"))?;
    let meta = serde_json::to_string(&SnapshotMeta {
        generated_at: date,
        source: SOURCE_URL,
    })
    .map_err(|e| e.to_string())?;
    let mut lines = vec![format!("\"_snapshot\":{meta}")];
    let mut counts = Vec::new();

    for &family in families {
        let Some(provider) = api.get(family) else {
            return Err(format!("family {family} is not in {input_label}"));
        };
        let Some(models_obj) = provider.get("models").and_then(Value::as_object) else {
            return Err(format!("family {family} has no models in {input_label}"));
        };
        let mut models: Vec<(&String, TrimmedModel<'_>)> = models_obj
            .iter()
            .filter_map(|(id, m)| trim_model(m, &release_re).map(|tm| (id, tm)))
            .collect();
        models.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
        let mut rows = Vec::with_capacity(models.len());
        for (id, tm) in &models {
            let k = serde_json::to_string(id).map_err(|e| e.to_string())?;
            let v = serde_json::to_string(tm).map_err(|e| e.to_string())?;
            rows.push(format!("{k}:{v}"));
        }
        let fam_json = serde_json::to_string(family).map_err(|e| e.to_string())?;
        let name_json = serde_json::to_string(provider.get("name").unwrap_or(&Value::Null))
            .map_err(|e| e.to_string())?;
        lines.push(format!(
            "{fam_json}:{{\"name\":{name_json},\"models\":{{\n{}\n}}}}",
            rows.join(",\n")
        ));
        counts.push(format!("{family} {}", models.len()));
    }
    let text = format!("{{\n{}\n}}\n", lines.join(",\n"));
    let _: Value =
        serde_json::from_str(&text).map_err(|e| format!("assembled invalid JSON: {e}"))?;
    Ok((text, counts))
}

fn today_utc() -> String {
    // Simple UTC YYYY-MM-DD using unix timestamp (avoids pulling chrono into xtask).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    // Howard Hinnant civil_from_days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    format!("{year:04}-{m:02}-{d:02}")
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    let mut date: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--date" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    err_line(&format!("missing value for --date\n{USAGE}"));
                    return ExitCode::from(2);
                };
                date = Some(val.clone());
            }
            a if a.starts_with("--date=") => {
                date = Some(a["--date=".len()..].to_owned());
            }
            a if a.starts_with('-') => {
                err_line(&format!("unexpected flag '{a}'\n{USAGE}"));
                return ExitCode::from(2);
            }
            p => positionals.push(p.to_owned()),
        }
        i += 1;
    }
    if positionals.len() > 2 {
        err_line(&format!(
            "unexpected argument '{}'\n{USAGE}",
            positionals[2]
        ));
        return ExitCode::from(2);
    }

    let date = date.unwrap_or_else(today_utc);
    let Ok(date_re) = Regex::new(r"^\d{4}-\d{2}-\d{2}$") else {
        return ExitCode::FAILURE;
    };
    if !date_re.is_match(&date) {
        err_line(&format!("--date must be YYYY-MM-DD, got {date}"));
        return ExitCode::FAILURE;
    }
    let Some(input) = positionals.first() else {
        err_line(USAGE);
        return ExitCode::from(2);
    };
    let out = positionals.get(1).map_or_else(
        || root.join("fixtures/models-dev-snapshot.json"),
        |p| Path::new(p).to_path_buf(),
    );
    let raw = match fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            err_line(&format!("cannot read {input}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let api: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            err_line(&format!("invalid JSON in {input}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let (text, counts) = match build_snapshot(&api, &date, input, FAMILIES) {
        Ok(v) => v,
        Err(e) => {
            err_line(&e);
            return ExitCode::FAILURE;
        }
    };
    if text.len() > SIZE_LIMIT_BYTES {
        err_line(&format!(
            "snapshot is {} bytes, over the {SIZE_LIMIT_BYTES} byte budget: drop a family",
            text.len()
        ));
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::write(&out, &text) {
        err_line(&format!("cannot write {}: {e}", out.display()));
        return ExitCode::FAILURE;
    }
    out_line(&format!(
        "{}: {} bytes, {}",
        out.display(),
        text.len(),
        counts.join(", ")
    ));
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trims_and_filters_models_deterministically() {
        let api = json!({
            "openai": {
                "name": "OpenAI",
                "models": {
                    "b-model": {
                        "name": "B",
                        "release_date": "2026-05-01",
                        "reasoning": true,
                        "tool_call": false,
                        "modalities": { "input": ["pdf", "text", "unknown"] },
                        "limit": { "context": 128000 },
                        "cost": { "input": 2.5, "output": 10 }
                    },
                    "a-embed": { "name": "Emb", "type": "embedding" },
                    "skip-img": { "name": "Img", "type": "image" }
                }
            }
        });
        let (text, counts) = build_snapshot(&api, "2026-11-24", "mem", &["openai"]).unwrap();
        assert_eq!(counts, vec!["openai 2"]);
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["_snapshot"]["generated_at"], "2026-11-24");
        assert!(parsed["openai"]["models"]["skip-img"].is_null());
        assert_eq!(parsed["openai"]["models"]["a-embed"]["type"], "embedding");
        assert_eq!(
            parsed["openai"]["models"]["b-model"]["modalities"]["input"],
            json!(["text", "pdf"])
        );
        assert!(parsed["openai"]["models"]["b-model"]["tool_call"].is_null());
    }
}
