//! Generate PipelineDefinition from sprint context for pipeline-based execution.
//!
//! When `--engine=pipeline` is used, this module converts the sprint's stories
//! into a DAG of pipeline stages that the kapable-agent will execute.

use std::collections::HashMap;

use kapable_pipeline::types::{
    BashStepDef, PipelineDefinition, RunCondition, StageDefinition, StepCommon, StepDefinition,
};

/// Context needed to generate a sprint pipeline.
pub struct SprintPipelineContext {
    pub epic_code: String,
    pub sprint_number: i32,
    pub session_id: String,
    pub stories: Vec<StoryContext>,
    pub product_brief: Option<String>,
    pub epic_intent: String,
    pub builder_agent_content: String,
    pub judge_agent_content: String,
    pub scrum_master_agent_content: String,
    pub working_dir: String,
    pub model_override: Option<String>,
    pub effort_override: Option<String>,
    pub budget_override: Option<f64>,
    pub add_dirs: Vec<String>,
}

/// Per-story context for pipeline generation.
pub struct StoryContext {
    pub code: String,
    pub id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub tasks: Vec<String>,
    pub story_json: serde_json::Value,
}

/// Generate a PipelineDefinition for a sprint.
///
/// Produces a DAG with:
/// 1. `source` stage -- emits story JSON as output variables
/// 2. `build-{code}` stages -- one per story, parallel, agent steps
/// 3. `judge-code` stage -- depends on all builds, reviews code quality
/// 4. `commit-merge` stage -- merges worktrees and pushes
/// 5. `retro-{code}` stages -- resume builder sessions for retrospective
/// 6. `output` stage -- summarizes results
pub fn generate_sprint_pipeline(ctx: &SprintPipelineContext) -> PipelineDefinition {
    let mut stages: Vec<StageDefinition> = Vec::new();

    // -- Stage: source --
    // Emit story metadata as output variables
    let mut source_commands = Vec::new();
    for story in &ctx.stories {
        let story_json_escaped = serde_json::to_string(&story.story_json)
            .unwrap_or_default()
            .replace('\'', "'\"'\"'");
        source_commands.push(format!(
            "echo '##kapable[set name=story_{code}]{json}'",
            code = story.code.replace('-', "_"),
            json = story_json_escaped,
        ));
    }
    let source_command = source_commands.join(" && ");

    stages.push(StageDefinition {
        id: "source".to_string(),
        label: Some("Load sprint stories".to_string()),
        depends_on: vec![],
        steps: vec![StepDefinition::Bash {
            common: step_common("emit-stories", Some("Emit story data"), Some(30)),
            def: BashStepDef {
                command: source_command,
                working_dir: Some(ctx.working_dir.clone()),
            },
        }],
        timeout_secs: Some(60),
        allow_failure: false,
        run_on: RunCondition::default(),
        condition: None,
        matrix: None,
    });

    // -- Stages: build-{code} --
    // One agent step per story, all depend on source, run in parallel
    let build_stage_ids: Vec<String> = ctx
        .stories
        .iter()
        .map(|s| format!("build-{}", s.code.to_lowercase()))
        .collect();

    for (i, story) in ctx.stories.iter().enumerate() {
        let prompt = format!(
            "You are executing story {} for epic {}.\n\n\
             ## Story\n{}\n\n\
             ## Description\n{}\n\n\
             ## Acceptance Criteria\n{}\n\n\
             ## Tasks\n{}\n\n\
             ## Epic Intent\n{}\n\n\
             {}\
             Implement this story completely. Mark tasks done as you complete them \
             using `epic-runner backlog task-done {} <INDEX>` and verify ACs with \
             `epic-runner backlog ac-verify {} <INDEX>`.",
            story.code,
            ctx.epic_code,
            story.title,
            story.description,
            story.acceptance_criteria.join("\n"),
            story.tasks.join("\n"),
            ctx.epic_intent,
            ctx.product_brief
                .as_ref()
                .map(|b| format!("## Product Brief\n{}\n\n", b))
                .unwrap_or_default(),
            story.code,
            story.code,
        );

        let model = ctx
            .model_override
            .clone()
            .unwrap_or_else(|| "sonnet".to_string());
        let budget = ctx.budget_override.unwrap_or(5.0);

        // Build the claude CLI command string
        let claude_args = build_claude_command(
            &model,
            ctx.effort_override.as_deref().unwrap_or("high"),
            &story.id,
            budget,
            &prompt,
            Some(&ctx.builder_agent_content),
            Some(&format!("{}/{}", ctx.working_dir, story.code)),
            false, // not resume
            &ctx.add_dirs,
        );

        stages.push(StageDefinition {
            id: build_stage_ids[i].clone(),
            label: Some(format!("Build: {} -- {}", story.code, story.title)),
            depends_on: vec!["source".to_string()],
            steps: vec![StepDefinition::Bash {
                common: step_common(
                    &format!("build-{}", story.code.to_lowercase()),
                    Some(&format!("Execute story {}", story.code)),
                    Some(3600),
                ),
                def: BashStepDef {
                    command: claude_args,
                    working_dir: Some(ctx.working_dir.clone()),
                },
            }],
            timeout_secs: Some(3600),
            allow_failure: true, // Individual story failure shouldn't block judge
            run_on: RunCondition::default(),
            condition: None,
            matrix: None,
        });
    }

    // -- Stage: judge-code --
    let judge_prompt = format!(
        "Review the code changes from sprint {} of epic {}.\n\
         Stories built: {}\n\
         Check code quality, test coverage, and acceptance criteria verification.\n\
         Output your verdict as JSON with fields: passed (bool), issues (array), score (0-100).",
        ctx.sprint_number,
        ctx.epic_code,
        ctx.stories
            .iter()
            .map(|s| s.code.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );

    let judge_command = build_claude_command(
        ctx.model_override.as_deref().unwrap_or("sonnet"),
        "high",
        &format!("judge-{}-s{}", ctx.epic_code, ctx.sprint_number),
        2.0,
        &judge_prompt,
        Some(&ctx.judge_agent_content),
        Some(&ctx.working_dir),
        false,
        &ctx.add_dirs,
    );

    stages.push(StageDefinition {
        id: "judge-code".to_string(),
        label: Some("Code quality review".to_string()),
        depends_on: build_stage_ids.clone(),
        steps: vec![StepDefinition::Bash {
            common: step_common("judge", Some("Judge code quality"), Some(1800)),
            def: BashStepDef {
                command: judge_command,
                working_dir: Some(ctx.working_dir.clone()),
            },
        }],
        timeout_secs: Some(1800),
        allow_failure: true,
        run_on: RunCondition::Always,
        condition: None,
        matrix: None,
    });

    // -- Stage: commit-merge --
    let merge_script = format!(
        "cd {} && git add -A && git commit -m 'sprint {}: {}' --allow-empty || true",
        ctx.working_dir, ctx.sprint_number, ctx.epic_code,
    );

    stages.push(StageDefinition {
        id: "commit-merge".to_string(),
        label: Some("Commit and merge".to_string()),
        depends_on: vec!["judge-code".to_string()],
        steps: vec![StepDefinition::Bash {
            common: step_common("merge", Some("Merge changes"), Some(120)),
            def: BashStepDef {
                command: merge_script,
                working_dir: Some(ctx.working_dir.clone()),
            },
        }],
        timeout_secs: Some(300),
        allow_failure: false,
        run_on: RunCondition::Always,
        condition: None,
        matrix: None,
    });

    // -- Stages: retro-{code} --
    for story in &ctx.stories {
        let retro_prompt = format!(
            "Write a retrospective for story {}. What went well? What could improve? \
             Output JSON: {{ went_well: [], improve: [], action_items: [] }}",
            story.code,
        );

        let retro_command = build_claude_command(
            ctx.model_override.as_deref().unwrap_or("sonnet"),
            "medium",
            &story.id, // Resume the builder session
            1.0,
            &retro_prompt,
            Some(&ctx.scrum_master_agent_content),
            Some(&ctx.working_dir),
            true, // resume
            &ctx.add_dirs,
        );

        stages.push(StageDefinition {
            id: format!("retro-{}", story.code.to_lowercase()),
            label: Some(format!("Retrospective: {}", story.code)),
            depends_on: vec!["commit-merge".to_string()],
            steps: vec![StepDefinition::Bash {
                common: step_common(
                    &format!("retro-{}", story.code.to_lowercase()),
                    Some(&format!("Retro for {}", story.code)),
                    Some(600),
                ),
                def: BashStepDef {
                    command: retro_command,
                    working_dir: Some(ctx.working_dir.clone()),
                },
            }],
            timeout_secs: Some(600),
            allow_failure: true,
            run_on: RunCondition::Always,
            condition: None,
            matrix: None,
        });
    }

    // -- Stage: output --
    stages.push(StageDefinition {
        id: "output".to_string(),
        label: Some("Sprint summary".to_string()),
        depends_on: ctx
            .stories
            .iter()
            .map(|s| format!("retro-{}", s.code.to_lowercase()))
            .collect(),
        steps: vec![StepDefinition::Bash {
            common: step_common("summary", Some("Emit sprint summary"), Some(30)),
            def: BashStepDef {
                command: format!(
                    "echo '##kapable[set name=sprint_status]completed' && \
                     echo '##kapable[set name=epic_code]{}' && \
                     echo '##kapable[set name=sprint_number]{}'",
                    ctx.epic_code, ctx.sprint_number,
                ),
                working_dir: None,
            },
        }],
        timeout_secs: Some(60),
        allow_failure: false,
        run_on: RunCondition::Always,
        condition: None,
        matrix: None,
    });

    PipelineDefinition {
        name: format!("{}-sprint-{}", ctx.epic_code, ctx.sprint_number),
        description: Some(format!(
            "Sprint {} of epic {}: {}",
            ctx.sprint_number, ctx.epic_code, ctx.epic_intent
        )),
        version: Some("1.0".to_string()),
        variables: HashMap::new(),
        secrets: vec![],
        stages,
        finally: vec![],
        on_complete: vec![],
        triggers: vec![],
        timeout_secs: Some(7200),
        auto_rollback: false,
        concurrency_group: Some(format!("epic-{}", ctx.epic_code)),
    }
}

/// Build a StepCommon with sensible defaults.
fn step_common(id: &str, label: Option<&str>, timeout_secs: Option<u64>) -> StepCommon {
    StepCommon {
        id: id.to_string(),
        label: label.map(String::from),
        timeout_secs,
        retry: None,
        env: HashMap::new(),
        run_on: RunCondition::default(),
        allow_failure: false,
        capture_output: true,
    }
}

/// Build a claude CLI command string.
#[allow(clippy::too_many_arguments)]
fn build_claude_command(
    model: &str,
    _effort: &str,
    session_id: &str,
    budget: f64,
    prompt: &str,
    system_prompt: Option<&str>,
    _working_dir: Option<&str>,
    resume: bool,
    add_dirs: &[String],
) -> String {
    let mut parts = vec![
        "claude".to_string(),
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--model".to_string(),
        model.to_string(),
        "--max-budget-usd".to_string(),
        format!("{:.2}", budget),
    ];

    if resume {
        parts.push("--resume".to_string());
        parts.push(session_id.to_string());
    } else {
        parts.push("--session-id".to_string());
        parts.push(session_id.to_string());
    }

    if let Some(sp) = system_prompt {
        // Truncate to reasonable size for command line
        let truncated = if sp.len() > 4000 { &sp[..4000] } else { sp };
        parts.push("--append-system-prompt".to_string());
        parts.push(format!("'{}'", truncated.replace('\'', "'\\''")));
    }

    for dir in add_dirs {
        parts.push("--add-dir".to_string());
        parts.push(dir.clone());
    }

    // Prompt goes last via --prompt flag
    parts.push("--prompt".to_string());
    let escaped_prompt = prompt.replace('\'', "'\\''");
    parts.push(format!("'{}'", escaped_prompt));

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_sprint_pipeline_basic() {
        let ctx = SprintPipelineContext {
            epic_code: "AUTH-001".to_string(),
            sprint_number: 1,
            session_id: "sess-123".to_string(),
            stories: vec![StoryContext {
                code: "AUTH-001-S001".to_string(),
                id: "uuid-1".to_string(),
                title: "Add login".to_string(),
                description: "Implement login flow".to_string(),
                acceptance_criteria: vec!["User can log in".to_string()],
                tasks: vec!["Create login form".to_string()],
                story_json: serde_json::json!({"code": "AUTH-001-S001"}),
            }],
            product_brief: None,
            epic_intent: "Build auth system".to_string(),
            builder_agent_content: "You are a builder".to_string(),
            judge_agent_content: "You are a judge".to_string(),
            scrum_master_agent_content: "You are a scrum master".to_string(),
            working_dir: "/tmp/work".to_string(),
            model_override: None,
            effort_override: None,
            budget_override: None,
            add_dirs: vec![],
        };

        let pipeline = generate_sprint_pipeline(&ctx);
        assert_eq!(pipeline.name, "AUTH-001-sprint-1");
        // source + build + judge + commit + retro + output = 6 stages
        assert_eq!(pipeline.stages.len(), 6);
        assert_eq!(pipeline.stages[0].id, "source");
        assert!(pipeline.stages[1].id.starts_with("build-"));
        assert_eq!(pipeline.stages[2].id, "judge-code");
        assert_eq!(pipeline.stages[3].id, "commit-merge");
        assert!(pipeline.stages[4].id.starts_with("retro-"));
        assert_eq!(pipeline.stages[5].id, "output");
    }

    #[test]
    fn test_generate_multi_story_pipeline() {
        let ctx = SprintPipelineContext {
            epic_code: "FE-002".to_string(),
            sprint_number: 3,
            session_id: "sess-456".to_string(),
            stories: vec![
                StoryContext {
                    code: "FE-002-S001".to_string(),
                    id: "uuid-a".to_string(),
                    title: "Header component".to_string(),
                    description: "Build header".to_string(),
                    acceptance_criteria: vec!["Header renders".to_string()],
                    tasks: vec!["Create Header.tsx".to_string()],
                    story_json: serde_json::json!({"code": "FE-002-S001"}),
                },
                StoryContext {
                    code: "FE-002-S002".to_string(),
                    id: "uuid-b".to_string(),
                    title: "Footer component".to_string(),
                    description: "Build footer".to_string(),
                    acceptance_criteria: vec!["Footer renders".to_string()],
                    tasks: vec!["Create Footer.tsx".to_string()],
                    story_json: serde_json::json!({"code": "FE-002-S002"}),
                },
            ],
            product_brief: Some("A web app".to_string()),
            epic_intent: "Build layout".to_string(),
            builder_agent_content: "builder".to_string(),
            judge_agent_content: "judge".to_string(),
            scrum_master_agent_content: "scrum".to_string(),
            working_dir: "/tmp/work".to_string(),
            model_override: Some("opus".to_string()),
            effort_override: Some("high".to_string()),
            budget_override: Some(10.0),
            add_dirs: vec!["/extra".to_string()],
        };

        let pipeline = generate_sprint_pipeline(&ctx);
        assert_eq!(pipeline.name, "FE-002-sprint-3");
        // source + 2 builds + judge + commit + 2 retros + output = 8 stages
        assert_eq!(pipeline.stages.len(), 8);

        // Build stages depend on source
        assert_eq!(pipeline.stages[1].depends_on, vec!["source"]);
        assert_eq!(pipeline.stages[2].depends_on, vec!["source"]);

        // Judge depends on both builds
        assert_eq!(pipeline.stages[3].depends_on.len(), 2);
    }

    #[test]
    fn test_build_claude_command_basic() {
        let cmd = build_claude_command(
            "sonnet",
            "high",
            "test-session",
            5.0,
            "Hello world",
            None,
            None,
            false,
            &[],
        );
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("--model"));
        assert!(cmd.contains("sonnet"));
        assert!(cmd.contains("--session-id"));
        assert!(cmd.contains("test-session"));
        assert!(!cmd.contains("--resume"));
    }

    #[test]
    fn test_build_claude_command_resume() {
        let cmd = build_claude_command(
            "opus",
            "high",
            "existing-session",
            10.0,
            "Continue",
            None,
            None,
            true,
            &[],
        );
        assert!(cmd.contains("--resume"));
        assert!(cmd.contains("existing-session"));
        assert!(!cmd.contains("--session-id"));
    }

    #[test]
    fn test_build_claude_command_with_add_dirs() {
        let cmd = build_claude_command(
            "sonnet",
            "high",
            "sess",
            1.0,
            "test",
            None,
            None,
            false,
            &["/dir1".to_string(), "/dir2".to_string()],
        );
        assert!(cmd.contains("--add-dir"));
        assert!(cmd.contains("/dir1"));
        assert!(cmd.contains("/dir2"));
    }
}
