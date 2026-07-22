use uuid::Uuid;
use warpui::assets::asset_cache::{AssetCache, AssetSource};
use warpui::elements::{
    Border, CacheOption, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Flex, Image, MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius,
    Shrinkable,
};
use warpui::fonts::Weight;
use warpui::image_cache::ImageType;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Element, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::{DiscoveredWorkspace, NewTaskInput, TaskAttachment, TaskPriority, WorkspaceId};
use crate::appearance::Appearance;
use crate::editor::{EditorOptions, EditorView, Event as EditorEvent, TextOptions};
use crate::ui_components::blended_colors;
use crate::view_components::action_button::{ActionButton, ButtonSize, NakedTheme, PrimaryTheme};

const SUPPORTED_IMAGE_MIME_TYPES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskDialogAttachment {
    file_name: String,
    mime_type: String,
    bytes: Vec<u8>,
}

impl TaskDialogAttachment {
    pub(crate) fn file_name(&self) -> &str {
        &self.file_name
    }

    pub(crate) fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Pure, in-memory state for task creation. The conservative limits keep the
/// dialog responsive while preventing Command-V screenshots from consuming an
/// unbounded amount of process memory: up to eight images, 10 MiB each.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskDialogState {
    workspace_id: Option<WorkspaceId>,
    title: String,
    priority: TaskPriority,
    context: Option<String>,
    attachments: Vec<TaskDialogAttachment>,
    next_attachment_number: usize,
}

impl TaskDialogState {
    pub(crate) const MAX_ATTACHMENTS: usize = 8;
    pub(crate) const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

    pub(crate) fn select_workspace(&mut self, workspace_id: WorkspaceId) {
        self.workspace_id = Some(workspace_id);
    }

    pub(crate) fn workspace_id(&self) -> Option<&WorkspaceId> {
        self.workspace_id.as_ref()
    }

    pub(crate) fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    pub(crate) fn set_priority(&mut self, priority: TaskPriority) {
        self.priority = priority;
    }

    pub(crate) fn priority(&self) -> TaskPriority {
        self.priority
    }

    pub(crate) fn set_context(&mut self, context: Option<String>) {
        self.context = context.filter(|context| !context.is_empty());
    }

    pub(crate) fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    /// Keeps pasted text in the task's Markdown context rather than treating
    /// it as a terminal or chat attachment.
    pub(crate) fn append_pasted_text(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref();
        if text.is_empty() {
            return;
        }

        match &mut self.context {
            Some(context) => context.push_str(text),
            None => self.context = Some(text.to_owned()),
        }
    }

    pub(crate) fn attachments(&self) -> &[TaskDialogAttachment] {
        &self.attachments
    }

    /// Adds a normalized clipboard image entirely in memory. The filename is
    /// generated from the insertion order and MIME type so clipboard metadata
    /// never becomes a task-store path component.
    pub(crate) fn add_pasted_image(
        &mut self,
        mime_type: impl AsRef<str>,
        bytes: Vec<u8>,
    ) -> Result<(), &'static str> {
        let mime_type = mime_type.as_ref();
        if !SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type)
            || !matches_image_mime_type(mime_type, &bytes)
        {
            return Err("Only PNG, JPEG, GIF, WebP, or SVG images can be attached.");
        }
        if bytes.len() > Self::MAX_ATTACHMENT_BYTES {
            return Err("Each pasted image must be 10 MiB or smaller.");
        }
        if self.attachments.len() >= Self::MAX_ATTACHMENTS {
            return Err("A task can include at most 8 pasted images.");
        }

        self.next_attachment_number += 1;
        self.attachments.push(TaskDialogAttachment {
            file_name: format!(
                "pasted-image-{}.{}",
                self.next_attachment_number,
                file_extension_for_mime_type(mime_type)
            ),
            mime_type: mime_type.to_owned(),
            bytes,
        });
        Ok(())
    }

    pub(crate) fn remove_attachment(&mut self, index: usize) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
        }
    }

    pub(crate) fn can_submit(&self) -> bool {
        self.workspace_id.is_some() && !self.title.trim().is_empty()
    }

    pub(crate) fn into_new_task_input(self) -> Result<NewTaskInput, &'static str> {
        let Some(workspace_id) = self.workspace_id else {
            return Err("Select a workspace before creating a task.");
        };
        let title = self.title.trim();
        if title.is_empty() {
            return Err("A task title is required.");
        }

        Ok(NewTaskInput::new(workspace_id, title)
            .with_priority(self.priority)
            .with_description(self.context.unwrap_or_default())
            .with_attachments(
                self.attachments
                    .into_iter()
                    .map(|attachment| {
                        TaskAttachment::from_memory(attachment.file_name, attachment.bytes)
                    })
                    .collect(),
            ))
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TaskDialogEvent {
    Cancelled,
    Submitted(NewTaskInput),
}

#[derive(Clone, Debug)]
pub(crate) enum TaskDialogAction {
    SelectWorkspace(WorkspaceId),
    SelectPriority(TaskPriority),
    RemoveAttachment(usize),
    Submit,
    Cancel,
}

/// A task-specific modal body. It intentionally uses only Warp's native
/// controls and the platform-normalized `ClipboardContent` exposed by
/// `ViewContext`; it does not reuse terminal/chat attachment state.
pub(crate) struct TaskDialog {
    state: TaskDialogState,
    workspaces: Vec<DiscoveredWorkspace>,
    title_editor: ViewHandle<EditorView>,
    context_editor: ViewHandle<EditorView>,
    workspace_mouse_states: Vec<MouseStateHandle>,
    priority_mouse_states: [MouseStateHandle; 3],
    attachment_remove_mouse_states: Vec<MouseStateHandle>,
    thumbnail_asset_ids: Vec<String>,
    paste_error: Option<String>,
    submit_button: ViewHandle<ActionButton>,
    cancel_button: ViewHandle<ActionButton>,
}

impl TaskDialog {
    pub(crate) fn new(ctx: &mut ViewContext<Self>) -> Self {
        let title_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                single_line: true,
                text: TextOptions::ui_font_size(appearance),
                ..Default::default()
            };
            EditorView::new(options, ctx)
        });
        title_editor.update(ctx, |editor, ctx| {
            editor.set_placeholder_text("Describe the work to do", ctx);
        });
        ctx.subscribe_to_view(&title_editor, |me, _, event, ctx| {
            me.handle_title_editor_event(event, ctx);
        });

        let context_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                autogrow: true,
                soft_wrap: true,
                delegate_paste_handling: true,
                text: TextOptions::ui_font_size(appearance),
                ..Default::default()
            };
            EditorView::new(options, ctx)
        });
        context_editor.update(ctx, |editor, ctx| {
            editor.set_placeholder_text("Markdown, links, acceptance criteria…", ctx);
        });
        ctx.subscribe_to_view(&context_editor, |me, _, event, ctx| {
            me.handle_context_editor_event(event, ctx);
        });

        let submit_button = ctx.add_view(|_ctx| {
            ActionButton::new("Crear", PrimaryTheme)
                .with_size(ButtonSize::Default)
                .on_click(|ctx| ctx.dispatch_typed_action(TaskDialogAction::Submit))
        });
        let cancel_button = ctx.add_view(|_ctx| {
            ActionButton::new("Cancelar", NakedTheme)
                .with_size(ButtonSize::Default)
                .on_click(|ctx| ctx.dispatch_typed_action(TaskDialogAction::Cancel))
        });

        Self {
            state: TaskDialogState::default(),
            workspaces: Vec::new(),
            title_editor,
            context_editor,
            workspace_mouse_states: Vec::new(),
            priority_mouse_states: [
                MouseStateHandle::default(),
                MouseStateHandle::default(),
                MouseStateHandle::default(),
            ],
            attachment_remove_mouse_states: Vec::new(),
            thumbnail_asset_ids: Vec::new(),
            paste_error: None,
            submit_button,
            cancel_button,
        }
    }

    /// Resets the form each time it is opened. The current task-queue
    /// workspace is retained when it is still available, but no dialog data is
    /// persisted or written to disk before submission.
    #[allow(
        dead_code,
        reason = "Task 5 supplies the action that opens this programmatic modal entry point."
    )]
    pub(crate) fn configure(
        &mut self,
        workspaces: Vec<DiscoveredWorkspace>,
        selected_workspace_id: Option<WorkspaceId>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.workspaces = workspaces
            .into_iter()
            .filter(|workspace| workspace.is_available)
            .collect();
        self.workspace_mouse_states = self
            .workspaces
            .iter()
            .map(|_| MouseStateHandle::default())
            .collect();
        self.state = TaskDialogState::default();
        if let Some(workspace_id) = selected_workspace_id
            && self
                .workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id)
        {
            self.state.select_workspace(workspace_id);
        }
        self.attachment_remove_mouse_states.clear();
        self.thumbnail_asset_ids.clear();
        self.paste_error = None;
        self.title_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer(ctx);
        });
        self.context_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer(ctx);
        });
        self.sync_submit_button(ctx);
        ctx.notify();
    }

    fn handle_title_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Edited(_) => {
                let title = self
                    .title_editor
                    .read(ctx, |editor, ctx| editor.buffer_text(ctx));
                self.state.set_title(title);
                self.sync_submit_button(ctx);
            }
            EditorEvent::Escape => ctx.emit(TaskDialogEvent::Cancelled),
            _ => {}
        }
    }

    fn handle_context_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Edited(_) => {
                let context = self
                    .context_editor
                    .read(ctx, |editor, ctx| editor.buffer_text(ctx));
                self.state.set_context(Some(context));
            }
            EditorEvent::Paste => self.paste_context_from_clipboard(ctx),
            EditorEvent::Escape => ctx.emit(TaskDialogEvent::Cancelled),
            _ => {}
        }
    }

    fn paste_context_from_clipboard(&mut self, ctx: &mut ViewContext<Self>) {
        let clipboard = ctx.clipboard().read();
        self.paste_error = None;
        self.state.append_pasted_text(clipboard.plain_text);

        for image in clipboard.images.unwrap_or_default() {
            match self.state.add_pasted_image(&image.mime_type, image.data) {
                Ok(()) => {
                    let Some(attachment) = self.state.attachments().last() else {
                        self.paste_error = Some("No se pudo preparar la imagen pegada.".into());
                        continue;
                    };
                    let asset_id = format!("task-dialog-thumbnail-{}", Uuid::new_v4());
                    AssetCache::handle(ctx).update(ctx, |asset_cache, ctx| {
                        asset_cache.insert_raw_asset_bytes::<ImageType>(
                            asset_id.clone(),
                            attachment.bytes(),
                            ctx,
                        );
                    });
                    self.thumbnail_asset_ids.push(asset_id);
                    self.attachment_remove_mouse_states
                        .push(MouseStateHandle::default());
                }
                Err(error) => self.paste_error = Some(error.to_owned()),
            }
        }

        let context = self.state.context().unwrap_or_default().to_owned();
        self.context_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&context, ctx);
        });
        ctx.notify();
    }

    fn sync_submit_button(&mut self, ctx: &mut ViewContext<Self>) {
        let disabled = !self.state.can_submit();
        self.submit_button.update(ctx, |button, ctx| {
            button.set_disabled(disabled, ctx);
        });
    }

    fn render_label(&self, text: &str, appearance: &Appearance) -> Box<dyn Element> {
        warpui::elements::FormattedTextElement::from_str(
            text.to_owned(),
            appearance.ui_font_family(),
            13.,
        )
        .with_color(blended_colors::text_main(
            appearance.theme(),
            appearance.theme().background(),
        ))
        .with_weight(Weight::Medium)
        .finish()
    }

    fn render_text_input(
        &self,
        editor: ViewHandle<EditorView>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        Container::new(
            appearance
                .ui_builder()
                .text_input(editor)
                .with_style(UiComponentStyles {
                    background: Some(appearance.theme().surface_2().into()),
                    border_color: Some(appearance.theme().outline().into()),
                    padding: Some(Coords::uniform(8.)),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
        .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .finish()
    }

    fn render_workspace_selector(&self, appearance: &Appearance) -> Box<dyn Element> {
        let mut selector = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        if self.workspaces.is_empty() {
            selector.add_child(
                warpui::elements::FormattedTextElement::from_str(
                    "No available workspace was found.",
                    appearance.ui_font_family(),
                    13.,
                )
                .with_color(blended_colors::text_sub(
                    appearance.theme(),
                    appearance.theme().background(),
                ))
                .finish(),
            );
        }

        for (index, workspace) in self.workspaces.iter().enumerate() {
            let workspace_id = workspace.id.clone();
            let selected = self.state.workspace_id() == Some(&workspace_id);
            let label = if selected {
                format!("✓ {}", workspace.display_name)
            } else {
                workspace.display_name.clone()
            };
            selector.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .button(
                            ButtonVariant::Text,
                            self.workspace_mouse_states[index].clone(),
                        )
                        .with_centered_text_label(label.to_owned())
                        .with_style(UiComponentStyles {
                            padding: Some(Coords::uniform(8.)),
                            background: selected.then(|| appearance.theme().surface_3().into()),
                            ..Default::default()
                        })
                        .build()
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(TaskDialogAction::SelectWorkspace(
                                workspace_id.clone(),
                            ));
                        })
                        .finish(),
                )
                .with_margin_bottom(4.)
                .finish(),
            );
        }
        selector.finish()
    }

    fn render_priority_selector(&self, appearance: &Appearance) -> Box<dyn Element> {
        let priorities = [
            (TaskPriority::High, "Alta"),
            (TaskPriority::Normal, "Normal"),
            (TaskPriority::Low, "Baja"),
        ];
        let mut selector = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for (index, (priority, label)) in priorities.into_iter().enumerate() {
            let selected = self.state.priority() == priority;
            selector.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .button(
                            ButtonVariant::Text,
                            self.priority_mouse_states[index].clone(),
                        )
                        .with_centered_text_label(label.to_owned())
                        .with_style(UiComponentStyles {
                            padding: Some(Coords::uniform(8.)),
                            background: selected.then(|| appearance.theme().surface_3().into()),
                            ..Default::default()
                        })
                        .build()
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(TaskDialogAction::SelectPriority(priority));
                        })
                        .finish(),
                )
                .with_margin_right(6.)
                .finish(),
            );
        }
        selector.finish()
    }

    fn render_attachments(&self, appearance: &Appearance) -> Box<dyn Element> {
        let mut rows = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (index, attachment) in self.state.attachments().iter().enumerate() {
            let preview = Image::new(
                AssetSource::Raw {
                    id: self.thumbnail_asset_ids[index].clone(),
                },
                CacheOption::BySize,
            )
            .first_frame_preview()
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .finish();
            let remove = appearance
                .ui_builder()
                .button(
                    ButtonVariant::Text,
                    self.attachment_remove_mouse_states[index].clone(),
                )
                .with_centered_text_label("Eliminar".to_owned())
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(TaskDialogAction::RemoveAttachment(index));
                })
                .finish();
            let name = warpui::elements::FormattedTextElement::from_str(
                format!("{} · {}", attachment.file_name(), attachment.mime_type()),
                appearance.ui_font_family(),
                12.,
            )
            .with_color(blended_colors::text_sub(
                appearance.theme(),
                appearance.theme().background(),
            ))
            .finish();
            rows.add_child(
                Container::new(
                    Flex::row()
                        .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_children([
                            ConstrainedBox::new(preview)
                                .with_width(40.)
                                .with_height(40.)
                                .finish(),
                            Shrinkable::new(1., Container::new(name).with_margin_left(8.).finish())
                                .finish(),
                            remove,
                        ])
                        .finish(),
                )
                .with_margin_top(6.)
                .finish(),
            );
        }
        rows.finish()
    }
}

impl Entity for TaskDialog {
    type Event = TaskDialogEvent;
}

impl View for TaskDialog {
    fn ui_name() -> &'static str {
        "TaskDialog"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.title_editor);
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut form = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        // The workspace selector intentionally comes first: it is the only
        // required non-text field and makes task ownership explicit.
        form.add_child(self.render_label("Espacio de trabajo", appearance));
        form.add_child(
            Container::new(self.render_workspace_selector(appearance))
                .with_margin_top(6.)
                .finish(),
        );
        form.add_child(
            Container::new(self.render_label("Título", appearance))
                .with_margin_top(14.)
                .finish(),
        );
        form.add_child(
            Container::new(self.render_text_input(self.title_editor.clone(), appearance))
                .with_margin_top(6.)
                .finish(),
        );
        form.add_child(
            Container::new(self.render_label("Prioridad", appearance))
                .with_margin_top(14.)
                .finish(),
        );
        form.add_child(
            Container::new(self.render_priority_selector(appearance))
                .with_margin_top(6.)
                .finish(),
        );
        form.add_child(
            Container::new(self.render_label("Contexto", appearance))
                .with_margin_top(14.)
                .finish(),
        );
        form.add_child(
            Container::new(self.render_text_input(self.context_editor.clone(), appearance))
                .with_margin_top(6.)
                .finish(),
        );
        form.add_child(
            Container::new(
                warpui::elements::FormattedTextElement::from_str(
                    "Pega texto o capturas con ⌘V. No se suben archivos desde este diálogo.",
                    appearance.ui_font_family(),
                    12.,
                )
                .with_color(blended_colors::text_sub(
                    appearance.theme(),
                    appearance.theme().background(),
                ))
                .finish(),
            )
            .with_margin_top(6.)
            .finish(),
        );
        if let Some(error) = &self.paste_error {
            form.add_child(
                Container::new(
                    warpui::elements::FormattedTextElement::from_str(
                        error.to_owned(),
                        appearance.ui_font_family(),
                        12.,
                    )
                    .with_color(appearance.theme().ui_error_color())
                    .finish(),
                )
                .with_margin_top(6.)
                .finish(),
            );
        }
        if !self.state.attachments().is_empty() {
            form.add_child(
                Container::new(self.render_attachments(appearance))
                    .with_margin_top(8.)
                    .finish(),
            );
        }

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(form.finish())
                .with_child(
                    Container::new(
                        Flex::row()
                            .with_main_axis_alignment(MainAxisAlignment::End)
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_children([
                                ChildView::new(&self.cancel_button).finish(),
                                Container::new(ChildView::new(&self.submit_button).finish())
                                    .with_margin_left(8.)
                                    .finish(),
                            ])
                            .finish(),
                    )
                    .with_margin_top(20.)
                    .finish(),
                )
                .finish(),
        )
        .with_horizontal_padding(24.)
        .with_vertical_padding(20.)
        .finish()
    }
}

impl TypedActionView for TaskDialog {
    type Action = TaskDialogAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            TaskDialogAction::SelectWorkspace(workspace_id) => {
                self.state.select_workspace(workspace_id.clone());
                self.sync_submit_button(ctx);
                ctx.notify();
            }
            TaskDialogAction::SelectPriority(priority) => {
                self.state.set_priority(*priority);
                ctx.notify();
            }
            TaskDialogAction::RemoveAttachment(index) => {
                self.state.remove_attachment(*index);
                self.thumbnail_asset_ids.remove(*index);
                self.attachment_remove_mouse_states.remove(*index);
                ctx.notify();
            }
            TaskDialogAction::Submit => match self.state.clone().into_new_task_input() {
                Ok(input) => ctx.emit(TaskDialogEvent::Submitted(input)),
                Err(error) => {
                    self.paste_error = Some(error.to_owned());
                    self.sync_submit_button(ctx);
                    ctx.notify();
                }
            },
            TaskDialogAction::Cancel => ctx.emit(TaskDialogEvent::Cancelled),
        }
    }
}

fn file_extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "img",
    }
}

fn matches_image_mime_type(mime_type: &str, bytes: &[u8]) -> bool {
    match mime_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/svg+xml" => std::str::from_utf8(bytes).ok().is_some_and(|text| {
            let text = text.trim_start();
            text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg"))
        }),
        _ => false,
    }
}
