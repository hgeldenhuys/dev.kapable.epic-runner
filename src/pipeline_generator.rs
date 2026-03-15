//! Generate PipelineDefinition from sprint context for pipeline-based execution.
//!
//! When `--engine=pipeline` is used, this module converts the sprint's stories
//! into a DAG of pipeline stages using proper `StepDefinition::Agent` steps
//! (not Bash-wrapped claude commands). The AgentStepRunner handles Claude CLI
//! dispatch, stream-json parsing, hooks, and output variable extraction.

use std::collections::HashMap;

use kapable_pipeline::types::{
    AgentStepDef, BashStepDef, PipelineDefinition, RunCondition, StageDefinition, StepCommon,
    StepDefinition,
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
    /// Claude Code hooks settings (stop-gate, track-files).
    pub hooks_settings: Option<serde_json::Value>,
    /// Deploy profile: "none" | "connect_app" | "bootstrap".
    pub deploy_profile: String,
    /// App ID for Connect App Pipeline deploy.
    pub deploy_app_id: Option<String>,
    /// Kapable API URL for bash steps.
    pub api_url: String,
    /// Kapable API key for bash steps.
    pub api_key: String,
    /// Product-specific definition of done (injected into judge).
    pub product_definition_of_done: Option<String>,
    /// Previous sprint learnings (injected into builder system prompt).
    pub previous_learnings: Option<String>,
    /// True = serial story execution (default), false = parallel.
    pub serial: bool,
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
/// 1. `source` stage — emits story JSON as output variables
/// 2. `build-{code}` stages — one per story, agent steps (serial or parallel)
/// 3. `judge-code` stage — depends on all builds, reviews code quality
/// 4. `commit-merge` stage — git operations (always runs)
/// 5. `deploy` stage — conditional on deploy_profile (bash curl)
/// 6. `retro-{code}` stages — resume builder sessions for retrospective
/// 7. `output` stage — summarizes results (always runs)
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
    // One agent step per story. Serial (chained depends_on) or parallel (all depend on source).
    let build_stage_ids: Vec<String> = ctx
        .stories
        .iter()
        .map(|s| format!("build-{}", s.code.to_lowercase()))
        .collect();

    // Build system prompt from product brief + previous learnings
    let system_prompt = build_system_prompt(
        ctx.product_brief.as_deref(),
        ctx.previous_learnings.as_deref(),
    );

    for (i, story) in ctx.stories.iter().enumerate() {
        let prompt = format!(
            "You are executing story {} for epic {}.\n\n\
             ## Story\n{}\n\n\
             ## Description\n{}\n\n\
             ## Acceptance Criteria\n{}\n\n\
             ## Tasks\n{}\n\n\
             ## Epic Intent\n{}\n\n\
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
            story.code,
            story.code,
        );

        // Serial: chain dependencies. Parallel: all depend on source.
        let depends = if ctx.serial && i > 0 {
            vec![build_stage_ids[i - 1].clone()]
        } else {
            vec!["source".to_string()]
        };

        let model = ctx
            .model_override
            .clone()
            .or_else(|| Some("opus".to_string()));
        let budget = ctx.budget_override.unwrap_or(5.0);

        stages.push(StageDefinition {
            id: build_stage_ids[i].clone(),
            label: Some(format!("Build: {} — {}", story.code, story.title)),
            depends_on: depends,
            steps: vec![StepDefinition::Agent {
                common: step_common(
                    &format!("build-{}", story.code.to_lowercase()),
                    Some(&format!("Execute story {}", story.code)),
                    Some(3600),
                ),
                def: AgentStepDef {
                    model,
                    effort: ctx
                        .effort_override
                        .clone()
                        .unwrap_or_else(|| "high".to_string()),
                    session_id: Some(story.id.clone()),
                    budget_usd: budget,
                    prompt,
                    system_prompt: system_prompt.clone(),
                    agent: None,
                    agent_dir: Some(ctx.working_dir.clone()),
                    resume: false,
                    chrome: false,
                    worktree: None,
                    max_turns: 200,
                    heartbeat_timeout_secs: 120,
                    add_dirs: ctx.add_dirs.clone(),
                    hooks_settings: ctx.hooks_settings.clone(),
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
         Epic intent: {}\n\
         {}\
         Check code quality, test coverage, and acceptance criteria verification.\n\
         Output your verdict as JSON with fields:\n\
         - intent_satisfied (bool)\n\
         - mission_progress (0-100)\n\
         - deploy_ready (bool)\n\
         - stories_completed (array of story codes)\n\
         - action_items (array of strings)\n\
         - next_sprint_goal (string, if more work needed)",
        ctx.sprint_number,
        ctx.epic_code,
        ctx.stories
            .iter()
            .map(|s| s.code.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        ctx.epic_intent,
        ctx.product_definition_of_done
            .as_ref()
            .map(|d| format!("Definition of Done:\n{}\n\n", d))
            .unwrap_or_default(),
    );

    stages.push(StageDefinition {
        id: "judge-code".to_string(),
        label: Some("Code quality review".to_string()),
        depends_on: build_stage_ids.clone(),
        steps: vec![StepDefinition::Agent {
            common: step_common("judge", Some("Judge code quality"), Some(1800)),
            def: AgentStepDef {
                model: ctx
                    .model_override
                    .clone()
                    .or_else(|| Some("sonnet".to_string())),
                effort: "high".to_string(),
                session_id: Some(format!(
                    "judge-{}-s{}",
                    ctx.epic_code, ctx.sprint_number
                )),
                budget_usd: 2.0,
                prompt: judge_prompt,
                system_prompt: system_prompt.clone(),
                agent: None,
                agent_dir: Some(ctx.working_dir.clone()),
                resume: false,
                chrome: false,
                worktree: None,
                max_turns: 50,
                heartbeat_timeout_secs: 120,
                add_dirs: ctx.add_dirs.clone(),
                hooks_settings: None, // Judge doesn't need stop-gate
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
        "cd {dir} && git add -A && git commit -m 'sprint {num}: {epic}' --allow-empty || true",
        dir = ctx.working_dir,
        num = ctx.sprint_number,
        epic = ctx.epic_code,
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

    // -- Stage: deploy (conditional) --
    let retro_depends_on = if ctx.deploy_profile != "none" {
        if let Some(ref app_id) = ctx.deploy_app_id {
            let deploy_cmd = format!(
                "curl -sf -X POST '{url}/v1/apps/{app_id}/environments/production/deploy' \
                 -H 'x-api-key: {key}' && \
                 echo '##kapable[set name=deploy_triggered]true'",
                url = ctx.api_url,
                app_id = app_id,
                key = ctx.api_key,
            );

            stages.push(StageDefinition {
                id: "deploy".to_string(),
                label: Some("Deploy to production".to_string()),
                depends_on: vec!["commit-merge".to_string()],
                steps: vec![StepDefinition::Bash {
                    common: step_common("trigger-deploy", Some("Trigger deploy"), Some(120)),
                    def: BashStepDef {
                        command: deploy_cmd,
                        working_dir: None,
                    },
                }],
                timeout_secs: Some(300),
                allow_failure: true,
                run_on: RunCondition::OnSuccess,
                condition: None,
                matrix: None,
            });

            "deploy".to_string()
        } else {
            "commit-merge".to_string()
        }
    } else {
        "commit-merge".to_string()
    };

    // -- Stages: retro-{code} --
    // Resume each builder session for retrospective interview.
    for story in &ctx.stories {
        let retro_prompt = format!(
            "Interview the builder about story {}.\n\
             What went well? What could improve? What should the next sprint know?\n\
             Output JSON: {{ \"learnings\": string, \"went_well\": [], \"improve\": [], \"action_items\": [] }}",
            story.code,
        );

        stages.push(StageDefinition {
            id: format!("retro-{}", story.code.to_lowercase()),
            label: Some(format!("Retrospective: {}", story.code)),
            depends_on: vec![retro_depends_on.clone()],
            steps: vec![StepDefinition::Agent {
                common: step_common(
                    &format!("retro-{}", story.code.to_lowercase()),
                    Some(&format!("Retro for {}", story.code)),
                    Some(600),
                ),
                def: AgentStepDef {
                    model: ctx
                        .model_override
                        .clone()
                        .or_else(|| Some("sonnet".to_string())),
                    effort: "medium".to_string(),
                    session_id: Some(story.id.clone()),
                    budget_usd: 1.0,
                    prompt: retro_prompt,
                    system_prompt: None,
                    agent: None,
                    agent_dir: Some(ctx.working_dir.clone()),
                    resume: true, // Resume the builder session
                    chrome: false,
                    worktree: None,
                    max_turns: 30,
                    heartbeat_timeout_secs: 60,
                    add_dirs: ctx.add_dirs.clone(),
                    hooks_settings: None, // Retro doesn't need stop-gate
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

/// Build system prompt from product brief + previous learnings.
fn build_system_prompt(brief: Option<&str>, learnings: Option<&str>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(b) = brief {
        parts.push(format!("## Product Brief\n{}", b));
    }
    if let Some(l) = learnings {
        if !l.is_empty() {
            parts.push(format!("## Previous Sprint Learnings\n{}", l));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx_defaults() -> SprintPipelineContext {
        SprintPipelineContext {
            epic_code: "TEST-001".to_string(),
            sprint_number: 1,
            session_id: "sess-123".to_string(),
            stories: vec![],
            product_brief: None,
            epic_intent: "Test intent".to_string(),
            builder_agent_content: "You are a builder".to_string(),
            judge_agent_content: "You are a judge".to_string(),
            scrum_master_agent_content: "You are a scrum master".to_string(),
            working_dir: "/tmp/work".to_string(),
            model_override: None,
            effort_override: None,
            budget_override: None,
            add_dirs: vec![],
            hooks_settings: None,
            deploy_profile: "none".to_string(),
            deploy_app_id: None,
            api_url: "https://api.kapable.dev".to_string(),
            api_key: "sk_test_key".to_string(),
            product_definition_of_done: None,
            previous_learnings: None,
            serial: true,
        }
    }

    fn story(code: &str, id: &str, title: &str) -> StoryContext {
        StoryContext {
            code: code.to_string(),
            id: id.to_string(),
            title: title.to_string(),
            description: format!("Implement {}", title),
            acceptance_criteria: vec![format!("{} works", title)],
            tasks: vec![format!("Create {}", title)],
            story_json: serde_json::json!({"code": code, "id": id}),
        }
    }

    #[test]
    fn test_generate_single_story_pipeline() {
        let mut ctx = test_ctx_defaults();
        ctx.epic_code = "AUTH-001".to_string();
        ctx.stories = vec![story("ER-001", "uuid-1", "Add login")];

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

        // Build stage should be Agent, not Bash
        match &pipeline.stages[1].steps[0] {
            StepDefinition::Agent { def, .. } => {
                assert_eq!(def.model.as_deref(), Some("opus"));
                assert_eq!(def.session_id.as_deref(), Some("uuid-1"));
                assert!(!def.resume);
            }
            _ => panic!("Expected Agent step for build stage"),
        }

        // Retro stage should resume the builder session
        match &pipeline.stages[4].steps[0] {
            StepDefinition::Agent { def, .. } => {
                assert!(def.resume);
                assert_eq!(def.session_id.as_deref(), Some("uuid-1"));
            }
            _ => panic!("Expected Agent step for retro stage"),
        }
    }

    #[test]
    fn test_generate_multi_story_serial() {
        let mut ctx = test_ctx_defaults();
        ctx.serial = true;
        ctx.stories = vec![
            story("ER-001", "uuid-a", "Header"),
            story("ER-002", "uuid-b", "Footer"),
        ];

        let pipeline = generate_sprint_pipeline(&ctx);
        // source + 2 builds + judge + commit + 2 retros + output = 8
        assert_eq!(pipeline.stages.len(), 8);

        // Serial: second build depends on first build
        assert_eq!(pipeline.stages[1].depends_on, vec!["source"]);
        assert_eq!(
            pipeline.stages[2].depends_on,
            vec!["build-er-001"]
        );
    }

    #[test]
    fn test_generate_multi_story_parallel() {
        let mut ctx = test_ctx_defaults();
        ctx.serial = false;
        ctx.stories = vec![
            story("ER-001", "uuid-a", "Header"),
            story("ER-002", "uuid-b", "Footer"),
        ];

        let pipeline = generate_sprint_pipeline(&ctx);

        // Parallel: both builds depend on source
        assert_eq!(pipeline.stages[1].depends_on, vec!["source"]);
        assert_eq!(pipeline.stages[2].depends_on, vec!["source"]);
    }

    #[test]
    fn test_generate_with_deploy_chain() {
        let mut ctx = test_ctx_defaults();
        ctx.deploy_profile = "connect_app".to_string();
        ctx.deploy_app_id = Some("app-123".to_string());
        ctx.stories = vec![story("ER-001", "uuid-1", "Feature")];

        let pipeline = generate_sprint_pipeline(&ctx);
        // source + build + judge + commit + deploy + retro + output = 7
        assert_eq!(pipeline.stages.len(), 7);

        // Deploy stage exists and depends on commit-merge
        let deploy = pipeline.stages.iter().find(|s| s.id == "deploy").unwrap();
        assert_eq!(deploy.depends_on, vec!["commit-merge"]);

        // Retro depends on deploy (not commit-merge)
        let retro = pipeline.stages.iter().find(|s| s.id.starts_with("retro-")).unwrap();
        assert_eq!(retro.depends_on, vec!["deploy"]);
    }

    #[test]
    fn test_generate_without_deploy() {
        let mut ctx = test_ctx_defaults();
        ctx.deploy_profile = "none".to_string();
        ctx.stories = vec![story("ER-001", "uuid-1", "Feature")];

        let pipeline = generate_sprint_pipeline(&ctx);
        // No deploy stage
        assert!(pipeline.stages.iter().all(|s| s.id != "deploy"));
        // Retro depends on commit-merge
        let retro = pipeline.stages.iter().find(|s| s.id.starts_with("retro-")).unwrap();
        assert_eq!(retro.depends_on, vec!["commit-merge"]);
    }

    #[test]
    fn test_hooks_settings_passed_to_builders() {
        let mut ctx = test_ctx_defaults();
        ctx.hooks_settings = Some(serde_json::json!({"hooks": {"Stop": []}}));
        ctx.stories = vec![story("ER-001", "uuid-1", "Feature")];

        let pipeline = generate_sprint_pipeline(&ctx);
        match &pipeline.stages[1].steps[0] {
            StepDefinition::Agent { def, .. } => {
                assert!(def.hooks_settings.is_some());
            }
            _ => panic!("Expected Agent step"),
        }
        // Judge should NOT have hooks
        match &pipeline.stages[2].steps[0] {
            StepDefinition::Agent { def, .. } => {
                assert!(def.hooks_settings.is_none());
            }
            _ => panic!("Expected Agent step"),
        }
    }

    #[test]
    fn test_system_prompt_composition() {
        let prompt = build_system_prompt(Some("Brief content"), Some("Learning 1"));
        assert!(prompt.is_some());
        let p = prompt.unwrap();
        assert!(p.contains("Product Brief"));
        assert!(p.contains("Brief content"));
        assert!(p.contains("Previous Sprint Learnings"));
        assert!(p.contains("Learning 1"));
    }

    #[test]
    fn test_system_prompt_none_when_empty() {
        let prompt = build_system_prompt(None, None);
        assert!(prompt.is_none());
    }
}
