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
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct CreateProduct {
    pub name: String,
    pub slug: String,
    pub repo_path: String,
    pub description: Option<String>,
}

// ── Story (Backlog Item) ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Story {
    pub id: Uuid,
    pub product_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub epic_code: Option<String>,
    pub status: StoryStatus,
    pub points: Option<i32>,
    pub acceptance_criteria: Option<serde_json::Value>,
    pub file_paths: Option<serde_json::Value>,
    pub dod_checklist: Option<serde_json::Value>,
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
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
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
    Failed,
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
            SprintStatus::Failed => "failed",
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
    Resumed,
    StoryTransitioned,
    Blocked,
    ImpedimentRaised,
    Completed,
    Failed,
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
            SprintEventType::Resumed => "resumed",
            SprintEventType::StoryTransitioned => "story",
            SprintEventType::Blocked => "blocked",
            SprintEventType::ImpedimentRaised => "impediment",
            SprintEventType::Completed => "completed",
            SprintEventType::Failed => "failed",
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
