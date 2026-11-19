//! Local-runtime fingerprints: which of Ollama, LM Studio, llama.cpp, vLLM, SGLang and LocalAI
//! listens on a loopback or private host, decided in a fixed order from the answers of cheap
//! GETs. Pure; the engine sends the requests (uncounted) and fills [`LocalFacts`].

use serde_json::Value;

use super::identify::clean_field;
use crate::llm::wire::GatewayFamily;

/// Fingerprint paths sent first, in parallel (≤ 3 at a time, 1.5 s each).
pub(super) const FIRST_PROBES: [&str; 5] = [
    "/api/version",
    "/props",
    "/api/v1/models",
    "/api/v0/models",
    "/health",
];

/// Follow-ups for a host no first-batch answer decided: SGLang, vLLM, LocalAI.
pub(super) const FOLLOW_UPS: [&str; 3] = ["/get_model_info", "/version", "/readyz"];

/// What the fingerprint requests answered; every field starts at "no answer".
#[derive(Debug, Default, Clone)]
pub(super) struct LocalFacts {
    /// `/api/version` body.
    pub api_version: Option<Value>,
    /// `/props` body.
    pub props: Option<Value>,
    /// `/api/v1/models` body (LM Studio v1: `models[].key` with a `capabilities` object).
    pub lm_studio_v1_models: Option<Value>,
    /// `/api/v0/models` body (LM Studio legacy: `data[].max_context_length`).
    pub lm_studio_models: Option<Value>,
    /// `/health` answered 200.
    pub health: bool,
    /// `/readyz` answered 200.
    pub readyz: bool,
    /// `/get_model_info` answered 200 (SGLang).
    pub get_model_info: bool,
    /// `/version` answered 200 (vLLM).
    pub vllm_version: bool,
}

/// A recognised runtime.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LocalHit {
    /// The family to report.
    pub family: GatewayFamily,
    /// Runtime version when the fingerprint carries one.
    pub version: Option<String>,
    /// `default_generation_settings.n_ctx` (llama.cpp): the context of every model.
    pub context: Option<u64>,
    /// The typed port is the runtime's default (never decides alone).
    pub port_hint: bool,
}

/// Whether `port` is `family`'s default port.
fn is_default_port(family: GatewayFamily, port: Option<u16>) -> bool {
    match family {
        GatewayFamily::Ollama => port == Some(11434),
        GatewayFamily::LmStudio => port == Some(1234),
        GatewayFamily::Vllm => port == Some(8000),
        GatewayFamily::LlamaCpp => matches!(port, Some(9931 | 8080)),
        _ => false,
    }
}

impl LocalHit {
    /// vLLM recognised by its model list (`owned_by: "vllm"`) rather than its fingerprints.
    pub(super) fn listed_vllm(port: Option<u16>) -> Self {
        Self {
            family: GatewayFamily::Vllm,
            version: None,
            context: None,
            port_hint: is_default_port(GatewayFamily::Vllm, port),
        }
    }
}

impl LocalFacts {
    /// The LM Studio model list with per-model context: the v1 body, else the legacy one.
    pub(super) fn lm_studio_list(&self) -> Option<&Value> {
        self.lm_studio_v1_models
            .as_ref()
            .or(self.lm_studio_models.as_ref())
    }

    /// Whether no first-batch fingerprint decided: SGLang, vLLM and LocalAI need one more
    /// request each.
    pub(super) fn needs_followups(&self) -> bool {
        self.classify_first_batch().is_none()
    }

    /// `/api/v1/models`: `models[].key` with a `capabilities` object.
    fn is_lm_studio_v1(&self) -> bool {
        self.lm_studio_v1_models.as_ref().is_some_and(|body| {
            body.get("models")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row.get("key").is_some()
                            && row.get("capabilities").is_some_and(Value::is_object)
                    })
                })
        })
    }

    /// `/api/v0/models`: `data[].max_context_length` (older installs).
    fn is_lm_studio_v0(&self) -> bool {
        self.lm_studio_models.as_ref().is_some_and(|body| {
            body.get("data")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row.get("max_context_length").is_some())
                })
        })
    }

    fn classify_first_batch(&self) -> Option<GatewayFamily> {
        let props = self.props.as_ref().is_some_and(|body| {
            body.get("model_path").is_some() && body.get("total_slots").is_some()
        });
        if props {
            Some(GatewayFamily::LlamaCpp)
        } else if self
            .api_version
            .as_ref()
            .is_some_and(|body| body.get("version").is_some_and(Value::is_string))
        {
            Some(GatewayFamily::Ollama)
        } else if self.is_lm_studio_v1() || self.is_lm_studio_v0() {
            Some(GatewayFamily::LmStudio)
        } else {
            None
        }
    }

    /// The runtime, decided in this order: `/props`, `/api/version`, `/api/v1/models`,
    /// `/api/v0/models`, `/get_model_info` (with `/health`), `/version` (with `/health`),
    /// `/readyz`.
    pub(super) fn classify(&self, port: Option<u16>) -> Option<LocalHit> {
        let family = self
            .classify_first_batch()
            .or(if self.health && self.get_model_info {
                Some(GatewayFamily::Sglang)
            } else if self.health && self.vllm_version {
                Some(GatewayFamily::Vllm)
            } else if self.readyz {
                Some(GatewayFamily::LocalAi)
            } else {
                None
            })?;
        let version = if family == GatewayFamily::Ollama {
            self.api_version
                .as_ref()
                .and_then(|body| body.get("version"))
                .and_then(Value::as_str)
                .map(clean_field)
        } else {
            None
        };
        let context = if family == GatewayFamily::LlamaCpp {
            self.props
                .as_ref()
                .and_then(|body| body.get("default_generation_settings"))
                .and_then(|settings| settings.get("n_ctx"))
                .and_then(Value::as_u64)
        } else {
            None
        };
        Some(LocalHit {
            family,
            version,
            context,
            port_hint: is_default_port(family, port),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn disambiguation_order_and_port_hints() {
        let ollama = LocalFacts {
            api_version: Some(json!({"version": "0.12.3"})),
            health: true,
            ..LocalFacts::default()
        };
        let hit = ollama.classify(Some(11434)).unwrap();
        assert_eq!(
            (hit.family, hit.version.as_deref(), hit.port_hint),
            (GatewayFamily::Ollama, Some("0.12.3"), true)
        );
        assert!(!ollama.needs_followups());

        let llama = LocalFacts {
            props: Some(
                json!({"model_path": "/m.gguf", "total_slots": 4, "default_generation_settings": {"n_ctx": 8192}}),
            ),
            api_version: Some(json!({"version": "x"})),
            ..LocalFacts::default()
        };
        let hit = llama.classify(None).unwrap();
        assert_eq!(
            (hit.family, hit.context, hit.port_hint),
            (GatewayFamily::LlamaCpp, Some(8192), false)
        );

        let lm = LocalFacts {
            lm_studio_models: Some(json!({"data": [{"id": "m", "max_context_length": 4096}]})),
            ..LocalFacts::default()
        };
        assert_eq!(
            lm.classify(Some(1234)).unwrap().family,
            GatewayFamily::LmStudio
        );
        let not_lm = LocalFacts {
            lm_studio_models: Some(json!({"data": [{"id": "m"}]})),
            ..LocalFacts::default()
        };
        assert!(not_lm.classify(None).is_none());

        let health_only = LocalFacts {
            health: true,
            ..LocalFacts::default()
        };
        assert!(health_only.needs_followups() && health_only.classify(None).is_none());
        let sglang = LocalFacts {
            health: true,
            get_model_info: true,
            vllm_version: true,
            ..LocalFacts::default()
        };
        assert_eq!(sglang.classify(None).unwrap().family, GatewayFamily::Sglang);
        let vllm = LocalFacts {
            health: true,
            vllm_version: true,
            ..LocalFacts::default()
        };
        assert_eq!(
            vllm.classify(Some(8000)).unwrap().family,
            GatewayFamily::Vllm
        );
        let localai = LocalFacts {
            readyz: true,
            ..LocalFacts::default()
        };
        assert_eq!(
            localai.classify(None).unwrap().family,
            GatewayFamily::LocalAi
        );
        assert!(
            LocalFacts::default().classify(Some(11434)).is_none(),
            "a port alone never decides"
        );
    }
}
