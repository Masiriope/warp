use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use fs4::fs_std::FileExt;
use thiserror::Error;
use uuid::Uuid;

use super::{
    AgentKind, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority, TaskStatus, TaskWorkspace,
    WorkspaceId, WorkspaceSource,
};

const STORE_DIRECTORY: &str = "TaskQueue";
const STORE_VERSION: &str = "v1";
const TASKS_DIRECTORY: &str = "tasks";
const STAGING_DIRECTORY: &str = ".staging";
const WORKSPACES_FILE: &str = "workspaces.json";
const TASK_FILE: &str = "task.md";
const ATTACHMENTS_DIRECTORY: &str = "attachments";

/// Filesystem-backed task queue storage. Its root is always private Warp
/// application data; it never derives paths from a selected workspace.
#[derive(Clone, Debug)]
pub(crate) struct TaskStore {
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskList {
    pub(crate) tasks: Vec<Task>,
    pub(crate) errors: Vec<TaskLoadError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskLoadError {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

#[derive(Debug, Error)]
pub(crate) enum TaskStoreError {
    #[error("task storage I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("task storage data at {path} is invalid: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("a task must include workspace metadata before it can be persisted")]
    WorkspaceMetadataMissing,
    #[error("workspace metadata does not match task workspace ID")]
    WorkspaceMetadataMismatch,
    #[error("workspace path is unavailable: {path}")]
    WorkspacePathUnavailable { path: PathBuf },
    #[error("workspace path is not valid UTF-8 for task storage: {path:?}")]
    WorkspacePathNotUtf8 { path: PathBuf },
    #[error("unsafe task storage path component {0:?}")]
    UnsafePathComponent(String),
    #[error("unsafe attachment filename {0:?}")]
    UnsafeAttachmentFilename(String),
    #[error("duplicate attachment filename {0:?}")]
    DuplicateAttachmentFilename(String),
}

impl TaskStore {
    /// Opens a store rooted in an injected application-data directory. The
    /// versioned task queue path is appended exactly once for every caller.
    pub(crate) fn open(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().join(STORE_DIRECTORY).join(STORE_VERSION),
        }
    }

    /// Opens storage below Warp's private, active-channel application-data
    /// directory. `data_dir` is channel aware (`.warp`, `.warp-dev`, etc.).
    ///
    /// Used by task-creation UI to keep queue data out of selected workspaces.
    pub(crate) fn open_in_active_channel_data_directory() -> Self {
        Self::open(warp_core::paths::data_dir())
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn task_directory(&self, workspace_id: &WorkspaceId, task_id: &TaskId) -> PathBuf {
        self.tasks_root().join(&workspace_id.0).join(&task_id.0)
    }

    /// Persists a complete task in a staging directory and only publishes it
    /// by renaming the staged directory into the task tree after every write
    /// and the workspace index update have succeeded.
    pub(crate) fn create(&self, input: NewTaskInput) -> Result<Task, TaskStoreError> {
        let workspace = input
            .workspace
            .clone()
            .ok_or(TaskStoreError::WorkspaceMetadataMissing)?;
        if workspace.id != input.workspace_id {
            return Err(TaskStoreError::WorkspaceMetadataMismatch);
        }
        validate_path_component(&workspace.id.0)?;
        if workspace.path.to_str().is_none() {
            return Err(TaskStoreError::WorkspacePathNotUtf8 {
                path: workspace.path,
            });
        }
        if !workspace.path.is_dir() {
            return Err(TaskStoreError::WorkspacePathUnavailable {
                path: workspace.path,
            });
        }

        self.create_directory(&self.root)?;
        let staging_root = self.root.join(STAGING_DIRECTORY);
        self.create_directory(&staging_root)?;

        let id = TaskId::new(Uuid::new_v4().to_string());
        let staging = staging_root.join(format!("create-{}", id.0));
        self.create_directory(&staging)?;

        let task = Task::new(id, input);
        let result = self.write_staged_task(&staging, task, &workspace);
        if result.is_err() {
            // A failed create owns only this unique staging directory. Never
            // remove the final task directory or another writer's staging work.
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    pub(crate) fn list(&self) -> Result<TaskList, TaskStoreError> {
        let tasks_root = self.tasks_root();
        if !tasks_root.exists() {
            return Ok(TaskList {
                tasks: Vec::new(),
                errors: Vec::new(),
            });
        }

        let mut task_files = Vec::new();
        for workspace in self.read_directories(&tasks_root)? {
            for task in self.read_directories(&workspace)? {
                let task_file = task.join(TASK_FILE);
                if task_file.is_file() {
                    task_files.push(task_file);
                }
            }
        }
        task_files.sort();

        let mut tasks = Vec::new();
        let mut errors = Vec::new();
        for task_file in task_files {
            match self.load_task(&task_file) {
                Ok(task) => tasks.push(task),
                Err(error) => errors.push(TaskLoadError {
                    path: task_file,
                    reason: error.to_string(),
                }),
            }
        }
        tasks.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        Ok(TaskList { tasks, errors })
    }

    fn write_staged_task(
        &self,
        staging: &Path,
        mut task: Task,
        workspace: &TaskWorkspace,
    ) -> Result<Task, TaskStoreError> {
        let attachments_directory = staging.join(ATTACHMENTS_DIRECTORY);
        self.create_directory(&attachments_directory)?;
        let mut attachment_names = HashSet::new();
        let mut stored_attachments = Vec::with_capacity(task.attachments.len());
        for attachment in &task.attachments {
            let filename = attachment_filename(attachment)?;
            if !attachment_names.insert(filename.clone()) {
                return Err(TaskStoreError::DuplicateAttachmentFilename(filename));
            }

            let destination = attachments_directory.join(&filename);
            if let Some(bytes) = attachment.in_memory_bytes() {
                self.write_file(&destination, bytes)?;
            } else {
                fs::copy(&attachment.path, &destination).map_err(|source| TaskStoreError::Io {
                    path: attachment.path.clone(),
                    source,
                })?;
                self.sync_file(&destination)?;
            }
            let reference = canonical_attachment_reference(&filename)?;
            stored_attachments.push(TaskAttachment::new(PathBuf::from(reference)));
        }
        task.attachments = stored_attachments;

        let task_file = staging.join(TASK_FILE);
        self.write_file(&task_file, render_task_markdown(&task)?.as_bytes())?;

        // Index writes happen before publication. An index failure therefore
        // cannot leave a visible task whose workspace metadata was not saved.
        self.upsert_workspace(workspace)?;

        let workspace_root = self.tasks_root().join(&task.workspace_id.0);
        self.create_directory(&workspace_root)?;
        let final_directory = workspace_root.join(&task.id.0);
        if final_directory.exists() {
            return Err(TaskStoreError::Invalid {
                path: final_directory,
                reason: "generated task ID already exists".into(),
            });
        }
        fs::rename(staging, &final_directory).map_err(|source| TaskStoreError::Io {
            path: final_directory,
            source,
        })?;
        Ok(task)
    }

    fn load_task(&self, task_file: &Path) -> Result<Task, TaskStoreError> {
        let contents = fs::read_to_string(task_file).map_err(|source| TaskStoreError::Io {
            path: task_file.to_path_buf(),
            source,
        })?;
        let mut task = parse_task_markdown(task_file, &contents)?;
        validate_task_location(task_file, &task)?;
        if task
            .workspace
            .as_ref()
            .is_some_and(|workspace| !workspace.path.is_dir())
        {
            let workspace_path = &task.workspace.as_ref().expect("checked above").path;
            task.status = TaskStatus::AttentionRequired;
            task.attention_reason = Some(format!(
                "workspace_path_missing: {} is not available",
                workspace_path.display()
            ));
        }
        Ok(task)
    }

    fn upsert_workspace(&self, workspace: &TaskWorkspace) -> Result<(), TaskStoreError> {
        let _lock = WorkspaceIndexLock::acquire(&self.root)?;
        let index = self.root.join(WORKSPACES_FILE);
        let mut workspaces = if index.exists() {
            let contents = fs::read(&index).map_err(|source| TaskStoreError::Io {
                path: index.clone(),
                source,
            })?;
            serde_json::from_slice::<Vec<TaskWorkspace>>(&contents).map_err(|source| {
                TaskStoreError::Invalid {
                    path: index.clone(),
                    reason: format!("could not parse workspace index: {source}"),
                }
            })?
        } else {
            Vec::new()
        };
        if let Some(existing) = workspaces.iter_mut().find(|item| item.id == workspace.id) {
            *existing = workspace.clone();
        } else {
            workspaces.push(workspace.clone());
        }
        workspaces.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        let bytes =
            serde_json::to_vec_pretty(&workspaces).map_err(|source| TaskStoreError::Invalid {
                path: index.clone(),
                reason: format!("could not serialize workspace index: {source}"),
            })?;
        self.write_atomic_file(&index, &bytes)
    }

    fn write_atomic_file(&self, path: &Path, bytes: &[u8]) -> Result<(), TaskStoreError> {
        let temporary = path.with_file_name(format!("{WORKSPACES_FILE}.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            self.write_file(&temporary, bytes)?;
            replace_file_without_removing_destination(&temporary, path, replace_existing_file)
                .map_err(|source| TaskStoreError::Io {
                    path: path.to_path_buf(),
                    source,
                })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn write_file(&self, path: &Path, bytes: &[u8]) -> Result<(), TaskStoreError> {
        let mut file = File::create(path).map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        file.write_all(bytes).map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        file.sync_all().map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn sync_file(&self, path: &Path) -> Result<(), TaskStoreError> {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|source| TaskStoreError::Io {
                path: path.to_path_buf(),
                source,
            })
    }

    fn create_directory(&self, path: &Path) -> Result<(), TaskStoreError> {
        fs::create_dir_all(path).map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read_directories(&self, path: &Path) -> Result<Vec<PathBuf>, TaskStoreError> {
        let entries = fs::read_dir(path).map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut directories = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|file_type| file_type.is_dir())
                    .map(|_| entry.path())
            })
            .collect::<Vec<_>>();
        directories.sort();
        Ok(directories)
    }

    fn tasks_root(&self) -> PathBuf {
        self.root.join(TASKS_DIRECTORY)
    }
}

/// A process-wide lock scoped to one task-store root. It serializes the
/// workspace-index read/modify/write transaction without coordinating
/// unrelated stores elsewhere on disk.
struct WorkspaceIndexLock {
    file: File,
}

impl WorkspaceIndexLock {
    fn acquire(root: &Path) -> Result<Self, TaskStoreError> {
        fs::create_dir_all(root).map_err(|source| TaskStoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = root.join(format!("{WORKSPACES_FILE}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| TaskStoreError::Io {
                path: path.clone(),
                source,
            })?;
        file.lock_exclusive()
            .map_err(|source| TaskStoreError::Io { path, source })?;
        Ok(Self { file })
    }
}

impl Drop for WorkspaceIndexLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn render_task_markdown(task: &Task) -> Result<String, TaskStoreError> {
    let workspace = task
        .workspace
        .as_ref()
        .ok_or(TaskStoreError::WorkspaceMetadataMissing)?;
    let mut markdown = String::from("---\n");
    push_string_field(&mut markdown, "id", &task.id.0);
    push_string_field(&mut markdown, "workspace_id", &workspace.id.0);
    push_string_field(
        &mut markdown,
        "workspace_source",
        workspace.source.storage_name(),
    );
    push_string_field(
        &mut markdown,
        "workspace_path",
        &workspace.path.to_string_lossy(),
    );
    push_string_field(
        &mut markdown,
        "workspace_display_name",
        &workspace.display_name,
    );
    push_string_field(&mut markdown, "title", &task.title);
    push_string_field(&mut markdown, "priority", priority_name(task.priority));
    push_string_field(
        &mut markdown,
        "agent_kind",
        agent_kind_name(task.agent_kind),
    );
    push_string_field(&mut markdown, "status", status_name(task.status));
    push_optional_string_field(
        &mut markdown,
        "terminal_pane_id",
        task.terminal_pane_id.as_deref(),
    );
    push_optional_string_field(
        &mut markdown,
        "attention_reason",
        task.attention_reason.as_deref(),
    );
    markdown.push_str(&format!("created_at_ms: {}\n", task.created_at_ms));
    markdown.push_str(&format!("updated_at_ms: {}\n", task.updated_at_ms));
    markdown.push_str("schema_version: 1\nattachments:\n");
    for attachment in &task.attachments {
        let reference = canonical_attachment_reference(&attachment_filename(attachment)?)?;
        markdown.push_str("  - ");
        markdown.push_str(&quoted(&reference));
        markdown.push('\n');
    }
    markdown.push_str("---\n");
    markdown.push_str(&task.description);
    Ok(markdown)
}

fn parse_task_markdown(path: &Path, contents: &str) -> Result<Task, TaskStoreError> {
    let (front_matter, description) = contents
        .strip_prefix("---\n")
        .and_then(|contents| contents.split_once("\n---\n"))
        .ok_or_else(|| invalid(path, "expected opening and closing front matter delimiters"))?;
    let fields = parse_front_matter(path, front_matter)?;

    let schema_version = required_field(path, &fields, "schema_version")?;
    if schema_version != "1" {
        return Err(invalid(path, "schema_version must be 1"));
    }
    let workspace_id = WorkspaceId::new(required_string(path, &fields, "workspace_id")?);
    validate_path_component(&workspace_id.0)?;
    let workspace_source =
        WorkspaceSource::from_storage_name(&required_string(path, &fields, "workspace_source")?)
            .ok_or_else(|| invalid(path, "workspace_source is not supported"))?;
    let workspace = TaskWorkspace::new(
        workspace_id.clone(),
        workspace_source,
        required_string(path, &fields, "workspace_path")?,
        required_string(path, &fields, "workspace_display_name")?,
    );
    let task_id = TaskId::new(required_string(path, &fields, "id")?);
    validate_path_component(&task_id.0)?;
    let mut attachment_references = HashSet::new();
    let mut attachments = Vec::new();
    for attachment in &fields.attachments {
        let reference = validate_attachment_reference(attachment)
            .map_err(|error| invalid(path, &error.to_string()))?;
        if !attachment_references.insert(reference.clone()) {
            return Err(TaskStoreError::DuplicateAttachmentFilename(reference));
        }
        attachments.push(TaskAttachment::new(PathBuf::from(reference)));
    }
    Ok(Task {
        id: task_id,
        workspace_id,
        workspace: Some(workspace),
        title: required_string(path, &fields, "title")?,
        description: description.to_owned(),
        priority: parse_priority(path, &required_string(path, &fields, "priority")?)?,
        agent_kind: parse_agent_kind(path, &required_string(path, &fields, "agent_kind")?)?,
        attachments,
        status: parse_status(path, &required_string(path, &fields, "status")?)?,
        terminal_pane_id: optional_string(path, &fields, "terminal_pane_id")?,
        attention_reason: optional_string(path, &fields, "attention_reason")?,
        created_at_ms: parse_u64(
            path,
            &required_field(path, &fields, "created_at_ms")?,
            "created_at_ms",
        )?,
        updated_at_ms: parse_u64(
            path,
            &required_field(path, &fields, "updated_at_ms")?,
            "updated_at_ms",
        )?,
    })
}

#[derive(Default)]
struct FrontMatter {
    fields: BTreeMap<String, String>,
    attachments: Vec<String>,
}

fn parse_front_matter(path: &Path, front_matter: &str) -> Result<FrontMatter, TaskStoreError> {
    let lines = front_matter.lines().collect::<Vec<_>>();
    let mut parsed = FrontMatter::default();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line == "attachments:" {
            if parsed
                .fields
                .insert("attachments".into(), String::new())
                .is_some()
            {
                return Err(invalid(path, "duplicate attachments field"));
            }
            index += 1;
            while index < lines.len() && lines[index].starts_with("  - ") {
                parsed
                    .attachments
                    .push(parse_quoted(path, &lines[index][4..])?);
                index += 1;
            }
            continue;
        }
        let (key, value) = line
            .split_once(": ")
            .ok_or_else(|| invalid(path, "front matter fields must use `key: value`"))?;
        if !is_supported_field(key) {
            return Err(invalid(
                path,
                &format!("unsupported front matter field {key:?}"),
            ));
        }
        if parsed
            .fields
            .insert(key.to_owned(), value.to_owned())
            .is_some()
        {
            return Err(invalid(
                path,
                &format!("duplicate front matter field {key:?}"),
            ));
        }
        index += 1;
    }
    Ok(parsed)
}

fn is_supported_field(field: &str) -> bool {
    matches!(
        field,
        "id" | "workspace_id"
            | "workspace_source"
            | "workspace_path"
            | "workspace_display_name"
            | "title"
            | "priority"
            | "agent_kind"
            | "status"
            | "terminal_pane_id"
            | "attention_reason"
            | "created_at_ms"
            | "updated_at_ms"
            | "schema_version"
    )
}

fn required_field(path: &Path, fields: &FrontMatter, key: &str) -> Result<String, TaskStoreError> {
    fields
        .fields
        .get(key)
        .cloned()
        .ok_or_else(|| invalid(path, &format!("missing required field {key:?}")))
}

fn required_string(path: &Path, fields: &FrontMatter, key: &str) -> Result<String, TaskStoreError> {
    parse_quoted(path, &required_field(path, fields, key)?)
}

fn optional_string(
    path: &Path,
    fields: &FrontMatter,
    key: &str,
) -> Result<Option<String>, TaskStoreError> {
    match fields.fields.get(key).map(String::as_str) {
        Some("null") => Ok(None),
        Some(value) => parse_quoted(path, value).map(Some),
        None => Err(invalid(path, &format!("missing required field {key:?}"))),
    }
}

fn parse_quoted(path: &Path, value: &str) -> Result<String, TaskStoreError> {
    serde_json::from_str(value)
        .map_err(|source| invalid(path, &format!("expected a quoted string: {source}")))
}

fn parse_u64(path: &Path, value: &str, key: &str) -> Result<u64, TaskStoreError> {
    value
        .parse()
        .map_err(|_| invalid(path, &format!("{key} must be an unsigned integer")))
}

fn parse_priority(path: &Path, value: &str) -> Result<TaskPriority, TaskStoreError> {
    match value {
        "high" => Ok(TaskPriority::High),
        "normal" => Ok(TaskPriority::Normal),
        "low" => Ok(TaskPriority::Low),
        _ => Err(invalid(path, "priority is not supported")),
    }
}

fn parse_agent_kind(path: &Path, value: &str) -> Result<AgentKind, TaskStoreError> {
    match value {
        "codex" => Ok(AgentKind::Codex),
        "claude_code" => Ok(AgentKind::ClaudeCode),
        _ => Err(invalid(path, "agent_kind is not supported")),
    }
}

fn parse_status(path: &Path, value: &str) -> Result<TaskStatus, TaskStoreError> {
    match value {
        "pending" => Ok(TaskStatus::Pending),
        "in_progress" => Ok(TaskStatus::InProgress),
        "review_required" => Ok(TaskStatus::ReviewRequired),
        "attention_required" => Ok(TaskStatus::AttentionRequired),
        "done" => Ok(TaskStatus::Done),
        _ => Err(invalid(path, "status is not supported")),
    }
}

fn priority_name(priority: TaskPriority) -> &'static str {
    match priority {
        TaskPriority::High => "high",
        TaskPriority::Normal => "normal",
        TaskPriority::Low => "low",
    }
}

fn agent_kind_name(agent_kind: AgentKind) -> &'static str {
    match agent_kind {
        AgentKind::Codex => "codex",
        AgentKind::ClaudeCode => "claude_code",
    }
}

fn status_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::ReviewRequired => "review_required",
        TaskStatus::AttentionRequired => "attention_required",
        TaskStatus::Done => "done",
    }
}

fn attachment_filename(attachment: &TaskAttachment) -> Result<String, TaskStoreError> {
    let filename = match attachment.file_name.as_deref() {
        Some(filename) => filename.to_owned(),
        None => attachment
            .path
            .file_name()
            .and_then(|filename| filename.to_str())
            .ok_or_else(|| {
                TaskStoreError::UnsafeAttachmentFilename(attachment.path.display().to_string())
            })?
            .to_owned(),
    };
    validate_attachment_filename(&filename)?;
    Ok(filename)
}

fn validate_path_component(value: &str) -> Result<(), TaskStoreError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control)
    {
        return Err(TaskStoreError::UnsafePathComponent(value.into()));
    }
    Ok(())
}

fn validate_attachment_filename(value: &str) -> Result<(), TaskStoreError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || Path::new(value).is_absolute()
        || Path::new(value).components().count() != 1
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control)
    {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    }
    Ok(())
}

fn canonical_attachment_reference(filename: &str) -> Result<String, TaskStoreError> {
    validate_attachment_filename(filename)?;
    Ok(format!("{ATTACHMENTS_DIRECTORY}/{filename}"))
}

fn validate_attachment_reference(value: &str) -> Result<String, TaskStoreError> {
    let Some((directory, filename)) = value.split_once('/') else {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    };
    if directory != ATTACHMENTS_DIRECTORY {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    }
    let canonical = canonical_attachment_reference(filename)?;
    if value != canonical {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    }
    Ok(canonical)
}

fn validate_task_location(task_file: &Path, task: &Task) -> Result<(), TaskStoreError> {
    let task_directory = task_file
        .parent()
        .ok_or_else(|| invalid(task_file, "task file has no task directory"))?;
    let workspace_directory = task_directory
        .parent()
        .ok_or_else(|| invalid(task_file, "task file has no workspace directory"))?;
    let stored_task_id = task_directory
        .file_name()
        .ok_or_else(|| invalid(task_file, "task directory has no name"))?;
    let stored_workspace_id = workspace_directory
        .file_name()
        .ok_or_else(|| invalid(task_file, "workspace directory has no name"))?;

    if stored_task_id != task.id.0.as_str() {
        return Err(invalid(
            task_file,
            "task ID does not match its parent directory",
        ));
    }
    if stored_workspace_id != task.workspace_id.0.as_str() {
        return Err(invalid(
            task_file,
            "workspace ID does not match its parent directory",
        ));
    }
    Ok(())
}

fn push_string_field(markdown: &mut String, key: &str, value: &str) {
    markdown.push_str(key);
    markdown.push_str(": ");
    markdown.push_str(&quoted(value));
    markdown.push('\n');
}

fn push_optional_string_field(markdown: &mut String, key: &str, value: Option<&str>) {
    markdown.push_str(key);
    markdown.push_str(": ");
    match value {
        Some(value) => markdown.push_str(&quoted(value)),
        None => markdown.push_str("null"),
    }
    markdown.push('\n');
}

fn quoted(value: &str) -> String {
    // JSON strings are also valid YAML scalars. They prevent front-matter
    // injection by escaping quotes, backslashes, and newlines.
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

fn invalid(path: &Path, reason: impl Into<String>) -> TaskStoreError {
    TaskStoreError::Invalid {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Replaces a durable file without deleting its current destination first.
/// This keeps readers from observing a missing index if the platform replace
/// operation fails.
fn replace_file_without_removing_destination(
    temporary: &Path,
    destination: &Path,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    replace(temporary, destination)
}

#[cfg(not(windows))]
fn replace_existing_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    // POSIX rename atomically replaces an existing file on the same filesystem.
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_existing_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let temporary = null_terminated_wide(temporary);
    let destination = null_terminated_wide(destination);
    // The staging file is in the same directory as the index. MoveFileExW
    // therefore performs a same-volume replacement, and WRITE_THROUGH asks
    // Windows to flush the move before returning.
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(io::Error::other)
}

#[cfg(windows)]
fn null_terminated_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    use std::thread;

    #[test]
    fn attachment_references_are_platform_independent_canonical_strings() {
        let reference = canonical_attachment_reference("notes.txt")
            .expect("safe attachment filename should have a canonical reference");

        assert_eq!(reference, "attachments/notes.txt");
        assert!(!reference.contains('\\'));
        assert_eq!(
            validate_attachment_reference(&reference)
                .expect("canonical attachment reference should validate"),
            reference
        );
        for noncanonical_reference in [
            "attachments/./notes.txt",
            "attachments//notes.txt",
            "attachments/../notes.txt",
            "attachments\\notes.txt",
        ] {
            assert!(
                validate_attachment_reference(noncanonical_reference).is_err(),
                "{noncanonical_reference} should be rejected"
            );
        }
    }

    #[test]
    fn index_replacement_failure_keeps_the_existing_destination_readable() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let destination = directory.path().join(WORKSPACES_FILE);
        let temporary = directory.path().join(format!("{WORKSPACES_FILE}.tmp"));
        fs::write(&destination, b"previous index").expect("previous index should be written");
        fs::write(&temporary, b"replacement index").expect("replacement index should be written");

        let result = replace_file_without_removing_destination(
            &temporary,
            &destination,
            |temporary, destination| {
                assert_eq!(
                    fs::read(destination).expect("destination should remain readable"),
                    b"previous index"
                );
                assert!(temporary.exists());
                Err(io::Error::other("simulated replacement failure"))
            },
        );

        assert!(result.is_err());
        assert_eq!(
            fs::read(&destination).expect("previous index should remain readable"),
            b"previous index"
        );
        assert_eq!(
            fs::read(&temporary).expect("temporary index should remain available for cleanup"),
            b"replacement index"
        );
    }

    #[test]
    fn workspace_index_lock_serializes_two_task_store_updates() {
        let data = tempfile::tempdir().expect("data directory should be created");
        let first_workspace_path = data.path().join("first-workspace");
        let second_workspace_path = data.path().join("second-workspace");
        fs::create_dir_all(&first_workspace_path).expect("first workspace should be created");
        fs::create_dir_all(&second_workspace_path).expect("second workspace should be created");
        let first_store = TaskStore::open(data.path());
        let second_store = first_store.clone();
        let index_store = first_store.clone();
        let index_lock = WorkspaceIndexLock::acquire(first_store.root())
            .expect("test should acquire the workspace index lock");

        let first = thread::spawn(move || {
            first_store.create(
                NewTaskInput::new(WorkspaceId::new("first-workspace"), "First task")
                    .with_workspace(TaskWorkspace::new(
                        WorkspaceId::new("first-workspace"),
                        WorkspaceSource::Github,
                        first_workspace_path,
                        "First workspace",
                    )),
            )
        });
        let second = thread::spawn(move || {
            second_store.create(
                NewTaskInput::new(WorkspaceId::new("second-workspace"), "Second task")
                    .with_workspace(TaskWorkspace::new(
                        WorkspaceId::new("second-workspace"),
                        WorkspaceSource::Github,
                        second_workspace_path,
                        "Second workspace",
                    )),
            )
        });

        drop(index_lock);
        first
            .join()
            .expect("first writer should not panic")
            .expect("first writer should persist");
        second
            .join()
            .expect("second writer should not panic")
            .expect("second writer should persist");

        let workspaces = serde_json::from_slice::<Vec<TaskWorkspace>>(
            &fs::read(index_store.root().join(WORKSPACES_FILE))
                .expect("workspace index should be readable"),
        )
        .expect("workspace index should be valid JSON");
        assert_eq!(
            workspaces
                .iter()
                .map(|workspace| workspace.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["first-workspace", "second-workspace"]
        );
    }

    #[test]
    fn failed_index_replacement_cleans_only_its_own_unique_temporary_file() {
        let data = tempfile::tempdir().expect("data directory should be created");
        let store = TaskStore::open(data.path());
        fs::create_dir_all(store.root()).expect("store root should be created");
        let unrelated_temporary = store.root().join("workspaces.json.unrelated.tmp");
        fs::write(&unrelated_temporary, b"another writer").expect("unrelated temp should exist");
        let destination = store.root().join("index-is-a-directory");
        fs::create_dir(&destination).expect("replacement destination should be a directory");

        assert!(
            store
                .write_atomic_file(&destination, b"replacement")
                .is_err()
        );

        let remaining_temporary_files = fs::read_dir(store.root())
            .expect("store root should be readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert_eq!(remaining_temporary_files, vec![unrelated_temporary]);
    }
}
