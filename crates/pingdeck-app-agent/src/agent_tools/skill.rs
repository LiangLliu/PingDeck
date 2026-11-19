//! The `read_skill` tool: loads a skill's instructions or one of its supporting files.
//!
//! Single responsibility: look an enabled skill up by name and return its `SKILL.md` (or a file of
//! its folder through [`read_skill_file`], confined to that folder). The file work runs on
//! `spawn_blocking`.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::skills::{MAX_SKILL_READ_BYTES, SkillInfo, read_skill_file, read_skill_text};
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolRegistry, ToolSource,
    ToolSpec,
};
use crate::error::AppError;

/// The tool's function name.
pub const READ_SKILL: &str = "read_skill";

/// The arguments of `read_skill`.
#[derive(Debug, Deserialize)]
struct Args {
    name: String,
    path: Option<String>,
}

impl Args {
    fn parse(arguments: &Value) -> Result<Self, AppError> {
        serde_json::from_value(arguments.clone())
            .map_err(|error| AppError::validation(format!("Invalid arguments: {error}")))
    }
}

/// `read_skill` over the skills enabled for one run.
pub struct ReadSkillTool {
    spec: ToolSpec,
    skills: Vec<SkillInfo>,
}

impl ReadSkillTool {
    fn new(skills: Vec<SkillInfo>) -> Self {
        Self {
            spec: ToolSpec {
                name: READ_SKILL.to_owned(),
                description: "Load the instructions of an available skill by name, or one of its supporting files (a path relative to the skill folder, such as `references/api.md`). Read the skill before relying on it.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "The skill name"},
                        "path": {
                            "type": "string",
                            "description": "A text file inside the skill folder, relative to it. Default: SKILL.md."
                        }
                    },
                    "required": ["name"]
                }),
                read_only: true,
                destructive: false,
                source: ToolSource::Builtin,
                class: ToolClass::Read,
                parallel_safe: true,
            },
            skills,
        }
    }

    fn skill(&self, name: &str) -> Result<&SkillInfo, AppError> {
        self.skills
            .iter()
            .find(|skill| skill.name == name)
            .ok_or_else(|| AppError::not_found(format!("no enabled skill named {name}")))
    }
}

/// Registers `read_skill` when at least one skill is enabled (it has nothing to read otherwise).
pub fn register(registry: &mut ToolRegistry, skills: Vec<SkillInfo>) {
    if !skills.is_empty() {
        registry.register(Arc::new(ReadSkillTool::new(skills)));
    }
}

impl AgentTool for ReadSkillTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args = Args::parse(arguments)?;
        self.skill(&args.name)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ToolContext<'a>,
        arguments: Value,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args = match Args::parse(&arguments) {
                Ok(args) => args,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let skill = match self.skill(&args.name) {
                Ok(skill) => skill.clone(),
                Err(error) => return ToolOutput::error(error.message()),
            };
            let read = tokio::task::spawn_blocking(move || match args.path {
                Some(path) => read_skill_file(&skill, &path),
                None => read_skill_text(&skill, MAX_SKILL_READ_BYTES),
            })
            .await;
            match read {
                Ok(Ok(text)) => ToolOutput::text(text),
                Ok(Err(error)) => ToolOutput::error(error.message()),
                Err(error) => ToolOutput::error(format!("skill read failed: {error}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::agent::skills::discover_skills;
    use crate::agent::tools::TestParts;
    use crate::app::AppState;
    use crate::cancel::CancelToken;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "pingdeck-skill-tool-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tool() -> (TempDir, Arc<dyn AgentTool>) {
        let temp = TempDir::new();
        let dir = temp.0.join("alpha");
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: Alpha\ndescription: d\n---\nUse me",
        )
        .unwrap();
        fs::write(dir.join("references/api.md"), "api notes").unwrap();
        let skills = discover_skills(std::slice::from_ref(&temp.0), &[]);
        let mut registry = ToolRegistry::new();
        register(&mut registry, skills);
        (temp, registry.get(READ_SKILL).unwrap().clone())
    }

    fn with_ctx<T>(run: impl FnOnce(&ToolContext<'_>) -> T) -> T {
        let state = AppState::open_in_memory().unwrap();
        let settings = state.settings();
        let cancel = CancelToken::new();
        let parts = TestParts::default();
        run(&ToolContext::for_test(
            Some(&state as &(dyn std::any::Any + Send + Sync)),
            &settings,
            &cancel,
            &parts,
        ))
    }

    #[test]
    fn it_is_registered_only_when_a_skill_is_enabled() {
        let mut registry = ToolRegistry::new();
        register(&mut registry, Vec::new());
        assert!(registry.is_empty());
        let (_temp, tool) = tool();
        assert_eq!(tool.spec().name, "read_skill");
        assert!(tool.spec().read_only && !tool.spec().destructive);
        assert_eq!(tool.spec().source, ToolSource::Builtin);
        assert_eq!(
            (tool.spec().class, tool.spec().parallel_safe),
            (ToolClass::Read, true)
        );
    }

    #[test]
    fn inspect_is_read_only_and_rejects_unknown_skills_and_bad_arguments() {
        let (_temp, tool) = tool();
        with_ctx(|ctx| {
            let inspection = tool.inspect(ctx, &json!({"name": "Alpha"})).unwrap();
            assert_eq!(
                inspection,
                CallInspection {
                    read_only: true,
                    ..CallInspection::default()
                }
            );
            assert!(
                tool.inspect(ctx, &json!({"name": "Alpha", "path": "references/api.md"}))
                    .is_ok()
            );
            assert!(matches!(
                tool.inspect(ctx, &json!({"name": "Ghost"})),
                Err(AppError::NotFound { .. })
            ));
            assert!(matches!(
                tool.inspect(ctx, &json!({})),
                Err(AppError::Validation { .. })
            ));
        });
    }

    #[tokio::test]
    async fn it_reads_skill_md_by_default_and_supporting_files_by_path() {
        let (_temp, tool) = tool();
        let state = AppState::open_in_memory().unwrap();
        let settings = state.settings();
        let cancel = CancelToken::new();
        let parts = TestParts::default();
        let mut ctx = ToolContext::for_test(
            Some(&state as &(dyn std::any::Any + Send + Sync)),
            &settings,
            &cancel,
            &parts,
        );
        ctx.workspace_dir = Some(Path::new("/"));
        let skill = tool.call(&ctx, json!({"name": "Alpha"})).await;
        assert!(!skill.is_error);
        assert!(skill.text_content().ends_with("Use me"));
        let file = tool
            .call(&ctx, json!({"name": "Alpha", "path": "references/api.md"}))
            .await;
        assert_eq!(file.text_content(), "api notes");

        for arguments in [
            json!({"name": "Alpha", "path": "../../etc/hosts"}),
            json!({"name": "Alpha", "path": "/etc/hosts"}),
            json!({"name": "Alpha", "path": "references/none.md"}),
            json!({"name": "Ghost"}),
            json!({}),
        ] {
            let output = tool.call(&ctx, arguments.clone()).await;
            assert!(output.is_error, "{arguments}");
        }
    }
}
