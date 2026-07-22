use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use warpui::{Entity, ModelContext, SingletonEntity};

use super::{TaskLoadError, TaskStore, TaskStoreError};

const INTERRUPTED_TASK_LAUNCH_REASON: &str =
    "La ejecución de la tarea se interrumpió porque Warp se reinició.";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum TaskStatus {
    Pending,
    InProgress,
    ReviewRequired,
    AttentionRequired,
    Done,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum TaskPriority {
    High,
    #[default]
    Normal,
    Low,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentKind {
    #[default]
    Codex,
    ClaudeCode,
}

impl AgentKind {
    pub(crate) const fn wrapper_name(self) -> &'static str {
        match self {
            Self::Codex => "codexauto",
            Self::ClaudeCode => "claudeauto",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) enum WorkspaceSource {
    Github,
    ActiveProjects,
}

impl WorkspaceSource {
    const fn display_name(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::ActiveProjects => "Active Projects",
        }
    }

    pub(crate) const fn storage_name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::ActiveProjects => "active_projects",
        }
    }

    pub(crate) fn from_storage_name(value: &str) -> Option<Self> {
        match value {
            "github" => Some(Self::Github),
            "active_projects" => Some(Self::ActiveProjects),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WorkspaceRoot {
    pub(crate) source: WorkspaceSource,
    pub(crate) path: PathBuf,
}

impl WorkspaceRoot {
    pub(crate) fn new(source: WorkspaceSource, path: impl Into<PathBuf>) -> Self {
        Self {
            source,
            path: path.into(),
        }
    }
}

pub(crate) fn default_sources(home: impl AsRef<Path>) -> Vec<WorkspaceRoot> {
    let home = home.as_ref();
    vec![
        WorkspaceRoot::new(WorkspaceSource::Github, home.join("Documents/GitHub")),
        WorkspaceRoot::new(
            WorkspaceSource::ActiveProjects,
            home.join("Desktop/01_Proyectos_Activos"),
        ),
    ]
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct WorkspaceId(pub(crate) String);

impl WorkspaceId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn from_path(path: &Path) -> Self {
        let normalized = normalize_path(path);
        let digest = Sha256::digest(path_identity_bytes(&normalized));
        Self(format!("{digest:x}"))
    }
}

/// The durable identity and location of a workspace used by persisted tasks.
///
/// This is deliberately independent from the live discovery model so stored
/// tasks can still be loaded when their original source root is unavailable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TaskWorkspace {
    pub(crate) id: WorkspaceId,
    pub(crate) source: WorkspaceSource,
    pub(crate) path: PathBuf,
    pub(crate) display_name: String,
}

impl TaskWorkspace {
    pub(crate) fn new(
        id: WorkspaceId,
        source: WorkspaceSource,
        path: impl Into<PathBuf>,
        display_name: impl Into<String>,
    ) -> Self {
        Self {
            id,
            source,
            path: path.into(),
            display_name: display_name.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct TaskId(pub(crate) String);

impl TaskId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TaskAttachment {
    pub(crate) path: PathBuf,
    #[serde(skip)]
    pub(crate) file_name: Option<String>,
    /// Pasted task-dialog images are held in memory until the task store's
    /// single staged write publishes the task. Persisted task snapshots always
    /// have this cleared and use `path` as their attachment reference.
    #[serde(skip)]
    pub(crate) bytes: Option<Arc<[u8]>>,
}

impl TaskAttachment {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file_name: None,
            bytes: None,
        }
    }

    /// Creates an attachment from a Command-V image without creating a
    /// temporary file. `TaskStore` writes these bytes only as part of its
    /// staged, atomic task-creation transaction.
    pub(crate) fn from_memory(file_name: impl Into<String>, bytes: Arc<[u8]>) -> Self {
        Self {
            path: PathBuf::new(),
            file_name: Some(file_name.into()),
            bytes: Some(bytes),
        }
    }

    /// Names the stored attachment independently of its source path. Storage
    /// validates this as one safe filename before copying any bytes.
    pub(crate) fn with_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    pub(crate) fn in_memory_bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    /// Keeps clipboard data shared from dialog state through the event
    /// boundary. Cloning `NewTaskInput` then clones only this small Arc, never
    /// the untrusted image payload itself.
    pub(crate) fn in_memory_bytes_arc(&self) -> Option<&Arc<[u8]>> {
        self.bytes.as_ref()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct NewTaskInput {
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) workspace: Option<TaskWorkspace>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) priority: TaskPriority,
    pub(crate) agent_kind: AgentKind,
    pub(crate) attachments: Vec<TaskAttachment>,
}

impl NewTaskInput {
    pub(crate) fn new(workspace_id: WorkspaceId, title: impl Into<String>) -> Self {
        Self {
            workspace_id,
            workspace: None,
            title: title.into(),
            description: String::new(),
            priority: TaskPriority::Normal,
            agent_kind: AgentKind::Codex,
            attachments: Vec::new(),
        }
    }

    pub(crate) fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub(crate) fn with_workspace(mut self, workspace: TaskWorkspace) -> Self {
        self.workspace_id = workspace.id.clone();
        self.workspace = Some(workspace);
        self
    }

    pub(crate) fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    pub(crate) fn with_agent_kind(mut self, agent_kind: AgentKind) -> Self {
        self.agent_kind = agent_kind;
        self
    }

    pub(crate) fn with_attachments(mut self, attachments: Vec<TaskAttachment>) -> Self {
        self.attachments = attachments;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Task {
    pub(crate) id: TaskId,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) workspace: Option<TaskWorkspace>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) priority: TaskPriority,
    pub(crate) agent_kind: AgentKind,
    pub(crate) attachments: Vec<TaskAttachment>,
    pub(crate) status: TaskStatus,
    pub(crate) terminal_pane_id: Option<String>,
    pub(crate) attention_reason: Option<String>,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
}

impl Task {
    pub(crate) fn new(id: TaskId, input: NewTaskInput) -> Self {
        Self {
            id,
            workspace_id: input.workspace_id,
            workspace: input.workspace,
            title: input.title,
            description: input.description,
            priority: input.priority,
            agent_kind: input.agent_kind,
            attachments: input.attachments,
            status: TaskStatus::Pending,
            terminal_pane_id: None,
            attention_reason: None,
            created_at_ms: task_timestamp_now(),
            updated_at_ms: task_timestamp_now(),
        }
    }

    pub(crate) fn mark_launched(
        &mut self,
        terminal_pane_id: impl Into<String>,
    ) -> Result<(), TaskQueueError> {
        match self.status {
            TaskStatus::Pending | TaskStatus::AttentionRequired => {
                self.terminal_pane_id = Some(terminal_pane_id.into());
                self.status = TaskStatus::InProgress;
                self.attention_reason = None;
                self.touch();
                Ok(())
            }
            status => Err(TaskQueueError::InvalidTaskTransition {
                action: "launch",
                status,
            }),
        }
    }

    pub(crate) fn mark_command_finished(&mut self, success: bool) -> Result<(), TaskQueueError> {
        self.require_status(TaskStatus::InProgress, "record command completion")?;
        if success {
            self.status = TaskStatus::ReviewRequired;
            self.attention_reason = None;
        } else {
            self.status = TaskStatus::AttentionRequired;
            self.attention_reason = Some("The command did not complete successfully.".into());
        }
        self.terminal_pane_id = None;
        self.touch();
        Ok(())
    }

    pub(crate) fn mark_attention_required(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<(), TaskQueueError> {
        match self.status {
            TaskStatus::Pending
            | TaskStatus::InProgress
            | TaskStatus::ReviewRequired
            | TaskStatus::AttentionRequired => {
                self.status = TaskStatus::AttentionRequired;
                self.attention_reason = Some(reason.into());
                self.terminal_pane_id = None;
                self.touch();
                Ok(())
            }
            status => Err(TaskQueueError::InvalidTaskTransition {
                action: "require attention",
                status,
            }),
        }
    }

    pub(crate) fn mark_done(&mut self) -> Result<(), TaskQueueError> {
        match self.status {
            TaskStatus::ReviewRequired | TaskStatus::AttentionRequired => {
                self.status = TaskStatus::Done;
                self.touch();
                Ok(())
            }
            status => Err(TaskQueueError::InvalidTaskTransition {
                action: "complete",
                status,
            }),
        }
    }

    fn touch(&mut self) {
        self.updated_at_ms = task_timestamp_now();
    }

    fn require_status(
        &self,
        expected: TaskStatus,
        action: &'static str,
    ) -> Result<(), TaskQueueError> {
        if self.status == expected {
            Ok(())
        } else {
            Err(TaskQueueError::InvalidTaskTransition {
                action,
                status: self.status,
            })
        }
    }
}

impl From<&DiscoveredWorkspace> for TaskWorkspace {
    fn from(workspace: &DiscoveredWorkspace) -> Self {
        Self::new(
            workspace.id.clone(),
            workspace.source,
            workspace.path.clone(),
            workspace.display_name.clone(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DiscoveredWorkspace {
    pub(crate) id: WorkspaceId,
    pub(crate) source: WorkspaceSource,
    pub(crate) path: PathBuf,
    pub(crate) display_name: String,
    pub(crate) is_available: bool,
}

impl DiscoveredWorkspace {
    fn available(source: WorkspaceSource, path: PathBuf) -> Self {
        let display_name = path_display_name(&path, source);
        Self {
            id: WorkspaceId::from_path(&path),
            source,
            path,
            display_name,
            is_available: true,
        }
    }

    fn unavailable(source: WorkspaceSource, path: PathBuf) -> Self {
        let display_name = path_display_name(&path, source);
        Self {
            id: WorkspaceId::from_path(&path),
            source,
            path,
            display_name,
            is_available: false,
        }
    }

    fn from_stored_workspace(workspace: &TaskWorkspace) -> Self {
        Self {
            id: workspace.id.clone(),
            source: workspace.source,
            path: workspace.path.clone(),
            display_name: workspace.display_name.clone(),
            is_available: workspace.path.is_dir(),
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum TaskQueueError {
    #[error("could not determine the home directory for the task queue")]
    HomeDirectoryUnavailable,
    #[error("cannot {action} a task in {status:?} status")]
    InvalidTaskTransition {
        action: &'static str,
        status: TaskStatus,
    },
    #[error("task Markdown path must be absolute: {0}")]
    RelativeTaskMarkdownPath(PathBuf),
    #[error("task Markdown path must be named task.md: {0}")]
    InvalidTaskMarkdownPath(PathBuf),
}

#[derive(Debug, Error)]
pub(crate) enum TaskQueuePersistError {
    #[error("task {0:?} no longer exists in the queue")]
    TaskNotFound(TaskId),
    #[error("task {0:?} has no workspace metadata")]
    WorkspaceMetadataMissing(TaskId),
    #[error("workspace is unavailable: {path}")]
    WorkspaceUnavailable { path: PathBuf },
    #[error(transparent)]
    Transition(#[from] TaskQueueError),
    #[error(transparent)]
    Storage(#[from] TaskStoreError),
}

#[derive(Default)]
pub(crate) struct TaskQueueModel {
    sources: Vec<WorkspaceRoot>,
    workspaces: Vec<DiscoveredWorkspace>,
    tasks: HashMap<TaskId, Task>,
    store: Option<TaskStore>,
    load_errors: Vec<TaskLoadError>,
    store_load_error: Option<String>,
    initialization_error: Option<TaskQueueError>,
}

/// Signals that the durable queue changed and every workspace sidebar should
/// refresh its view of the shared task data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskQueueEvent {
    Updated,
}

impl TaskQueueModel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn from_roots(roots: Vec<WorkspaceRoot>) -> Self {
        let mut model = Self::new();
        model.discover(roots);
        model
    }

    /// Builds the durable queue from live workspace discovery plus a caller
    /// supplied store. Keeping this constructor injectable makes startup load
    /// behavior testable without coupling tests to the active channel path.
    pub(crate) fn from_roots_and_store(roots: Vec<WorkspaceRoot>, store: &TaskStore) -> Self {
        let mut model = Self::from_roots(roots);
        model.store = Some(store.clone());
        model.reload_from_store(store);
        model
    }

    pub(crate) fn from_home(home: Option<PathBuf>) -> Self {
        let store = TaskStore::open_in_active_channel_data_directory();
        match home {
            Some(home) => Self::from_roots_and_store(default_sources(home), &store),
            None => {
                let mut model = Self {
                    store: Some(store.clone()),
                    initialization_error: Some(TaskQueueError::HomeDirectoryUnavailable),
                    ..Self::new()
                };
                model.reload_from_store(&store);
                model
            }
        }
    }

    pub(crate) fn initialization_error(&self) -> Option<&TaskQueueError> {
        self.initialization_error.as_ref()
    }

    /// Individual malformed task files found during the last successful load.
    /// Valid tasks remain available even when this collection is non-empty.
    pub(crate) fn load_errors(&self) -> &[TaskLoadError] {
        &self.load_errors
    }

    /// An I/O-level load failure never makes the global model unusable. The
    /// message is retained for diagnostics while the discovered queue remains
    /// available to the UI.
    pub(crate) fn store_load_error(&self) -> Option<&str> {
        self.store_load_error.as_deref()
    }

    pub(crate) fn discover(&mut self, sources: Vec<WorkspaceRoot>) {
        self.workspaces = discover_workspaces(&sources);
        self.sources = sources;
        self.reconcile_stored_workspaces();
    }

    pub(crate) fn sources(&self) -> &[WorkspaceRoot] {
        &self.sources
    }

    pub(crate) fn workspaces(&self) -> &[DiscoveredWorkspace] {
        &self.workspaces
    }

    pub(crate) fn workspaces_for_source(
        &self,
        source: WorkspaceSource,
    ) -> Vec<&DiscoveredWorkspace> {
        self.workspaces
            .iter()
            .filter(|workspace| workspace.source == source)
            .collect()
    }

    pub(crate) fn workspace(&self, workspace_id: &WorkspaceId) -> Option<&DiscoveredWorkspace> {
        self.workspaces
            .iter()
            .find(|workspace| &workspace.id == workspace_id)
    }

    pub(crate) fn insert_task(&mut self, task: Task) {
        self.tasks.insert(task.id.clone(), task);
        self.reconcile_stored_workspaces();
    }

    /// Persists before updating the model, so a write failure cannot create an
    /// in-memory task that disappears on the next load.
    pub(crate) fn create_with_store(
        &mut self,
        store: &TaskStore,
        input: NewTaskInput,
    ) -> Result<Task, TaskStoreError> {
        let input = if input.workspace.is_some() {
            input
        } else {
            let workspace = self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == input.workspace_id)
                .ok_or(TaskStoreError::WorkspaceMetadataMissing)?;
            input.with_workspace(TaskWorkspace::from(workspace))
        };
        let task = store.create(input)?;
        self.insert_task(task.clone());
        Ok(task)
    }

    /// Persists a task and broadcasts the resulting queue change to every
    /// workspace window. Selection remains local to the submitting window.
    pub(crate) fn create_with_store_and_notify(
        &mut self,
        store: &TaskStore,
        input: NewTaskInput,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Task, TaskStoreError> {
        let task = self.create_with_store(store, input)?;
        ctx.emit(TaskQueueEvent::Updated);
        Ok(task)
    }

    /// Creates a task in Warp's private active-channel application-data store.
    /// Selection is deliberately owned by the submitting workspace window.
    pub(crate) fn create_in_active_store(
        &mut self,
        input: NewTaskInput,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Task, TaskStoreError> {
        let store = self.active_store();
        self.create_with_store_and_notify(&store, input, ctx)
    }

    /// Replaces the in-memory task snapshot with the valid entries loaded by
    /// storage. Individual malformed task files are returned for the caller to
    /// surface without hiding the remaining valid tasks.
    pub(crate) fn load_from_store(
        &mut self,
        store: &TaskStore,
    ) -> Result<Vec<TaskLoadError>, TaskStoreError> {
        let loaded = store.list()?;
        self.tasks = loaded
            .tasks
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect();
        self.load_errors = loaded.errors;
        self.reconcile_stored_workspaces();
        Ok(self.load_errors.clone())
    }

    pub(crate) fn task(&self, task_id: &TaskId) -> Option<&Task> {
        self.tasks.get(task_id)
    }

    pub(crate) fn task_markdown_path(&self, task: &Task) -> PathBuf {
        self.active_store()
            .task_markdown_path(&task.workspace_id, &task.id)
    }

    pub(crate) fn tasks_for_workspace(&self, workspace_id: &WorkspaceId) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|task| &task.workspace_id == workspace_id)
            .collect()
    }

    /// Applies a launch state transition only after replacing the durable
    /// Markdown record. A failed write leaves the model's task untouched.
    pub(crate) fn launch_with_store(
        &mut self,
        store: &TaskStore,
        task_id: &TaskId,
        agent_kind: AgentKind,
        terminal_pane_id: impl Into<String>,
    ) -> Result<Task, TaskQueuePersistError> {
        let workspace_path = self
            .tasks
            .get(task_id)
            .ok_or_else(|| TaskQueuePersistError::TaskNotFound(task_id.clone()))?
            .workspace
            .as_ref()
            .map(|workspace| workspace.path.clone())
            .ok_or_else(|| TaskQueuePersistError::WorkspaceMetadataMissing(task_id.clone()))?;
        if !workspace_path.is_dir() {
            self.update_task_with_store(store, task_id, |task| {
                task.mark_attention_required(format!(
                    "El workspace ya no está disponible: {}",
                    workspace_path.display()
                ))
            })?;
            return Err(TaskQueuePersistError::WorkspaceUnavailable {
                path: workspace_path,
            });
        }

        let terminal_pane_id = terminal_pane_id.into();
        self.update_task_with_store(store, task_id, |task| {
            task.agent_kind = agent_kind;
            task.mark_launched(terminal_pane_id)
        })
    }

    /// Persists the linked command's terminal outcome before the sidebar is
    /// allowed to render its next lifecycle state.
    pub(crate) fn finish_linked_command_with_store(
        &mut self,
        store: &TaskStore,
        task_id: &TaskId,
        success: bool,
    ) -> Result<Task, TaskQueuePersistError> {
        self.update_task_with_store(store, task_id, |task| task.mark_command_finished(success))
    }

    pub(crate) fn require_linked_task_attention_with_store(
        &mut self,
        store: &TaskStore,
        task_id: &TaskId,
        reason: impl Into<String>,
    ) -> Result<Task, TaskQueuePersistError> {
        let reason = reason.into();
        self.update_task_with_store(store, task_id, |task| task.mark_attention_required(reason))
    }

    /// Persists an explicit user completion before exposing the terminal task
    /// as done in the in-memory queue.
    pub(crate) fn mark_done_with_store(
        &mut self,
        store: &TaskStore,
        task_id: &TaskId,
    ) -> Result<Task, TaskQueuePersistError> {
        self.update_task_with_store(store, task_id, Task::mark_done)
    }

    pub(crate) fn launch_in_active_store(
        &mut self,
        task_id: &TaskId,
        agent_kind: AgentKind,
        terminal_pane_id: impl Into<String>,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Task, TaskQueuePersistError> {
        let store = self.active_store();
        let result = self.launch_with_store(&store, task_id, agent_kind, terminal_pane_id);
        if result.is_ok()
            || matches!(
                &result,
                Err(TaskQueuePersistError::WorkspaceUnavailable { .. })
            )
        {
            ctx.emit(TaskQueueEvent::Updated);
        }
        result
    }

    pub(crate) fn finish_linked_command_in_active_store(
        &mut self,
        task_id: &TaskId,
        success: bool,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Task, TaskQueuePersistError> {
        let store = self.active_store();
        let task = self.finish_linked_command_with_store(&store, task_id, success)?;
        ctx.emit(TaskQueueEvent::Updated);
        Ok(task)
    }

    pub(crate) fn require_linked_task_attention_in_active_store(
        &mut self,
        task_id: &TaskId,
        reason: impl Into<String>,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Task, TaskQueuePersistError> {
        let store = self.active_store();
        let task = self.require_linked_task_attention_with_store(&store, task_id, reason)?;
        ctx.emit(TaskQueueEvent::Updated);
        Ok(task)
    }

    pub(crate) fn mark_done_in_active_store(
        &mut self,
        task_id: &TaskId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<Task, TaskQueuePersistError> {
        let store = self.active_store();
        let task = self.mark_done_with_store(&store, task_id)?;
        ctx.emit(TaskQueueEvent::Updated);
        Ok(task)
    }

    fn update_task_with_store(
        &mut self,
        store: &TaskStore,
        task_id: &TaskId,
        mutate: impl FnOnce(&mut Task) -> Result<(), TaskQueueError>,
    ) -> Result<Task, TaskQueuePersistError> {
        let mut updated = self
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| TaskQueuePersistError::TaskNotFound(task_id.clone()))?;
        mutate(&mut updated)?;
        store.update(&updated)?;
        self.tasks.insert(task_id.clone(), updated.clone());
        Ok(updated)
    }

    fn active_store(&self) -> TaskStore {
        self.store
            .clone()
            .unwrap_or_else(TaskStore::open_in_active_channel_data_directory)
    }

    #[cfg(test)]
    pub(crate) fn set_store_for_test(&mut self, store: TaskStore) {
        self.store = Some(store);
    }

    fn reload_from_store(&mut self, store: &TaskStore) {
        match self.load_from_store(store) {
            Ok(_) => {
                self.recover_interrupted_task_launches(store);
                self.store_load_error = None;
            }
            Err(error) => self.store_load_error = Some(error.to_string()),
        }
    }

    /// A terminal-pane ID cannot survive a process restart. Startup converts
    /// those orphaned links into retryable tasks before any workspace renders.
    fn recover_interrupted_task_launches(&mut self, store: &TaskStore) {
        let mut recovery_errors = Vec::new();
        for task in self
            .tasks
            .values_mut()
            .filter(|task| task.status == TaskStatus::InProgress)
        {
            let task_path = store.task_markdown_path(&task.workspace_id, &task.id);
            // InProgress is always a valid source for this transition.
            if let Err(error) = task.mark_attention_required(INTERRUPTED_TASK_LAUNCH_REASON) {
                recovery_errors.push(TaskLoadError {
                    path: task_path,
                    reason: format!(
                        "No se pudo recuperar la tarea interrumpida tras reiniciar Warp: {error}"
                    ),
                });
                continue;
            }
            if let Err(error) = store.update(task) {
                // Keep the task retryable in this process even if the backing
                // store is temporarily unavailable. The next startup retries
                // the durable normalization rather than hiding the task.
                recovery_errors.push(TaskLoadError {
                    path: task_path,
                    reason: format!(
                        "No se pudo persistir la recuperación de una tarea interrumpida: {error}"
                    ),
                });
            }
        }
        self.load_errors.extend(recovery_errors);
    }

    fn reconcile_stored_workspaces(&mut self) {
        let stored_workspaces = self
            .tasks
            .values()
            .filter_map(|task| task.workspace.as_ref())
            .collect::<Vec<_>>();
        for stored_workspace in stored_workspaces {
            if self
                .workspaces
                .iter()
                .all(|workspace| workspace.id != stored_workspace.id)
            {
                self.workspaces
                    .push(DiscoveredWorkspace::from_stored_workspace(stored_workspace));
            }
        }
        self.workspaces.sort_by(|left, right| {
            workspace_source_order(left.source)
                .cmp(&workspace_source_order(right.source))
                .then_with(|| {
                    left.display_name
                        .to_lowercase()
                        .cmp(&right.display_name.to_lowercase())
                })
                .then_with(|| normalize_path(&left.path).cmp(&normalize_path(&right.path)))
        });
    }
}

impl Entity for TaskQueueModel {
    type Event = TaskQueueEvent;
}

impl SingletonEntity for TaskQueueModel {}

const fn workspace_source_order(source: WorkspaceSource) -> u8 {
    match source {
        WorkspaceSource::Github => 0,
        WorkspaceSource::ActiveProjects => 1,
    }
}

fn discover_workspaces(sources: &[WorkspaceRoot]) -> Vec<DiscoveredWorkspace> {
    let mut workspaces_by_source = Vec::<(WorkspaceSource, Vec<DiscoveredWorkspace>)>::new();

    for source in sources {
        let source_workspaces = match fs::read_dir(&source.path) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    entry
                        .file_type()
                        .ok()
                        .filter(|file_type| file_type.is_dir())
                        .map(|_| entry.path())
                })
                .map(|path| match path.canonicalize() {
                    Ok(path) => DiscoveredWorkspace::available(source.source, path),
                    Err(_) => DiscoveredWorkspace::unavailable(source.source, path),
                })
                .collect(),
            Err(_) => vec![DiscoveredWorkspace::unavailable(
                source.source,
                source.path.clone(),
            )],
        };

        if let Some((_, workspaces)) = workspaces_by_source
            .iter_mut()
            .find(|(workspace_source, _)| *workspace_source == source.source)
        {
            workspaces.extend(source_workspaces);
        } else {
            workspaces_by_source.push((source.source, source_workspaces));
        }
    }

    workspaces_by_source
        .into_iter()
        .flat_map(|(_, mut source_workspaces)| {
            source_workspaces.sort_by(|left, right| {
                left.display_name
                    .to_lowercase()
                    .cmp(&right.display_name.to_lowercase())
                    .then_with(|| normalize_path(&left.path).cmp(&normalize_path(&right.path)))
            });
            source_workspaces
        })
        .collect()
}

fn path_display_name(path: &Path, source: WorkspaceSource) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| OsStr::new(source.display_name()))
        .to_string_lossy()
        .into_owned()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                Some(Component::CurDir | Component::ParentDir) | None => {
                    normalized.push(component.as_os_str());
                }
            },
            Component::Normal(component) => normalized.push(component),
        }
    }

    normalized
}

#[cfg(unix)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

fn task_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(windows)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(all(not(unix), not(windows)))]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    // This fallback has deterministic UTF-8 semantics on platforms without an
    // OS-specific byte or wide-character representation exposed by the standard library.
    path.as_os_str().to_string_lossy().as_bytes().to_vec()
}
