use std::ffi::OsStr;
use std::path::Path;

use super::{AgentKind, TaskQueueError};

pub(crate) fn build_launch_command(
    agent: AgentKind,
    task_markdown: &Path,
) -> Result<String, TaskQueueError> {
    if !task_markdown.is_absolute() {
        return Err(TaskQueueError::RelativeTaskMarkdownPath(
            task_markdown.to_path_buf(),
        ));
    }
    if task_markdown.file_name() != Some(OsStr::new("task.md")) {
        return Err(TaskQueueError::InvalidTaskMarkdownPath(
            task_markdown.to_path_buf(),
        ));
    }

    let prompt = format!(
        "Lee y ejecuta la tarea en {}, incluidos sus adjuntos.",
        task_markdown.display()
    );
    Ok(format!("{} {}", agent.wrapper_name(), shell_quote(&prompt)))
}

/// Quotes one argument for a POSIX shell using single-quote escaping.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
