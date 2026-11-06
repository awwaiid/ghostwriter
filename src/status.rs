#[derive(Debug, Clone, serde::Serialize)]
pub struct GhostwriterStatus {
    pub running: bool,
    pub waiting_for_trigger: bool,
    pub processing: bool,
    pub last_activity: Option<String>,
    pub error: Option<String>,
    pub uptime_seconds: u64,
    pub executions_count: u64,
    pub current_model: String,
    pub current_prompt: String,
}

impl Default for GhostwriterStatus {
    fn default() -> Self {
        Self {
            running: false,
            waiting_for_trigger: false,
            processing: false,
            last_activity: None,
            error: None,
            uptime_seconds: 0,
            executions_count: 0,
            current_model: "claude-sonnet-4-6".to_string(),
            current_prompt: "general.json".to_string(),
        }
    }
}
