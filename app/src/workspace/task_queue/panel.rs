use std::sync::{Arc, Mutex};

use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Border, Container, CornerRadius, CrossAxisAlignment, Element, Flex, Hoverable, MainAxisSize,
    MouseStateHandle, Padding, ParentElement, Radius, Shrinkable, Text,
};
use warpui::platform::Cursor;
use warpui::{AppContext, SingletonEntity};

use super::{
    AgentKind, Task, TaskId, TaskPriority, TaskQueueModel, TaskStatus, WorkspaceId, WorkspaceSource,
};
use crate::appearance::Appearance;
use crate::workspace::action::WorkspaceAction;

const TASK_ROW_RADIUS: f32 = 4.;
const TASK_SECTION_GAP: f32 = 8.;

/// Ephemeral selection state for one workspace window's Tasks sidebar.
/// Durable tasks live in `TaskQueueModel`; never place window selection there.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskPanelSelection {
    selected_workspace_id: Option<WorkspaceId>,
    selected_task_id: Option<TaskId>,
}

impl TaskPanelSelection {
    pub(crate) fn select_workspace(&mut self, workspace_id: WorkspaceId) {
        self.selected_workspace_id = Some(workspace_id);
        self.selected_task_id = None;
    }

    pub(crate) fn select_task_in_workspace(&mut self, workspace_id: WorkspaceId, task_id: TaskId) {
        self.selected_workspace_id = Some(workspace_id);
        self.selected_task_id = Some(task_id);
    }

    pub(crate) fn selected_workspace_id(&self) -> Option<&WorkspaceId> {
        self.selected_workspace_id.as_ref()
    }

    pub(crate) fn selected_task_id(&self) -> Option<&TaskId> {
        self.selected_task_id.as_ref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TaskPresentation {
    pub(crate) priority_label: &'static str,
    pub(crate) status_label: &'static str,
}

pub(crate) fn task_presentation(priority: TaskPriority, status: TaskStatus) -> TaskPresentation {
    let priority_label = match priority {
        TaskPriority::High => "Alta",
        TaskPriority::Normal => "Normal",
        TaskPriority::Low => "Baja",
    };
    let status_label = match status {
        TaskStatus::Pending => "Pendiente",
        TaskStatus::InProgress => "En curso",
        TaskStatus::ReviewRequired => "Pendiente de revisión",
        TaskStatus::AttentionRequired => "Requiere atención",
        TaskStatus::Done => "Terminada",
    };
    TaskPresentation {
        priority_label,
        status_label,
    }
}

pub(crate) fn tasks_for_selected_workspace<'a>(
    tasks: &'a [Task],
    selected_workspace_id: Option<&WorkspaceId>,
) -> Vec<&'a Task> {
    let Some(selected_workspace_id) = selected_workspace_id else {
        return Vec::new();
    };
    tasks
        .iter()
        .filter(|task| &task.workspace_id == selected_workspace_id)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskSessionRow<T> {
    pub(crate) session: T,
    pub(crate) task_id: Option<TaskId>,
}

impl<T> TaskSessionRow<T> {
    #[cfg(test)]
    pub(crate) fn ordinary(session: T) -> Self {
        Self {
            session,
            task_id: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn linked(session: T, task_id: TaskId) -> Self {
        Self {
            session,
            task_id: Some(task_id),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LinkedTaskSession<T> {
    pub(crate) session: T,
    pub(crate) task_id: TaskId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskSessionPartition<T> {
    pub(crate) ordinary: Vec<T>,
    pub(crate) linked: Vec<LinkedTaskSession<T>>,
}

/// Only explicit task metadata participates in this partition. Ordinary Warp
/// sessions remain ordinary rows; this helper intentionally does not infer an
/// agent or task relationship from a terminal title, command, or process.
pub(crate) fn partition_task_linked_sessions<T>(
    sessions: impl IntoIterator<Item = TaskSessionRow<T>>,
) -> TaskSessionPartition<T> {
    let mut ordinary = Vec::new();
    let mut linked = Vec::new();
    for row in sessions {
        match row.task_id {
            Some(task_id) => linked.push(LinkedTaskSession {
                session: row.session,
                task_id,
            }),
            None => ordinary.push(row.session),
        }
    }
    TaskSessionPartition { ordinary, linked }
}

#[derive(Clone, Debug)]
pub(crate) struct TaskPanelActions {
    pub(crate) select: WorkspaceAction,
    pub(crate) launch_codex: WorkspaceAction,
    pub(crate) launch_claude_code: WorkspaceAction,
    pub(crate) open_linked_session: WorkspaceAction,
    pub(crate) mark_done: WorkspaceAction,
}

pub(crate) fn task_actions_for(task_id: TaskId) -> TaskPanelActions {
    TaskPanelActions {
        select: WorkspaceAction::SelectTask(task_id.clone()),
        launch_codex: WorkspaceAction::LaunchTask {
            task_id: task_id.clone(),
            agent: AgentKind::Codex,
        },
        launch_claude_code: WorkspaceAction::LaunchTask {
            task_id: task_id.clone(),
            agent: AgentKind::ClaudeCode,
        },
        open_linked_session: WorkspaceAction::OpenLinkedTaskSession(task_id.clone()),
        mark_done: WorkspaceAction::MarkTaskDone(task_id),
    }
}

pub(crate) fn render_task_queue_panel(
    selection: Arc<Mutex<TaskPanelSelection>>,
    search_query: &str,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let queue = TaskQueueModel::as_ref(app);
    let selection_snapshot = selection
        .lock()
        .map(|selection| selection.clone())
        .unwrap_or_default();
    let selected_workspace_id = selection_snapshot.selected_workspace_id();
    let selected_task_id = selection_snapshot.selected_task_id();

    let mut content = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(TASK_SECTION_GAP);

    content.add_child(render_workspace_selector(
        queue,
        selected_workspace_id,
        selection.clone(),
        app,
    ));

    let Some(selected_workspace_id) = selected_workspace_id else {
        content.add_child(render_empty_state(
            "Selecciona un espacio de trabajo para ver sus tareas.",
            app,
        ));
        return Container::new(content.finish())
            .with_padding(Padding::uniform(8.))
            .finish();
    };

    let search_query = search_query.trim().to_lowercase();
    let mut tasks = queue.tasks_for_workspace(selected_workspace_id);
    tasks.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    if !search_query.is_empty() {
        tasks.retain(|task| {
            task.title.to_lowercase().contains(&search_query)
                || task.description.to_lowercase().contains(&search_query)
        });
    }

    let mut task_rows = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(4.);
    task_rows.add_child(section_heading("Tareas", app));
    if tasks.is_empty() {
        task_rows.add_child(render_empty_state(
            if search_query.is_empty() {
                "No hay tareas para este espacio de trabajo."
            } else {
                "No hay tareas que coincidan con la búsqueda."
            },
            app,
        ));
    } else {
        for task in tasks {
            task_rows.add_child(render_task_row(
                task,
                selected_task_id == Some(&task.id),
                app,
            ));
        }
    }
    content.add_child(task_rows.finish());

    if let Some(task) = selected_task_id
        .and_then(|task_id| queue.task(task_id))
        .filter(|task| task.workspace_id == *selected_workspace_id)
    {
        let workspace_is_available = queue
            .workspace(selected_workspace_id)
            .is_some_and(|workspace| workspace.is_available);
        content.add_child(render_task_detail(task, workspace_is_available, app));
    }

    Container::new(content.finish())
        .with_padding(Padding::uniform(8.))
        .with_background(internal_colors::fg_overlay_1(theme))
        .finish()
}

fn render_workspace_selector(
    queue: &TaskQueueModel,
    selected_workspace_id: Option<&WorkspaceId>,
    selection: Arc<Mutex<TaskPanelSelection>>,
    app: &AppContext,
) -> Box<dyn Element> {
    let mut sections = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(4.);
    sections.add_child(section_heading("Espacios de trabajo", app));

    for (source, label) in [
        (WorkspaceSource::Github, "GitHub"),
        (WorkspaceSource::ActiveProjects, "01_Proyectos_Activos"),
    ] {
        sections.add_child(source_heading(label, app));
        let workspaces = queue.workspaces_for_source(source);
        if workspaces.is_empty() {
            sections.add_child(secondary_text("No se han encontrado proyectos.", app));
        } else {
            for workspace in workspaces {
                sections.add_child(render_workspace_row(
                    workspace.id.clone(),
                    workspace.display_name.clone(),
                    workspace.is_available,
                    selected_workspace_id == Some(&workspace.id),
                    selection.clone(),
                    app,
                ));
            }
        }
    }

    sections.finish()
}

fn render_workspace_row(
    workspace_id: WorkspaceId,
    display_name: String,
    is_available: bool,
    is_selected: bool,
    selection: Arc<Mutex<TaskPanelSelection>>,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let text_color = if is_available {
        theme.main_text_color(theme.background())
    } else {
        theme.sub_text_color(theme.background())
    };
    let background = is_selected.then(|| internal_colors::fg_overlay_3(theme));
    let label = if is_available {
        display_name
    } else {
        format!("{display_name} (no disponible)")
    };

    let row = Container::new(
        Text::new_inline(label, appearance.ui_font_family(), 12.)
            .with_color(text_color.into())
            .finish(),
    )
    .with_padding(Padding::uniform(0.).with_horizontal(8.).with_vertical(4.))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TASK_ROW_RADIUS)));
    let row = if let Some(background) = background {
        row.with_background(background)
    } else {
        row
    }
    .finish();

    Hoverable::new(MouseStateHandle::default(), move |_| row)
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            if let Ok(mut task_selection) = selection.lock() {
                task_selection.select_workspace(workspace_id.clone());
            }
            ctx.dispatch_typed_action(WorkspaceAction::ShowTaskQueue);
        })
        .finish()
}

fn render_task_row(task: &Task, is_selected: bool, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let presentation = task_presentation(task.priority, task.status);
    let action = task_actions_for(task.id.clone()).select;
    let title = Text::new_inline(task.title.clone(), appearance.ui_font_family(), 12.)
        .with_color(theme.main_text_color(theme.background()).into())
        .finish();
    let metadata = Text::new_inline(
        format!(
            "{} · {}",
            presentation.priority_label, presentation.status_label
        ),
        appearance.ui_font_family(),
        11.,
    )
    .with_color(task_status_color(task.status, app).into())
    .finish();
    let row = Container::new(
        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(2.)
            .with_child(title)
            .with_child(metadata)
            .finish(),
    )
    .with_padding(Padding::uniform(8.))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TASK_ROW_RADIUS)));
    let row = if is_selected {
        row.with_background(internal_colors::fg_overlay_3(theme))
            .with_border(Border::all(1.).with_border_fill(theme.accent()))
    } else {
        row
    }
    .finish();

    Hoverable::new(MouseStateHandle::default(), move |_| row)
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

fn render_task_detail(
    task: &Task,
    workspace_is_available: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let presentation = task_presentation(task.priority, task.status);
    let actions = task_actions_for(task.id.clone());
    let mut detail = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(6.);
    detail.add_child(section_heading("Detalle de tarea", app));
    detail.add_child(
        Text::new_inline(task.title.clone(), appearance.ui_font_family(), 13.)
            .with_color(theme.main_text_color(theme.background()).into())
            .finish(),
    );
    detail.add_child(secondary_text(
        format!(
            "Prioridad: {} · Estado: {}",
            presentation.priority_label, presentation.status_label
        ),
        app,
    ));
    let context_label = if task.description.trim().is_empty() {
        "Contexto: sin contexto".to_owned()
    } else {
        format!("Contexto: {}", task.description)
    };
    detail.add_child(secondary_text(context_label, app));

    let mut attachments = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(2.);
    attachments.add_child(secondary_text(
        format!("Adjuntos: {}", task.attachments.len()),
        app,
    ));
    for attachment in &task.attachments {
        attachments.add_child(secondary_text(
            format!("• {}", attachment.path.display()),
            app,
        ));
    }
    detail.add_child(attachments.finish());

    let mut buttons = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.);
    buttons.add_child(
        Shrinkable::new(
            1.,
            task_action_button(
                "Abrir con Codex",
                actions.launch_codex,
                true,
                workspace_is_available,
                app,
            ),
        )
        .finish(),
    );
    buttons.add_child(
        Shrinkable::new(
            1.,
            task_action_button(
                "Abrir con Claude Code",
                actions.launch_claude_code,
                false,
                workspace_is_available,
                app,
            ),
        )
        .finish(),
    );
    detail.add_child(buttons.finish());

    if task.terminal_pane_id.is_some() {
        detail.add_child(task_action_button(
            "Abrir sesión",
            actions.open_linked_session,
            false,
            true,
            app,
        ));
    }
    if task.status == TaskStatus::ReviewRequired {
        detail.add_child(task_action_button(
            "Marcar terminada",
            actions.mark_done,
            false,
            true,
            app,
        ));
    }

    Container::new(detail.finish())
        .with_padding(Padding::uniform(8.))
        .with_background(internal_colors::fg_overlay_2(theme))
        .with_border(Border::all(1.).with_border_fill(internal_colors::fg_overlay_3(theme)))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TASK_ROW_RADIUS)))
        .finish()
}

fn task_action_button(
    label: &'static str,
    action: WorkspaceAction,
    is_primary: bool,
    is_enabled: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let (background, text_color) = if !is_enabled {
        (
            internal_colors::fg_overlay_2(theme),
            theme.sub_text_color(theme.background()),
        )
    } else if is_primary {
        (theme.accent(), theme.background())
    } else {
        (
            internal_colors::fg_overlay_3(theme),
            theme.main_text_color(theme.background()),
        )
    };
    let button = Container::new(
        Text::new_inline(label, appearance.ui_font_family(), 11.)
            .with_color(text_color.into())
            .finish(),
    )
    .with_padding(Padding::uniform(0.).with_horizontal(8.).with_vertical(6.))
    .with_background(background)
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(TASK_ROW_RADIUS)))
    .finish();
    if !is_enabled {
        return button;
    }
    Hoverable::new(MouseStateHandle::default(), move |_| button)
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

fn section_heading(label: &'static str, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    Text::new_inline(label, appearance.ui_font_family(), 11.)
        .with_color(theme.main_text_color(theme.background()).into())
        .finish()
}

fn source_heading(label: &'static str, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    Container::new(
        Text::new_inline(label, appearance.ui_font_family(), 11.)
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
    )
    .with_padding(Padding::uniform(0.).with_top(4.).with_left(4.))
    .finish()
}

fn secondary_text(label: impl Into<String>, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    Text::new_inline(label.into(), appearance.ui_font_family(), 11.)
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish()
}

fn render_empty_state(label: &'static str, app: &AppContext) -> Box<dyn Element> {
    Container::new(secondary_text(label, app))
        .with_padding(Padding::uniform(8.))
        .finish()
}

fn task_status_color(status: TaskStatus, app: &AppContext) -> pathfinder_color::ColorU {
    let theme = Appearance::as_ref(app).theme();
    match status {
        TaskStatus::AttentionRequired => theme.ansi_fg_red(),
        TaskStatus::ReviewRequired => theme.ansi_fg_yellow(),
        TaskStatus::Done => theme.ansi_fg_green(),
        TaskStatus::Pending | TaskStatus::InProgress => {
            theme.sub_text_color(theme.background()).into()
        }
    }
}
