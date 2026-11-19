//! Test-only `Default` for the use-case input DTOs, so a test literal names only the fields it
//! means and ends with `..Default::default()`. A new DTO field is added here once instead of in
//! every literal. Ids and names default to empty; `enabled` defaults to `true`.
// The DTOs must not derive `Default` outside tests, so the trivially derivable impls stay manual.
#![allow(clippy::derivable_impls)]

use crate::app::{
    EnvironmentVariableInput, ExecuteDraftRequestInput, ExecuteRequestInput, GlobalsSaveInput,
    OAuthTokenInput, RunCollectionInput, SaveCollectionInput, SaveEnvironmentInput,
    SaveFolderInput, SaveRequestInput,
};
use crate::workspace::{AuthConfig, HttpMethod, NetworkOptions, ProtocolKind, RequestBody};

impl Default for SaveCollectionInput {
    fn default() -> Self {
        Self {
            id: String::new(),
            workspace_id: String::new(),
            name: String::new(),
            description: String::new(),
            auth: AuthConfig::Inherit,
            variables: Vec::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        }
    }
}

impl Default for SaveFolderInput {
    fn default() -> Self {
        Self {
            id: String::new(),
            collection_id: String::new(),
            parent_folder_id: None,
            name: String::new(),
            description: String::new(),
            auth: AuthConfig::Inherit,
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
        }
    }
}

impl Default for SaveEnvironmentInput {
    fn default() -> Self {
        Self {
            id: String::new(),
            workspace_id: String::new(),
            name: String::new(),
            variables: Vec::new(),
            confirm_before_send: None,
            confirm_safe_methods: None,
            declared_hosts: None,
            base_revision: None,
        }
    }
}

impl Default for EnvironmentVariableInput {
    fn default() -> Self {
        Self {
            id: None,
            key: String::new(),
            value: None,
            is_secret: false,
            enabled: true,
            note: String::new(),
        }
    }
}

impl Default for GlobalsSaveInput {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            variables: Vec::new(),
            base_revision: None,
        }
    }
}

impl Default for SaveRequestInput {
    fn default() -> Self {
        Self {
            id: String::new(),
            workspace_id: String::new(),
            collection_id: None,
            folder_id: None,
            name: String::new(),
            protocol: ProtocolKind::HttpRest,
            method: HttpMethod::Get,
            url: String::new(),
            query_params: Vec::new(),
            path_variables: Vec::new(),
            headers: Vec::new(),
            auth: AuthConfig::None,
            body: RequestBody::None,
            network_options: NetworkOptions::default(),
            assertions: Vec::new(),
            description: String::new(),
            pre_request_script: String::new(),
            test_script: String::new(),
            position: 0,
            mode: Default::default(),
            ai: None,
            examples: None,
        }
    }
}

impl Default for ExecuteDraftRequestInput {
    fn default() -> Self {
        Self {
            request: SaveRequestInput::default(),
            environment_id: None,
            confirmed: None,
            save_to_history: None,
        }
    }
}

impl Default for ExecuteRequestInput {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            request_id: String::new(),
            environment_id: None,
            confirmed: None,
        }
    }
}

impl Default for RunCollectionInput {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            collection_id: String::new(),
            environment_id: None,
            iterations: None,
            delay_ms: None,
            stop_on_error: None,
            data: None,
            confirmed: None,
            folder_id: None,
            request_ids: None,
        }
    }
}

impl Default for OAuthTokenInput {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            environment_id: None,
            collection_id: None,
            request_id: None,
            auth: AuthConfig::None,
        }
    }
}
