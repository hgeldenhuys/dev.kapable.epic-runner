use epic_runner::executor::*;
use uuid::Uuid;

#[test]
fn build_command_includes_all_flags() {
    let config = ExecutorConfig {
        model: "opus".into(),
        effort: "max".into(),
        worktree_name: "AUTH-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec!["/tmp/other".into()],
        system_prompt: Some("You are a builder".into()),
        prompt: "Build the auth system".into(),
        chrome: true,
        max_budget_usd: Some(5.0),
        allowed_tools: None,
        resume_session: false,
        agent: None,
        heartbeat_timeout_secs: 300,
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--print".to_string()));
    assert!(args.contains(&"stream-json".to_string()));
    assert!(args.contains(&"--chrome".to_string()));
    assert!(args.contains(&"--worktree".to_string()));
    assert!(args.contains(&"AUTH-001".to_string()));
    assert!(args.contains(&"--add-dir".to_string()));
    assert!(args.contains(&"/tmp/other".to_string()));
    // Budget enforcement disabled — will re-enable with production cost tracking
    // assert!(args.contains(&"--max-budget-usd".to_string()));
    // assert!(args.contains(&"5".to_string()));
}

#[test]
fn build_command_resume_uses_resume_flag() {
    let config = ExecutorConfig {
        model: "opus".into(),
        effort: "max".into(),
        worktree_name: "AUTH-001".into(),
        session_id: Uuid::new_v4(),
        repo_path: "/tmp/test-repo".into(),
        add_dirs: vec![],
        system_prompt: None,
        prompt: "Continue".into(),
        chrome: false,
        max_budget_usd: None,
        allowed_tools: None,
        resume_session: true,
        agent: None,
        heartbeat_timeout_secs: 300,
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
        max_budget_usd: None,
        allowed_tools: Some(vec!["Read".into(), "Glob".into()]),
        resume_session: false,
        agent: Some("rubber-duck".into()),
        heartbeat_timeout_secs: 120,
    };
    let cmd = build_command(&config);
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    assert!(args.contains(&"--agent".to_string()));
    assert!(args.contains(&"rubber-duck".to_string()));
    assert!(args.contains(&"--allowed-tools".to_string()));
    assert!(args.contains(&"Read,Glob".to_string()));
}
