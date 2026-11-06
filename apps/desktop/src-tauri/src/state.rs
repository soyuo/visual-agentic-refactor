use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tauri::{AppHandle, Emitter};

use crate::command_error::{CommandError, CommandErrorCode};
use crate::events::{TASK_PROGRESS_EVENT, TaskProgressEvent, TaskStatus};

#[derive(Clone, Default)]
pub struct AppState {
    inner: Arc<TaskRegistry>,
}

#[derive(Default)]
struct TaskRegistry {
    next_task_id: AtomicU64,
    cancellation_flags: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

pub struct TaskControl {
    task_id: u64,
    cancellation_requested: Arc<AtomicBool>,
    registry: Weak<TaskRegistry>,
}

impl AppState {
    pub fn register_task(&self) -> Result<TaskControl, CommandError> {
        let task_id = self.inner.next_task_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        self.inner
            .cancellation_flags
            .lock()
            .map_err(|_| {
                CommandError::new(
                    CommandErrorCode::Internal,
                    "작업 상태를 초기화하지 못했습니다.",
                )
            })?
            .insert(task_id, Arc::clone(&cancellation_requested));

        Ok(TaskControl {
            task_id,
            cancellation_requested,
            registry: Arc::downgrade(&self.inner),
        })
    }

    pub fn request_cancel(&self, task_id: u64) -> Result<(), CommandError> {
        let cancellation_flags = self.inner.cancellation_flags.lock().map_err(|_| {
            CommandError::new(
                CommandErrorCode::Internal,
                "작업 상태를 확인하지 못했습니다.",
            )
        })?;
        let Some(cancellation_requested) = cancellation_flags.get(&task_id) else {
            return Err(CommandError::new(
                CommandErrorCode::InvalidInput,
                "취소할 수 있는 진행 중인 작업이 없습니다.",
            ));
        };

        cancellation_requested.store(true, Ordering::Release);
        Ok(())
    }
}

impl TaskControl {
    pub fn task_id(&self) -> u64 {
        self.task_id
    }

    pub fn is_cancel_requested(&self) -> bool {
        self.cancellation_requested.load(Ordering::Acquire)
    }

    pub fn emit_progress(
        &self,
        app: &AppHandle,
        completed: u64,
        total: Option<u64>,
        message: Option<String>,
    ) -> Result<(), CommandError> {
        self.emit(app, TaskStatus::Running, completed, total, message)
    }

    pub fn emit_status(
        &self,
        app: &AppHandle,
        status: TaskStatus,
        completed: u64,
        total: Option<u64>,
        message: Option<String>,
    ) -> Result<(), CommandError> {
        self.emit(app, status, completed, total, message)
    }

    fn emit(
        &self,
        app: &AppHandle,
        status: TaskStatus,
        completed: u64,
        total: Option<u64>,
        message: Option<String>,
    ) -> Result<(), CommandError> {
        app.emit(
            TASK_PROGRESS_EVENT,
            TaskProgressEvent {
                task_id: self.task_id,
                status,
                completed,
                total,
                message,
            },
        )
        .map_err(|_| {
            CommandError::new(
                CommandErrorCode::Internal,
                "작업 진행 상태를 UI에 전달하지 못했습니다.",
            )
        })
    }
}

impl Drop for TaskControl {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            if let Ok(mut cancellation_flags) = registry.cancellation_flags.lock() {
                cancellation_flags.remove(&self.task_id);
            }
        }
    }
}
