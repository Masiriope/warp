use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

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
    /// Reserved for the UI startup wiring that is intentionally outside this
    /// storage-only task.
    #[expect(
        dead_code,
        reason = "Task queue UI startup wiring is intentionally deferred to a later task."
    )]
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
            fs::copy(&attachment.path, &destination).map_err(|source| TaskStoreError::Io {
                path: attachment.path.clone(),
                source,
            })?;
            self.sync_file(&destination)?;
            stored_attachments.push(TaskAttachment::new(
                Path::new(ATTACHMENTS_DIRECTORY).join(filename),
            ));
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
        let temporary = path.with_file_name(format!("{WORKSPACES_FILE}.tmp"));
        self.write_file(&temporary, bytes)?;

        // POSIX rename atomically replaces an existing file. Windows does not
        // replace an existing destination with `std::fs::rename`, so remove
        // the old index only after the durable temporary file is ready.
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path).map_err(|source| TaskStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        fs::rename(&temporary, path).map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
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
        let path = attachment.path.to_string_lossy();
        validate_attachment_reference(&path)?;
        markdown.push_str("  - ");
        markdown.push_str(&quoted(&path));
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
    let workspace_source =
        WorkspaceSource::from_storage_name(&required_string(path, &fields, "workspace_source")?)
            .ok_or_else(|| invalid(path, "workspace_source is not supported"))?;
    let workspace = TaskWorkspace::new(
        workspace_id.clone(),
        workspace_source,
        required_string(path, &fields, "workspace_path")?,
        required_string(path, &fields, "workspace_display_name")?,
    );
    let mut attachments = Vec::new();
    for attachment in &fields.attachments {
        validate_attachment_reference(attachment)
            .map_err(|error| invalid(path, &error.to_string()))?;
        attachments.push(TaskAttachment::new(attachment.clone()));
    }
    Ok(Task {
        id: TaskId::new(required_string(path, &fields, "id")?),
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

fn validate_attachment_reference(value: &str) -> Result<(), TaskStoreError> {
    let path = Path::new(value);
    let mut components = path.components();
    let Some(Component::Normal(directory)) = components.next() else {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    };
    if directory != ATTACHMENTS_DIRECTORY {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    }
    let Some(Component::Normal(filename)) = components.next() else {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    };
    if components.next().is_some() {
        return Err(TaskStoreError::UnsafeAttachmentFilename(value.into()));
    }
    let filename = filename
        .to_str()
        .ok_or_else(|| TaskStoreError::UnsafeAttachmentFilename(value.into()))?;
    validate_attachment_filename(filename)
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
