use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Product ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub repo_path: String,
    pub description: Option<String>,
    /// Short prefix for story codes, e.g. "ER" → stories become ER-001, ER-002
    #[serde(default)]
    pub story_prefix: Option<String>,
    /// Git remote URL for multi-machine portability. When set, repo_path is resolved locally.
    #[serde(default)]
    pub repo_url: Option<String>,
    /// Product brief — architecture, file map, conventions, gotchas.
    /// Injected into agent system prompts via {{product.brief}} to cut orientation cost.
    /// Auto-updated from retro learnings at sprint end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Product-level definition of done — the judge evaluates every story against these checks
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_of_done: Option<Vec<DoDCheckItem>>,
    /// Override for the default branch (main, master, develop, etc.).
    /// If not set, detect_default_branch() probes the remote at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// Deploy profile: "connect_app" (blue-green via Connect App Pipeline),
    /// "bootstrap" (Rust deploy pipeline), or "none" (CLI/non-deployable).
    /// When "none", the entire deploy chain (deploy_standby, gate_deploy_ok,
    /// judge_ab, gate_ab, promote) is skipped without failing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_profile: Option<String>,
    /// Connect App Pipeline app ID (UUID) for this product.
    /// Replaces the hard-coded DEPLOY_APP_ID env var — each product declares its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_app_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct CreateProduct {
    pub name: String,
    pub slug: String,
    pub repo_path: String,
    pub description: Option<String>,
    /// Git remote URL for multi-machine portability
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
}

// ── Acceptance Criterion ──────────────────────

/// A single testable acceptance criterion for a story.
/// Structured so agents and humans can verify completion mechanically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// The criterion text (Given/When/Then as single string, or plain assertion)
    #[serde(default)]
    pub criterion: String,
    /// Short title for the AC (used by groomer agents)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Given clause (structured GWT format from groomer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given: Option<String>,
    /// When clause (structured GWT format from groomer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Then clause (structured GWT format from groomer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<String>,
    /// How to verify this criterion (shell command, curl, manual step)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testable_by: Option<String>,
    /// Source file this criterion relates to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Approximate line number hint for the relevant code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_hint: Option<String>,
    /// Whether this criterion has been verified as passing
    #[serde(default)]
    pub verified: bool,
    /// Evidence of verification (test output, screenshot path, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

impl AcceptanceCriterion {
    /// Returns the best display text for this AC.
    /// Prefers `criterion` if set, otherwise synthesizes from GWT fields or title.
    pub fn display_text(&self) -> String {
        if !self.criterion.is_empty() {
            return self.criterion.clone();
        }
        if let (Some(g), Some(w), Some(t)) = (&self.given, &self.when, &self.then) {
            return format!("Given {} When {} Then {}", g, w, t);
        }
        if let Some(title) = &self.title {
            return title.clone();
        }
        "(no criterion text)".to_string()
    }
}

// ── Story Task ───────────────────────────────

/// A discrete unit of work within a story, assigned to a persona.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoryTask {
    /// What needs to be done
    #[serde(default)]
    pub description: String,
    /// Persona responsible: backend_engineer, frontend_engineer, qa_engineer, architect, devops
    #[serde(default)]
    pub persona: String,
    /// File path to modify
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number hint
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_hint: Option<String>,
    /// Whether this task is complete
    #[serde(default)]
    pub done: bool,
    /// Brief note on what was done (filled on completion)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Deserialize acceptance criteria that can be either plain strings or structured objects.
fn deserialize_flexible_acs<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<AcceptanceCriterion>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let opt: Option<Vec<serde_json::Value>> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(items) => {
            let mut acs = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        acs.push(AcceptanceCriterion {
                            criterion: s,
                            ..Default::default()
                        });
                    }
                    obj @ serde_json::Value::Object(_) => {
                        let ac: AcceptanceCriterion =
                            serde_json::from_value(obj).map_err(D::Error::custom)?;
                        acs.push(ac);
                    }
                    _ => {}
                }
            }
            Ok(Some(acs))
        }
    }
}

/// Deserialize tasks that can be either plain strings or structured objects.
fn deserialize_flexible_tasks<'de, D>(deserializer: D) -> Result<Option<Vec<StoryTask>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let opt: Option<Vec<serde_json::Value>> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(items) => {
            let mut tasks = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        tasks.push(StoryTask {
                            description: s,
                            ..Default::default()
                        });
                    }
                    obj @ serde_json::Value::Object(_) => {
                        let task: StoryTask =
                            serde_json::from_value(obj).map_err(D::Error::custom)?;
                        tasks.push(task);
                    }
                    _ => {}
                }
            }
            Ok(Some(tasks))
        }
    }
}

// ── Story Plan ──────────────────────────────────

/// The plan attached to a story before execution starts.
/// Written by the groomer, consumed by the builder, evaluated by the judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryPlan {
    /// High-level approach (1-3 sentences)
    #[serde(default)]
    pub approach: String,
    /// Known risks or unknowns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risks: Option<Vec<String>>,
    /// Estimated context-window turns (builder uses this for pacing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_turns: Option<i32>,
}

// ── Story Log Entry ─────────────────────────────

/// A session-level summary attached to the story after execution.
/// Each sprint attempt on a story produces one log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryLogEntry {
    /// What happened in this session (1-3 sentences)
    #[serde(default)]
    pub summary: String,
    /// Claude Code session ID (for transcript lookup)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Sprint that produced this entry
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprint_id: Option<String>,
    /// When this entry was created
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

// ── Action Item ─────────────────────────────────

/// A follow-up action discovered during retro or judge evaluation.
/// Links back to the story that surfaced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionItem {
    /// What needs to be done
    #[serde(default)]
    pub description: String,
    /// Story code this was discovered from (e.g. "ER-042")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_story: Option<String>,
    /// Current status: open, done, wont_do
    #[serde(default = "default_action_item_status")]
    pub status: String,
    /// Which ceremony surfaced this: retro, judge, builder
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_from: Option<String>,
}

fn default_action_item_status() -> String {
    "open".to_string()
}

// ── Definition of Done Check Item ───────────────

/// A single item in a product-level definition of done.
/// The judge evaluates each story/sprint against these conditional rules.
///
/// This is a rule engine, not a flat checklist. Each check has a condition
/// (`applies_when`) so the judge only enforces it when relevant — e.g.,
/// "cargo clippy" only runs when Rust files changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoDCheckItem {
    /// Human-readable rule name (e.g. "Unit Tests Pass")
    #[serde(default)]
    pub name: String,
    /// What this check verifies (context for the judge)
    #[serde(default)]
    pub description: String,
    /// Category: code_quality, testing, deployment, documentation, process
    #[serde(default = "default_category")]
    pub category: String,
    /// When does this check apply? If None, always applies.
    /// Patterns: "files_match:src/**/*.rs", "files_match:app/**/*.tsx",
    ///           "story_has_tag:frontend", "always"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_when: Option<String>,
    /// How to verify — shell command, agent action, or manual step
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// Hard gate (must pass) vs advisory (judge decides)
    #[serde(default = "default_true")]
    pub required: bool,
    /// Can this check be consolidated across stories in a sprint?
    /// e.g., one browser smoke test for 5 frontend stories
    #[serde(default)]
    pub consolidatable: bool,
}

fn default_true() -> bool {
    true
}

fn default_category() -> String {
    "code_quality".to_string()
}

// ── Story (Backlog Item) ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Story {
    pub id: Uuid,
    pub product_id: Uuid,
    /// Human-readable code, e.g. "ER-042" (product-scoped sequential)
    #[serde(default)]
    pub code: Option<String>,
    /// The WHAT — verb-led outcome, not a feature name.
    /// Good: "User logs in with email and password"
    /// Bad: "Login page"
    #[serde(default)]
    pub title: String,
    /// The WHY — "so that [measurable outcome]".
    /// This is the most important field for trade-off decisions.
    /// Without it, the builder doesn't know if the implementation
    /// actually delivers value vs. just compiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The WHO — "as a [specific persona]".
    /// Forces thinking about who benefits. In a BaaS platform,
    /// "as the orchestrator" vs "as a co-founder viewing the console"
    /// produce very different implementations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Additional narrative context (not the "why" — use intent for that)
    pub description: Option<String>,
    pub epic_code: Option<String>,
    pub status: StoryStatus,
    pub points: Option<i32>,
    /// Acceptance criteria — supports both plain strings and structured objects.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_flexible_acs"
    )]
    pub acceptance_criteria: Option<Vec<AcceptanceCriterion>>,
    /// Tasks — supports both plain strings and structured objects.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_flexible_tasks"
    )]
    pub tasks: Option<Vec<StoryTask>>,
    /// Story codes this depends on (e.g. ["ER-016", "ER-017"]).
    /// Must be completed before this story can start.
    /// Used by orchestrator for dependency-layer execution ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<String>>,
    /// When this story was last planned (ISO8601).
    /// Used to detect stale plans — if the codebase changed significantly
    /// since planned_at, the story should be re-planned before execution.
    #[serde(default, alias = "groomed_at")]
    pub planned_at: Option<String>,
    /// Number of sprint attempts on this story (incremented each sprint, used for retry limiting)
    #[serde(default)]
    pub attempt_count: Option<i32>,
    /// Why this story is blocked (structured reason, set when status = blocked)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// Files modified during story execution (populated by builder via git diff)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<Vec<String>>,
    /// Session-level summaries — each sprint attempt appends one entry
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_entries: Option<Vec<StoryLogEntry>>,
    /// The plan for this story (written by groomer, consumed by builder)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<StoryPlan>,
    /// Follow-up actions discovered during execution (retro, judge, builder)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_items: Option<Vec<ActionItem>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StoryStatus {
    Draft,
    Ready,
    Planned,
    InProgress,
    Done,
    Deployed,
    Blocked,
    Parked,
    Rejected,
}

impl std::fmt::Display for StoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            StoryStatus::Draft => "draft",
            StoryStatus::Ready => "ready",
            StoryStatus::Planned => "planned",
            StoryStatus::InProgress => "in_progress",
            StoryStatus::Done => "done",
            StoryStatus::Deployed => "deployed",
            StoryStatus::Blocked => "blocked",
            StoryStatus::Parked => "parked",
            StoryStatus::Rejected => "rejected",
        };
        write!(f, "{s}")
    }
}

impl StoryStatus {
    /// Whether this status makes a story eligible for automatic sprint selection.
    /// Uses a whitelist: only ready, planned, and draft stories are eligible.
    /// Blocked, parked, done, deployed, and in_progress are excluded.
    pub fn is_eligible_for_sprint(&self) -> bool {
        matches!(
            self,
            StoryStatus::Ready | StoryStatus::Planned | StoryStatus::Draft
        )
    }
}

// ── Epic ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Epic {
    pub id: Uuid,
    pub product_id: Uuid,
    pub code: String,
    pub domain: String,
    pub instance: i32,
    pub title: String,
    pub intent: String,
    pub success_criteria: Option<serde_json::Value>,
    pub status: EpicStatus,
    pub worktree_name: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EpicStatus {
    Active,
    Planned,
    Blocked,
    Closed,
    Abandoned,
    /// Legacy alias — builders sometimes write "done" instead of "closed"
    Done,
}

impl EpicStatus {
    /// Normalize legacy variants to their canonical form
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed | Self::Abandoned | Self::Done)
    }
}

impl std::fmt::Display for EpicStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EpicStatus::Active => "active",
            EpicStatus::Planned => "planned",
            EpicStatus::Blocked => "blocked",
            EpicStatus::Closed | EpicStatus::Done => "closed",
            EpicStatus::Abandoned => "abandoned",
        };
        write!(f, "{s}")
    }
}

// ── Intent + Success Criteria ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessCriteria {
    pub description: String,
    pub verification_method: String,
    pub verified: bool,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    #[serde(default)]
    pub intent_satisfied: bool,
    /// Legacy field — code-judge agent may not output this.
    /// Defaults to 0.0; evaluate_verdict now uses mission_progress instead.
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub criteria_results: Vec<CriterionResult>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// v3: Mission progress percentage (0-100)
    #[serde(default)]
    pub mission_progress: Option<f64>,
    /// v3: Whether the code is ready to deploy
    #[serde(default)]
    pub deploy_ready: Option<bool>,
    /// v3: New stories discovered during this sprint (to add to backlog)
    #[serde(default)]
    pub delta_stories: Option<Vec<DeltaStory>>,
    /// v3: List of story codes completed in this sprint
    #[serde(default)]
    pub stories_completed: Option<Vec<String>>,
    /// v3: Stories that need re-grooming (plan was wrong, scope changed).
    /// These get their grooming fields cleared so the next sprint re-grooms them.
    #[serde(default)]
    pub stories_to_regroom: Option<Vec<String>>,
    /// v4: Follow-up action items discovered during review
    #[serde(default)]
    pub action_items: Option<Vec<ActionItem>>,
    /// v4: All files changed during this sprint (from git diff)
    #[serde(default)]
    pub changed_files: Option<Vec<String>>,
    /// v5: Refined sprint goal for the NEXT sprint.
    /// First sprint inherits the epic goal; the judge can refine it for subsequent sprints
    /// based on what was accomplished and what remains.
    #[serde(default)]
    pub next_sprint_goal: Option<String>,
    /// v6: Per-story updates from the judge for incomplete stories.
    /// Contains new tasks, blocked status, and reasons — applied back to stories
    /// so the next sprint has specific guidance rather than a blind retry.
    #[serde(default)]
    pub story_updates: Option<Vec<JudgeStoryUpdate>>,
    /// v7: Provisional pass — code quality is acceptable but browser/deploy ACs
    /// cannot be verified because the app was not deployed. The orchestrator
    /// should NOT create a full implementation sprint — instead close with
    /// done_pending_deploy or create a deploy-only verification sprint.
    #[serde(default)]
    pub provisional: Option<bool>,
}

/// Judge's update for an incomplete story — adds tasks, flags blockers, explains why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeStoryUpdate {
    pub code: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub new_tasks: Option<Vec<JudgeNewTask>>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

/// A task added by the judge to an incomplete story.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeNewTask {
    pub description: String,
    #[serde(default)]
    pub persona: Option<String>,
}

/// A story discovered by the judge during sprint evaluation, to be added back to the backlog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaStory {
    pub title: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub size: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
    pub evidence: Option<String>,
    pub notes: Option<String>,
}

// ── Sprint ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sprint {
    pub id: Uuid,
    pub epic_id: Uuid,
    pub number: i32,
    pub session_id: Uuid,
    pub status: SprintStatus,
    pub goal: Option<String>,
    pub system_prompt: Option<String>,
    pub stories: Option<serde_json::Value>,
    pub ceremony_log: Option<serde_json::Value>,
    pub rubber_duck_insights: Option<serde_json::Value>,
    /// v3: Sprint velocity — {stories_planned, stories_completed, context_windows_used}
    #[serde(default)]
    pub velocity: Option<serde_json::Value>,
    /// Total cost in USD for this sprint (sum of all ceremony node costs)
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Per-ceremony cost breakdown: {"researcher": 0.96, "builder": 1.97, ...}
    #[serde(default)]
    pub ceremony_costs: Option<serde_json::Value>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Outward heartbeat — last time the orchestrator confirmed this sprint is alive.
    /// Updated every ~30s during execution. Stale heartbeats (>5 min) indicate zombie sprints.
    #[serde(default)]
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// Condensed handoff summary written after sprint completes.
    /// Contains verdict, deploy outcome, files changed, commits, stories completed, and cost.
    /// Used by build_epic_log() to construct {{epic_log}} for the next sprint's builder.
    #[serde(default)]
    pub handoff_summary: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SprintStatus {
    Planning,
    Researching,
    Grooming,
    Executing,
    Reviewing,
    Retrospecting,
    Replenishing,
    Completed,
    Cancelled,
    Blocked,
    Failed,
}

impl std::fmt::Display for SprintStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SprintStatus::Planning => "planning",
            SprintStatus::Researching => "researching",
            SprintStatus::Grooming => "grooming",
            SprintStatus::Executing => "executing",
            SprintStatus::Reviewing => "reviewing",
            SprintStatus::Retrospecting => "retrospecting",
            SprintStatus::Replenishing => "replenishing",
            SprintStatus::Completed => "completed",
            SprintStatus::Cancelled => "cancelled",
            SprintStatus::Blocked => "blocked",
            SprintStatus::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

// ── Ceremony ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeremonyResult {
    pub ceremony: CeremonyType,
    pub status: CeremonyStatus,
    pub output: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CeremonyType {
    Research,
    Groom,
    Plan,
    Execute,
    BusinessReview,
    Retro,
    Replenish,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CeremonyStatus {
    Running,
    Completed,
    Failed,
    Skipped,
}

// ── Sprint Event ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintEvent {
    pub sprint_id: Uuid,
    pub event_type: SprintEventType,
    pub node_id: Option<String>,
    pub node_label: Option<String>,
    pub summary: String,
    pub detail: Option<serde_json::Value>,
    pub cost_usd: Option<f64>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SprintEventType {
    Started,
    NodeStarted,
    NodeCompleted,
    CeremonyStarted,
    CeremonyCompleted,
    StopHookFired,
    RubberDuckTriggered,
    SupervisorResumed,
    SupervisorAborted,
    AgentMessage,
    Resumed,
    StoryTransitioned,
    Blocked,
    ImpedimentRaised,
    Completed,
    Failed,
    /// Deploy node progress step (merge, push, trigger, wait, verify)
    DeployStep,
    /// Aggregated tool use summary (replaces individual CeremonyStarted events for high-frequency tools)
    ToolUseSummary,
}

impl SprintEvent {
    pub fn event_type_str(&self) -> &str {
        match self.event_type {
            SprintEventType::Started => "started",
            SprintEventType::NodeStarted => "node_started",
            SprintEventType::NodeCompleted => "node_completed",
            SprintEventType::CeremonyStarted => "ceremony",
            SprintEventType::CeremonyCompleted => "done",
            SprintEventType::StopHookFired => "stop_hook",
            SprintEventType::RubberDuckTriggered => "rubber_duck",
            SprintEventType::SupervisorResumed => "resumed",
            SprintEventType::SupervisorAborted => "aborted",
            SprintEventType::AgentMessage => "agent_message",
            SprintEventType::Resumed => "resumed",
            SprintEventType::StoryTransitioned => "story",
            SprintEventType::Blocked => "blocked",
            SprintEventType::ImpedimentRaised => "impediment",
            SprintEventType::Completed => "completed",
            SprintEventType::Failed => "failed",
            SprintEventType::DeployStep => "deploy_step",
            SprintEventType::ToolUseSummary => "tool_use_summary",
        }
    }
}

// ── Impediment ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Impediment {
    pub id: Uuid,
    pub product_id: Uuid,
    pub blocking_epic: String,
    pub blocked_by_epic: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub status: ImpedimentStatus,
    pub raised_by_sprint: Option<Uuid>,
    pub resolved_by_sprint: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImpedimentStatus {
    Open,
    Acknowledged,
    Resolved,
    WontFix,
}

// ── Supervisor Decision ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorDecision {
    pub sprint_id: Uuid,
    pub stop_hook_count: i32,
    pub decision: SupervisorAction,
    pub reasoning: String,
    pub rubber_duck_insights: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorAction {
    Complete,
    Resume,
    ResumeWithRubberDuck,
    ResumeForTaskEnforcement,
    Abort,
    RaiseImpediment,
    EscalateToHuman,
}

// ── Rubber Duck Session ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubberDuckSession {
    pub sprint_id: Uuid,
    pub trigger_reason: String,
    pub stuck_state_summary: String,
    pub insights: Vec<String>,
    pub recommended_action: SupervisorAction,
    pub cost_usd: Option<f64>,
    pub timestamp: DateTime<Utc>,
}

// ── Backlog Item (v3) ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogItem {
    pub id: Uuid,
    pub product_id: Uuid,
    #[serde(default)]
    pub code: Option<String>,
    pub title: String,
    pub description: Option<String>,
    /// JSON array of {criterion, testable_by}
    pub acceptance_criteria: Option<serde_json::Value>,
    /// JSON array of {task, file_path, line_number}
    pub tasks: Option<serde_json::Value>,
    /// T-shirt size: xs, s, m, l, xl
    #[serde(default)]
    pub size: Option<String>,
    /// Tags for groomer matching to epic mission
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub status: BacklogItemStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BacklogItemStatus {
    Draft,
    Refined,
    Ready,
    Done,
}

impl std::fmt::Display for BacklogItemStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BacklogItemStatus::Draft => "draft",
            BacklogItemStatus::Refined => "refined",
            BacklogItemStatus::Ready => "ready",
            BacklogItemStatus::Done => "done",
        };
        write!(f, "{s}")
    }
}

// ── Sprint Assignment (v3) ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintAssignment {
    pub id: Uuid,
    pub sprint_id: Uuid,
    pub backlog_item_id: Uuid,
    pub status: AssignmentStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// What was accomplished, even if partial
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentStatus {
    Assigned,
    InProgress,
    Completed,
    Deferred,
}

impl std::fmt::Display for AssignmentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AssignmentStatus::Assigned => "assigned",
            AssignmentStatus::InProgress => "in_progress",
            AssignmentStatus::Completed => "completed",
            AssignmentStatus::Deferred => "deferred",
        };
        write!(f, "{s}")
    }
}

// ── Research Note (v5) ────────────────────────
//
// Many-to-many: a research note can be linked to multiple stories,
// and a story can reference multiple notes. Stored as full-text
// markdown with metadata for retrieval and groomer injection.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchNote {
    pub id: Uuid,
    pub product_id: Uuid,
    /// Human-readable title for the research note
    pub title: String,
    /// Full markdown content of the research document
    pub content: String,
    /// Original file path the content was loaded from (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Tags for categorization and retrieval
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Join record linking a research note to a story (many-to-many).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryResearchLink {
    pub id: Uuid,
    pub story_id: Uuid,
    pub research_note_id: Uuid,
    pub created_at: DateTime<Utc>,
}

// ── Research Artifact (v3) ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchArtifact {
    pub id: Uuid,
    pub product_id: Uuid,
    /// Optional — can be product-wide or epic-scoped
    pub epic_code: Option<String>,
    /// JSON: files, patterns, dependencies, conventions discovered
    pub content: serde_json::Value,
    /// Refresh after N sprints (default 3)
    #[serde(default = "default_staleness_ttl")]
    pub staleness_ttl_sprints: i32,
    pub created_at: DateTime<Utc>,
    pub refreshed_at: Option<DateTime<Utc>>,
}

fn default_staleness_ttl() -> i32 {
    3
}

// ── Context Capacity ──────────────────────────

/// T-shirt size to context fraction mapping for sprint capacity planning.
pub fn size_to_context_fraction(size: &str) -> f64 {
    match size.to_lowercase().as_str() {
        "xs" => 0.125, // 1/8 context window
        "s" => 0.25,   // 1/4 context window
        "m" => 0.5,    // 1/2 context window
        "l" => 1.0,    // 1 full context window
        "xl" => 2.0,   // Too big for one sprint — break down
        _ => 0.5,      // Default to medium
    }
}
