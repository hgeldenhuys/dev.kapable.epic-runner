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
    /// Product-level definition of done — the judge evaluates every story against these checks
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_of_done: Option<Vec<DoDCheckItem>>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// The criterion text (Given/When/Then or plain assertion)
    pub criterion: String,
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

// ── Story Task ───────────────────────────────

/// A discrete unit of work within a story, assigned to a persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryTask {
    /// What needs to be done
    pub description: String,
    /// Persona responsible: backend_engineer, frontend_engineer, qa_engineer, architect, devops
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

// ── Story Plan ──────────────────────────────────

/// The plan attached to a story before execution starts.
/// Written by the groomer, consumed by the builder, evaluated by the judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryPlan {
    /// High-level approach (1-3 sentences)
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

/// A single item in a product-level definition of done checklist.
/// The judge evaluates each story against these before marking done.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoDCheckItem {
    /// The check description (e.g. "All tests pass")
    pub check: String,
    /// How to verify (shell command or manual step)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    /// Is this check required (vs advisory)?
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
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
    /// Structured acceptance criteria — each criterion is testable and trackable.
    /// Covers happy path, empty state, AND edge cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Vec<AcceptanceCriterion>>,
    /// Structured tasks — each task has a persona, file target, and completion state.
    /// This IS the implementation plan. Ordered by dependency (architect → backend → frontend → QA).
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
        };
        write!(f, "{s}")
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
    Blocked,
    Closed,
    Abandoned,
}

impl std::fmt::Display for EpicStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EpicStatus::Active => "active",
            EpicStatus::Blocked => "blocked",
            EpicStatus::Closed => "closed",
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
    pub intent_satisfied: bool,
    pub confidence: f64,
    pub criteria_results: Vec<CriterionResult>,
    pub summary: String,
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
