use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use warpui::{Entity, SingletonEntity};

use super::{TaskLoadError, TaskStore, TaskStoreError};

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
pub(crate) enum AgentKind {
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
pub(crate) struct TaskId(pub(crate) String);

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
}

impl TaskAttachment {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file_name: None,
        }
    }

    /// Names the stored attachment independently of its source path. Storage
    /// validates this as one safe filename before copying any bytes.
    pub(crate) fn with_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
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

    // Task 2 persists creation and loading only. Durable status-update methods
    // are intentionally deferred; a future store update must commit before it
    // mutates this in-memory state.
    pub(crate) fn mark_launched(
        &mut self,
        terminal_pane_id: impl Into<String>,
    ) -> Result<(), TaskQueueError> {
        match self.status {
            TaskStatus::Pending | TaskStatus::AttentionRequired => {
                self.terminal_pane_id = Some(terminal_pane_id.into());
                self.status = TaskStatus::InProgress;
                self.attention_reason = None;
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
        Ok(())
    }

    pub(crate) fn mark_attention_required(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<(), TaskQueueError> {
        match self.status {
            TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::ReviewRequired => {
                self.status = TaskStatus::AttentionRequired;
                self.attention_reason = Some(reason.into());
                Ok(())
            }
            status => Err(TaskQueueError::InvalidTaskTransition {
                action: "require attention",
                status,
            }),
        }
    }

    pub(crate) fn mark_done(&mut self) -> Result<(), TaskQueueError> {
        self.require_status(TaskStatus::ReviewRequired, "complete")?;
        self.status = TaskStatus::Done;
        Ok(())
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
    #[error("workspace {0:?} is not discovered")]
    WorkspaceNotFound(WorkspaceId),
    #[error("task {0:?} is not in the queue")]
    TaskNotFound(TaskId),
    #[error("task Markdown path must be absolute: {0}")]
    RelativeTaskMarkdownPath(PathBuf),
    #[error("task Markdown path must be named task.md: {0}")]
    InvalidTaskMarkdownPath(PathBuf),
}

#[derive(Default)]
pub(crate) struct TaskQueueModel {
    sources: Vec<WorkspaceRoot>,
    workspaces: Vec<DiscoveredWorkspace>,
    tasks: HashMap<TaskId, Task>,
    selected_workspace_id: Option<WorkspaceId>,
    selected_task_id: Option<TaskId>,
    initialization_error: Option<TaskQueueError>,
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

    pub(crate) fn from_home(home: Option<PathBuf>) -> Self {
        match home {
            Some(home) => Self::from_roots(default_sources(home)),
            None => Self {
                initialization_error: Some(TaskQueueError::HomeDirectoryUnavailable),
                ..Self::new()
            },
        }
    }

    pub(crate) fn initialization_error(&self) -> Option<&TaskQueueError> {
        self.initialization_error.as_ref()
    }

    pub(crate) fn discover(&mut self, sources: Vec<WorkspaceRoot>) {
        self.workspaces = discover_workspaces(&sources);
        self.sources = sources;
        if self
            .selected_workspace_id
            .as_ref()
            .is_some_and(|id| !self.workspaces.iter().any(|workspace| &workspace.id == id))
        {
            self.selected_workspace_id = None;
        }
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

    pub(crate) fn select_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<(), TaskQueueError> {
        if self
            .workspaces
            .iter()
            .any(|workspace| workspace.id == workspace_id)
        {
            self.selected_workspace_id = Some(workspace_id);
            Ok(())
        } else {
            Err(TaskQueueError::WorkspaceNotFound(workspace_id))
        }
    }

    pub(crate) fn selected_workspace_id(&self) -> Option<&WorkspaceId> {
        self.selected_workspace_id.as_ref()
    }

    pub(crate) fn insert_task(&mut self, task: Task) {
        self.tasks.insert(task.id.clone(), task);
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
        if self
            .selected_task_id
            .as_ref()
            .is_some_and(|task_id| !self.tasks.contains_key(task_id))
        {
            self.selected_task_id = None;
        }
        Ok(loaded.errors)
    }

    pub(crate) fn task(&self, task_id: &TaskId) -> Option<&Task> {
        self.tasks.get(task_id)
    }

    pub(crate) fn tasks_for_workspace(&self, workspace_id: &WorkspaceId) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|task| &task.workspace_id == workspace_id)
            .collect()
    }

    pub(crate) fn select_task(&mut self, task_id: TaskId) -> Result<(), TaskQueueError> {
        if self.tasks.contains_key(&task_id) {
            self.selected_task_id = Some(task_id);
            Ok(())
        } else {
            Err(TaskQueueError::TaskNotFound(task_id))
        }
    }

    pub(crate) fn selected_task_id(&self) -> Option<&TaskId> {
        self.selected_task_id.as_ref()
    }
}

impl Entity for TaskQueueModel {
    type Event = ();
}

impl SingletonEntity for TaskQueueModel {}

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
