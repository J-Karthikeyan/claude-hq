//! Wire formats, ported from `backend/app/schemas.py`.
//!
//! The privacy boundary lives here, and Rust expresses it better than Pydantic
//! did: `deny_unknown_fields` is checked by generated code rather than at
//! runtime by a validator, and the tool allowlist becomes an *enum* -- an
//! unknown tool name cannot be represented at all, so there is no code path
//! where one reaches the database.

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};

/// Built-in Claude Code tools. Anything else deserialises to `Other`.
///
/// This is the key improvement over the Python: `mcp__acme_internal__query`
/// does not become a string we must remember to sanitise -- it becomes
/// `Tool::Other`, and the original text is dropped before it exists as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Tool {
    Bash, BashOutput, KillShell, Read, Write, Edit, NotebookEdit,
    Glob, Grep, Task, Agent, WebFetch, WebSearch, TodoWrite,
    ExitPlanMode, EnterPlanMode, SlashCommand, Skill, AskUserQuestion,
    Artifact, Workflow, Monitor, ToolSearch,
    Other,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Bash => "Bash", Tool::BashOutput => "BashOutput",
            Tool::KillShell => "KillShell", Tool::Read => "Read",
            Tool::Write => "Write", Tool::Edit => "Edit",
            Tool::NotebookEdit => "NotebookEdit", Tool::Glob => "Glob",
            Tool::Grep => "Grep", Tool::Task => "Task", Tool::Agent => "Agent",
            Tool::WebFetch => "WebFetch", Tool::WebSearch => "WebSearch",
            Tool::TodoWrite => "TodoWrite", Tool::ExitPlanMode => "ExitPlanMode",
            Tool::EnterPlanMode => "EnterPlanMode", Tool::SlashCommand => "SlashCommand",
            Tool::Skill => "Skill", Tool::AskUserQuestion => "AskUserQuestion",
            Tool::Artifact => "Artifact", Tool::Workflow => "Workflow",
            Tool::Monitor => "Monitor", Tool::ToolSearch => "ToolSearch",
            Tool::Other => "Other",
        }
    }
}

/// Custom deserialiser: map a known name to its variant, everything else to
/// `Other`. Unlike a `#[serde(other)]` on a unit variant, this also covers
/// names that are not valid Rust identifiers.
impl<'de> Deserialize<'de> for Tool {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "Bash" => Tool::Bash, "BashOutput" => Tool::BashOutput,
            "KillShell" => Tool::KillShell, "Read" => Tool::Read,
            "Write" => Tool::Write, "Edit" => Tool::Edit,
            "NotebookEdit" => Tool::NotebookEdit, "Glob" => Tool::Glob,
            "Grep" => Tool::Grep, "Task" => Tool::Task, "Agent" => Tool::Agent,
            "WebFetch" => Tool::WebFetch, "WebSearch" => Tool::WebSearch,
            "TodoWrite" => Tool::TodoWrite, "ExitPlanMode" => Tool::ExitPlanMode,
            "EnterPlanMode" => Tool::EnterPlanMode, "SlashCommand" => Tool::SlashCommand,
            "Skill" => Tool::Skill, "AskUserQuestion" => Tool::AskUserQuestion,
            "Artifact" => Tool::Artifact, "Workflow" => Tool::Workflow,
            "Monitor" => Tool::Monitor, "ToolSearch" => Tool::ToolSearch,
            _ => Tool::Other,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCounts {
    #[serde(default)] pub input: i64,
    #[serde(default)] pub output: i64,
    #[serde(default, rename = "cacheRead")] pub cache_read: i64,
    #[serde(default, rename = "cacheCreation")] pub cache_creation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCount {
    pub name: Tool,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayStat {
    pub date: NaiveDate,
    #[serde(default)] pub prompts: i64,
    #[serde(default)] pub tools: i64,
    #[serde(default)] pub artifacts: i64,
    #[serde(default)] pub replies: i64,
    #[serde(default)] pub tokens: TokenCounts,
    #[serde(default, rename = "toolBreakdown")] pub tool_breakdown: Vec<ToolCount>,
    /// `Option` carries the opt-in directly: absent means the user did not
    /// share cost, which is different from having spent nothing.
    #[serde(default, rename = "costUSD")] pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatPayload {
    #[serde(rename = "schemaVersion")] pub schema_version: u8,
    #[serde(default, rename = "trainerName")] pub trainer_name: String,
    pub days: Vec<DayStat>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_names_cannot_reach_the_database() {
        let j = r#"{"name":"mcp__acme_internal__query","count":3}"#;
        let tc: ToolCount = serde_json::from_str(j).unwrap();
        assert_eq!(tc.name, Tool::Other);
        // The original string is gone -- not sanitised later, never stored.
        assert_eq!(tc.name.as_str(), "Other");
    }

    #[test]
    fn known_tools_survive() {
        let tc: ToolCount = serde_json::from_str(r#"{"name":"Bash","count":5}"#).unwrap();
        assert_eq!(tc.name, Tool::Bash);
    }

    #[test]
    fn unknown_fields_are_refused_so_they_cannot_leak() {
        let j = r#"{"schemaVersion":1,"days":[],
                    "folderLeaderboard":[{"folder":"/Users/me/acme-unreleased"}]}"#;
        let err = serde_json::from_str::<StatPayload>(j).unwrap_err().to_string();
        assert!(err.contains("folderLeaderboard"), "got: {err}");
    }

    #[test]
    fn a_client_cannot_submit_its_own_score() {
        let j = r#"{"schemaVersion":1,"days":[],"xp":999999}"#;
        assert!(serde_json::from_str::<StatPayload>(j).is_err());
    }

    #[test]
    fn absent_cost_is_distinct_from_zero_cost() {
        let quiet: DayStat = serde_json::from_str(r#"{"date":"2026-09-26"}"#).unwrap();
        let shared: DayStat = serde_json::from_str(r#"{"date":"2026-09-26","costUSD":0.0}"#).unwrap();
        assert_eq!(quiet.cost_usd, None);
        assert_eq!(shared.cost_usd, Some(0.0));
    }

    #[test]
    fn a_full_payload_round_trips() {
        let j = r#"{"schemaVersion":1,"trainerName":"Het","days":[
          {"date":"2026-09-26","prompts":10,"tools":30,"artifacts":1,
           "tokens":{"input":5,"output":6,"cacheRead":7,"cacheCreation":8},
           "toolBreakdown":[{"name":"Bash","count":20},{"name":"mcp__x__y","count":10}]}]}"#;
        let p: StatPayload = serde_json::from_str(j).unwrap();
        assert_eq!(p.days.len(), 1);
        assert_eq!(p.days[0].tokens.cache_read, 7);
        let names: Vec<_> = p.days[0].tool_breakdown.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Bash", "Other"]);
    }
}
