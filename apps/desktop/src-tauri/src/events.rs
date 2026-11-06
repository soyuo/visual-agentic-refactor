use serde::Serialize;

pub const TASK_PROGRESS_EVENT: &str = "task-progress";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressEvent {
    pub task_id: u64,
    pub status: TaskStatus,
    pub completed: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}
