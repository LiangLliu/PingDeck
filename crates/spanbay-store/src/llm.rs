//! The LLM facade of the store: the `catalog` and `store` repositories, plus crate-internal
//! aliases of the llm-domain, client-config and model modules they are written against.

pub(crate) use spanbay_client_config as client_config;
pub(crate) use spanbay_llm_domain::llm::{
    balance_state, capture_record, failure, probe, providers, status, usage,
};
pub(crate) use spanbay_model::{capabilities, wire};

pub mod catalog;
pub mod store;
