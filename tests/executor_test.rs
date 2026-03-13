use epic_runner::executor::*;
use uuid::Uuid;

#[test]
fn build_command_includes_all_flags() {
    let config = ExecutorConfig {
        model: "opus".into(),
        effort: "high".into(),
        worktree_name: "AUTH-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec!["/tmp/other".into()],
        system_prompt: Some("You are a builder".into()),
        prompt: "Build the auth system".into(),
        chrome: true,
        brief: false,
        max_budget_usd: Some(5.0),
        allowed_tools: None,
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 300,
        node_id: None,
        node_label: None,
        max_turns: None,
        extra_env: vec![],
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--print".to_string()));
    assert!(args.contains(&"stream-json".to_string()));
    assert!(args.contains(&"--effort".to_string()));
    assert!(args.contains(&"high".to_string()));
    assert!(args.contains(&"--chrome".to_string()));
    assert!(args.contains(&"--worktree".to_string()));
    assert!(args.contains(&"AUTH-001".to_string()));
    assert!(args.contains(&"--add-dir".to_string()));
    assert!(args.contains(&"/tmp/other".to_string()));
    assert!(!args.contains(&"--brief".to_string()));

    // Verify CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS env var is set
    let has_git_env = cmd.as_std().get_envs().any(|(k, v)| {
        k.to_string_lossy() == "CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS"
            && v.map(|v| v.to_string_lossy().into_owned()) == Some("1".to_string())
    });
    assert!(
        has_git_env,
        "Expected CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS=1 env var"
    );
}

#[test]
fn build_command_resume_uses_resume_flag() {
    let config = ExecutorConfig {
        model: "opus".into(),
        effort: "high".into(),
        worktree_name: "AUTH-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: "Continue".into(),
        chrome: false,
        brief: false,
        max_budget_usd: None,
        allowed_tools: None,
        resume_session: true,
        agent: None,
        heartbeat_timeout_secs: 300,
        node_id: None,
        node_label: None,
        max_turns: None,
        extra_env: vec![],
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--resume".to_string()));
    assert!(!args.contains(&"--worktree".to_string()));
    assert!(!args.contains(&"--chrome".to_string()));
}

#[test]
fn build_command_with_agent() {
    let config = ExecutorConfig {
        model: "haiku".into(),
        effort: "low".into(),
        worktree_name: String::new(),
        session_id: Uuid::new_v4(),
        repo_path: ".".into(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: "Diagnose".into(),
        chrome: false,
        brief: false,
        max_budget_usd: None,
        allowed_tools: Some(vec!["Read".into(), "Glob".into()]),
        resume_session: false,
        agent: Some("rubber-duck".into()),
        heartbeat_timeout_secs: 120,
        node_id: None,
        node_label: None,
        max_turns: None,
        extra_env: vec![],
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--agent".to_string()));
    // Agent name is resolved to an absolute path (embedded → temp dir)
    let agent_arg = args
        .iter()
        .find(|a| a.contains("rubber-duck"))
        .expect("Should have an arg containing rubber-duck");
    assert!(
        agent_arg.ends_with("rubber-duck.md"),
        "Agent path should end with rubber-duck.md, got: {agent_arg}"
    );
    assert!(args.contains(&"--allowed-tools".to_string()));
    assert!(args.contains(&"Read,Glob".to_string()));
}

#[test]
fn build_command_brief_flag() {
    let config = ExecutorConfig {
        model: "sonnet".into(),
        effort: "high".into(),
        worktree_name: "FLOW-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: "Research the codebase".into(),
        chrome: false,
        brief: true,
        max_budget_usd: None,
        allowed_tools: None,
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 300,
        node_id: None,
        node_label: None,
        max_turns: None,
        extra_env: vec![],
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--brief".to_string()));
}

#[test]
fn build_command_max_turns_from_config() {
    let config = ExecutorConfig {
        model: "sonnet".into(),
        effort: "high".into(),
        worktree_name: "TEST-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: "Test max turns".into(),
        chrome: false,
        brief: false,
        max_budget_usd: None,
        allowed_tools: None,
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 300,
        node_id: None,
        node_label: None,
        max_turns: Some(15),
        extra_env: vec![],
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    // Should use the configured value, not the default 50
    let turns_idx = args.iter().position(|a| a == "--max-turns").unwrap();
    assert_eq!(args[turns_idx + 1], "15");
}

#[test]
fn build_command_max_turns_default() {
    let config = ExecutorConfig {
        model: "sonnet".into(),
        effort: "high".into(),
        worktree_name: "TEST-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: "Test default turns".into(),
        chrome: false,
        brief: false,
        max_budget_usd: None,
        allowed_tools: None,
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 300,
        node_id: None,
        node_label: None,
        max_turns: None,
        extra_env: vec![],
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    // Should fall back to default 50
    let turns_idx = args.iter().position(|a| a == "--max-turns").unwrap();
    assert_eq!(args[turns_idx + 1], "50");
}
