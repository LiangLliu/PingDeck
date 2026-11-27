//! Request model helpers: URL/query/path-variable sync, new/normalised drafts and effective auth.
//!
//! Single responsibility: every rule about what a request *is* while it is being edited, so a
//! shell re-implements none of it. Three files: [`url_sync`] (URL text ⇄ Params table ⇄ path
//! variables), [`draft`] (defaults, templates, normalisation, history → draft) and
//! [`effective_auth`] (what an `Inherit` auth resolves to, and the `Authorization` preview).
//! The AI API mode rules (HTTP ⇄ AI switch, the `{{provider key}}` send guard) live in
//! `workspace::ai_request` and are re-exported here as `request_switch_mode` and
//! `request_ensure_sendable`; a draft is a plain `Request`, so `mode` and `ai` travel through
//! every function of this module untouched, and URL sync treats the non-routable label of a
//! provider target like any other URL text.
//!
//! # Keeping a synchronous copy of the URL sync
//!
//! URL sync runs on every keystroke, and an IPC round trip per keystroke makes the URL bar lag
//! and lets a slow answer overwrite newer typing. A shell therefore MAY keep a local synchronous
//! copy of the five URL functions, **provided** it passes every case in
//! `fixtures/url-sync-cases.json` (generated from this module, consumed by
//! `tests/url_sync_fixture.rs` here and by `src/api/logic/requestModel.test.ts`). The core stays
//! the specification: when a case changes, the fixture changes first. Everything else here
//! (drafts, effective auth) is not hot and always goes through the core.
//!
//! # Deliberate differences from the former TypeScript (`draft.ts`)
//!
//! * One `{{variable}}` token definition everywhere: `{{`, one or more characters that are
//!   neither `{` nor `}`, `}}`. Tokens are never encoded or decoded; they keep `+`, `%`, `&`, `=`
//!   and spaces untouched in both directions (the TS decoded `{{x+y}}` into `{{x y}}`, and split
//!   `{{p&q}}` into two pairs).
//! * Query encoding is the `encodeURIComponent` set (everything but `A-Za-z0-9-_.!~*'()` is
//!   `%XX`, a space is `%20`, never `+`), outside tokens. The executor (`executor/request.rs`)
//!   re-serialises with `application/x-www-form-urlencoded` at send time, but it first *decodes*
//!   the URL text with the same form decoding used here (`+` and `%20` are both a space,
//!   `%2B` is `+`), so the URL bar text and the sent request always carry identical keys and
//!   values; the bar keeps the readable `%20` form.
//! * The query is the text after the first `?` **before** the first `#`. The TS parsed a `?`
//!   that sat inside the fragment as a query. A URL without a `?` has no URL-backed rows
//!   (the TS kept the previous rows, which left stale rows after deleting `?a=1` from the bar).
//! * Editing the URL keeps the rows the URL cannot represent (disabled rows and enabled rows
//!   with a blank key), appended after the rows parsed from the URL; the TS dropped them.
//! * Path variable names: a `:name` only counts right after a `/` (or at the start), exactly
//!   where the executor substitutes it, so `host:port` and `a:b` are no longer variables; names
//!   are listed in URL order (the TS listed every `:name` before every `{name}`).

mod draft;
mod effective_auth;
mod url_sync;
pub(crate) use url_sync::URI_COMPONENT;

pub use draft::{
    RequestDraftOverrides, RequestTemplate, draft_normalize, request_draft_from_history,
    request_draft_from_history_resolved, request_new_draft,
};
pub use effective_auth::{
    AuthPreview, AuthPreviewLocation, AuthSource, EffectiveAuth, EffectiveAuthInput,
    request_effective_auth,
};
pub use url_sync::{
    UrlSyncEdit, UrlSyncInput, UrlSyncOutput, merge_path_variables, path_variable_names,
    query_params_from_url, sync_request_url, url_with_query_params,
};

pub use crate::workspace::ai_request::{
    ensure_sendable as request_ensure_sendable, switch_mode as request_switch_mode,
};

#[cfg(test)]
mod ai_mode_tests;
