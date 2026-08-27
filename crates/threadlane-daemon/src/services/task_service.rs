use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use threadlane_protocol::tasks::*;

#[derive(Clone, Default)]
pub struct TaskService {
    tasks: Arc<Mutex<HashMap<String, SupervisorTask>>>,
}

impl TaskService {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn list_tasks(&self, req: ListTasksRequest) -> Result<ListTasksResponse, String> {
        let tasks_lock = self.tasks.lock().map_err(|e| e.to_string())?;
        let tasks = tasks_lock
            .values()
            .filter(|t| t.project_path == req.project_path)
            .cloned()
            .collect();
        Ok(ListTasksResponse { tasks })
    }

    pub fn start_task(&self, req: StartTaskRequest) -> Result<SupervisorTask, String> {
        let task_id = format!("task_{}", uuid_v4_like());
        let now = chrono_iso_now();

        let task = SupervisorTask {
            task_id: task_id.clone(),
            project_path: req.project_path,
            prompt: req.prompt,
            kind: TaskKind::Task,
            status: TaskStatus::Running,
            created_at: now.clone(),
            updated_at: now,
            summary: Some("Task initialized in background".to_string()),
            error: None,
        };

        let mut tasks_lock = self.tasks.lock().map_err(|e| e.to_string())?;
        tasks_lock.insert(task_id, task.clone());

        Ok(task)
    }

    pub fn cancel_task(&self, req: CancelTaskRequest) -> Result<(), String> {
        let mut tasks_lock = self.tasks.lock().map_err(|e| e.to_string())?;
        if let Some(task) = tasks_lock.get_mut(&req.task_id) {
            task.status = TaskStatus::Cancelled;
            task.updated_at = chrono_iso_now();
            Ok(())
        } else {
            Err(format!("Task '{}' not found", req.task_id))
        }
    }
}

fn uuid_v4_like() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

fn chrono_iso_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now();
    let datetime: chrono::DateTime<chrono::Utc> = now.into();
    datetime.to_rfc3339()
}
