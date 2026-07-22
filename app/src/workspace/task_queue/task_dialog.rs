use std::sync::Arc;

use uuid::Uuid;
use warpui::assets::asset_cache::{AssetCache, AssetSource};
use warpui::elements::{
    Border, CacheOption, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CornerRadius, CrossAxisAlignment, Fill, Flex, Image, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, ParentElement, Radius, ScrollbarWidth, Shrinkable,
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

const SUPPORTED_IMAGE_MIME_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
const INVALID_IMAGE_HEADER_ERROR: &str = "The pasted image has an invalid or unsupported header.";
const IMAGE_TOO_LARGE_ERROR: &str = "The pasted image is too large to attach.";
const WORKSPACE_SELECTOR_MAX_HEIGHT: f32 = 176.;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskDialogAttachment {
    file_name: String,
    mime_type: String,
    bytes: Arc<[u8]>,
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
    pub(crate) const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
    pub(crate) const MAX_DECODED_RGBA_BYTES: u64 = 64 * 1024 * 1024;

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
        bytes: impl Into<Arc<[u8]>>,
    ) -> Result<(), &'static str> {
        let mime_type = mime_type.as_ref();
        if !SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type) {
            return Err("Only PNG, JPEG, or WebP images can be attached.");
        }
        let bytes = bytes.into();
        if bytes.len() > Self::MAX_ATTACHMENT_BYTES {
            return Err("Each pasted image must be 10 MiB or smaller.");
        }
        validate_raster_image_header(mime_type, &bytes)?;
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

    /// Builds an event payload while retaining the draft until Workspace has
    /// persisted it and closes the dialog. Attachments contain Arc<[u8]>, so
    /// this clones handles and metadata but never the clipboard image bytes.
    pub(crate) fn new_task_input_for_submission(&self) -> Result<NewTaskInput, &'static str> {
        self.clone().into_new_task_input()
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
    workspace_scroll_state: ClippedScrollStateHandle,
    form_scroll_state: ClippedScrollStateHandle,
    thumbnail_asset_slots: Vec<String>,
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

        let thumbnail_asset_namespace = Uuid::new_v4();
        let thumbnail_asset_slots = (0..TaskDialogState::MAX_ATTACHMENTS)
            .map(|slot| format!("task-dialog-thumbnail-{thumbnail_asset_namespace}-{slot}"))
            .collect();

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
            workspace_scroll_state: Default::default(),
            form_scroll_state: Default::default(),
            thumbnail_asset_slots,
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
            let Some(asset_id) = self.next_thumbnail_asset_id() else {
                // State enforces the same eight-image limit. Keep this
                // defensive branch so a UI/cache mismatch never causes an
                // unbounded new asset ID to be generated.
                self.paste_error = Some("No se pudo reservar la previsualización.".into());
                continue;
            };
            match self.state.add_pasted_image(&image.mime_type, image.data) {
                Ok(()) => {
                    let Some(attachment) = self.state.attachments().last() else {
                        self.paste_error = Some("No se pudo preparar la imagen pegada.".into());
                        continue;
                    };

                    // `add_pasted_image` performed all header and decoded-size
                    // checks before this cache insertion can trigger any image
                    // decode work.
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

    /// Returns one of the fixed, per-dialog thumbnail slots. Opening and
    /// cancelling the dialog repeatedly therefore replaces at most eight raw
    /// assets instead of allocating a new UUID-backed cache entry per paste.
    fn next_thumbnail_asset_id(&self) -> Option<String> {
        self.thumbnail_asset_slots
            .iter()
            .find(|asset_id| !self.thumbnail_asset_ids.contains(*asset_id))
            .cloned()
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

    fn render_scrollable_workspace_selector(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let selector = ClippedScrollable::vertical(
            self.workspace_scroll_state.clone(),
            self.render_workspace_selector(appearance),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        ConstrainedBox::new(selector)
            .with_max_height(WORKSPACE_SELECTOR_MAX_HEIGHT)
            .finish()
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
            Container::new(self.render_scrollable_workspace_selector(appearance))
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

        let theme = appearance.theme();
        let scrollable_form = ClippedScrollable::vertical(
            self.form_scroll_state.clone(),
            form.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                // Scroll only the fields. The footer remains in the modal so
                // Create and Cancel stay reachable even with many workspaces
                // or the maximum number of pasted-image rows.
                .with_child(Shrinkable::new(1., scrollable_form).finish())
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
            TaskDialogAction::Submit => {
                if !self.state.can_submit() {
                    self.paste_error = Some(
                        if self.state.workspace_id().is_none() {
                            "Select a workspace before creating a task."
                        } else {
                            "A task title is required."
                        }
                        .to_owned(),
                    );
                    self.sync_submit_button(ctx);
                    ctx.notify();
                    return;
                }

                // Keep the draft until Workspace successfully persists and
                // closes the modal. The submission clones only Arc handles,
                // never the raw clipboard bytes, so a storage failure can be
                // retried without losing title, context, or attachments.
                match self.state.new_task_input_for_submission() {
                    Ok(input) => ctx.emit(TaskDialogEvent::Submitted(input)),
                    Err(error) => {
                        // `can_submit` checked the same invariants above. This
                        // is retained for future validation additions.
                        self.paste_error = Some(error.to_owned());
                        self.sync_submit_button(ctx);
                        ctx.notify();
                    }
                }
            }
            TaskDialogAction::Cancel => ctx.emit(TaskDialogEvent::Cancelled),
        }
    }
}

fn file_extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "img",
    }
}

/// Validates only the inexpensive container/header fields needed to establish
/// safe dimensions. In particular, it never asks an image decoder to expand
/// attacker-controlled data before the pixel and RGBA-memory budgets hold.
fn validate_raster_image_header(mime_type: &str, bytes: &[u8]) -> Result<(), &'static str> {
    let dimensions = match mime_type {
        "image/png" => png_dimensions(bytes),
        "image/jpeg" => jpeg_dimensions(bytes),
        "image/webp" => webp_dimensions(bytes),
        _ => None,
    }
    .ok_or(INVALID_IMAGE_HEADER_ERROR)?;

    let (width, height) = dimensions;
    if width == 0 || height == 0 {
        return Err(INVALID_IMAGE_HEADER_ERROR);
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(IMAGE_TOO_LARGE_ERROR)?;
    let decoded_rgba_bytes = pixels.checked_mul(4).ok_or(IMAGE_TOO_LARGE_ERROR)?;
    if pixels > TaskDialogState::MAX_IMAGE_PIXELS
        || decoded_rgba_bytes > TaskDialogState::MAX_DECODED_RGBA_BYTES
    {
        return Err(IMAGE_TOO_LARGE_ERROR);
    }
    Ok(())
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }

    // This walks container chunks only (at most the compressed-byte cap), not
    // image data. A complete PNG must have a first IHDR, at least one IDAT,
    // and a terminal IEND chunk before it can reach the preview decoder.
    let mut offset = PNG_SIGNATURE.len();
    let mut dimensions = None;
    let mut saw_idat = false;
    while offset < bytes.len() {
        let chunk_header_end = offset.checked_add(8)?;
        if chunk_header_end > bytes.len() {
            return None;
        }
        let chunk_length = usize::try_from(u32::from_be_bytes(
            bytes[offset..offset + 4].try_into().ok()?,
        ))
        .ok()?;
        let chunk_type = &bytes[offset + 4..chunk_header_end];
        let chunk_data_start = chunk_header_end;
        let chunk_data_end = chunk_data_start.checked_add(chunk_length)?;
        let chunk_end = chunk_data_end.checked_add(4)?; // trailing CRC
        if chunk_end > bytes.len() {
            return None;
        }

        match chunk_type {
            b"IHDR"
                if offset == PNG_SIGNATURE.len() && dimensions.is_none() && chunk_length == 13 =>
            {
                dimensions = Some((
                    u32::from_be_bytes(
                        bytes[chunk_data_start..chunk_data_start + 4]
                            .try_into()
                            .ok()?,
                    ),
                    u32::from_be_bytes(
                        bytes[chunk_data_start + 4..chunk_data_start + 8]
                            .try_into()
                            .ok()?,
                    ),
                ));
            }
            b"IDAT" if dimensions.is_some() => saw_idat = true,
            b"IEND" if dimensions.is_some() && saw_idat && chunk_length == 0 => {
                return (chunk_end == bytes.len()).then_some(dimensions?);
            }
            _ if dimensions.is_none() => return None,
            _ => {}
        }
        offset = chunk_end;
    }
    None
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }

    let mut index = 2;
    let mut dimensions = None;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index] == 0xff {
            index += 1;
        }
        let marker = *bytes.get(index)?;
        index += 1;

        match marker {
            0x01 | 0xd0..=0xd7 => continue,
            0xd9 => return None,
            _ => {}
        }

        let segment_length = usize::from(u16::from_be_bytes([
            *bytes.get(index)?,
            *bytes.get(index + 1)?,
        ]));
        if segment_length < 2 || index.checked_add(segment_length)? > bytes.len() {
            return None;
        }
        if marker == 0xda {
            // Scan the bounded compressed payload only for the JPEG EOI
            // marker. This checks structural completion without decoding a
            // frame or expanding any image data.
            let scan_data_start = index + segment_length;
            return dimensions.filter(|_| {
                bytes[scan_data_start..]
                    .windows(2)
                    .any(|marker| marker == [0xff, 0xd9])
            });
        }
        if is_jpeg_start_of_frame(marker) {
            if dimensions.is_some() || segment_length < 8 {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]));
            let width = u32::from(u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]));
            dimensions = Some((width, height));
        }
        index += segment_length;
    }
    None
}

fn is_jpeg_start_of_frame(marker: u8) -> bool {
    matches!(
        marker,
        0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
    )
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 20 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let riff_length = usize::try_from(u32::from_le_bytes(bytes[4..8].try_into().ok()?)).ok()?;
    let riff_end = riff_length.checked_add(8)?;
    if riff_end != bytes.len() {
        return None;
    }

    // Validate every RIFF chunk's declared payload and mandatory alignment so
    // a short container cannot make it to an image decoder. This walks at
    // most MAX_ATTACHMENT_BYTES and never decodes a frame.
    let mut offset = 12;
    let mut dimensions = None;
    let mut extended_still_image = false;
    let mut saw_extended_image_data = false;
    while offset < riff_end {
        let chunk_header_end = offset.checked_add(8)?;
        if chunk_header_end > riff_end {
            return None;
        }
        let chunk_type = &bytes[offset..offset + 4];
        let chunk_length = usize::try_from(u32::from_le_bytes(
            bytes[offset + 4..chunk_header_end].try_into().ok()?,
        ))
        .ok()?;
        let chunk_data_start = chunk_header_end;
        let chunk_data_end = chunk_data_start.checked_add(chunk_length)?;
        if chunk_data_end > riff_end {
            return None;
        }
        let padded_chunk_end = chunk_data_end.checked_add(chunk_length & 1)?;
        if padded_chunk_end > riff_end || (chunk_length & 1 == 1 && bytes[chunk_data_end] != 0) {
            return None;
        }

        if offset == 12 {
            extended_still_image = chunk_type == b"VP8X";
            dimensions =
                webp_chunk_dimensions(chunk_type, &bytes[chunk_data_start..chunk_data_end]);
        } else if extended_still_image
            && matches!(chunk_type, b"VP8 " | b"VP8L")
            && webp_chunk_dimensions(chunk_type, &bytes[chunk_data_start..chunk_data_end]).is_some()
        {
            // A nonanimated VP8X canvas is not itself image data. Require a
            // bounded, structurally valid still-image chunk before allowing
            // the container to reach the preview decoder.
            saw_extended_image_data = true;
        }
        offset = padded_chunk_end;
    }

    if extended_still_image && !saw_extended_image_data {
        None
    } else {
        dimensions
    }
}

fn webp_chunk_dimensions(chunk_type: &[u8], chunk_data: &[u8]) -> Option<(u32, u32)> {
    match chunk_type {
        b"VP8X" if chunk_data.len() == 10 => {
            // In the WebP VP8X feature byte, the Animation (A) flag is bit 6
            // in the specification's MSB-first diagram, i.e. numeric bit 1
            // (0x02) in byte 0. Reject it before any preview/frame decode.
            if chunk_data[0] & 0x02 != 0 {
                return None;
            }
            Some((
                1 + u32::from_le_bytes([chunk_data[4], chunk_data[5], chunk_data[6], 0]),
                1 + u32::from_le_bytes([chunk_data[7], chunk_data[8], chunk_data[9], 0]),
            ))
        }
        b"VP8 " if chunk_data.len() >= 10 => {
            if &chunk_data[3..6] != b"\x9d\x01\x2a" {
                return None;
            }
            Some((
                u32::from(u16::from_le_bytes([chunk_data[6], chunk_data[7]]) & 0x3fff),
                u32::from(u16::from_le_bytes([chunk_data[8], chunk_data[9]]) & 0x3fff),
            ))
        }
        b"VP8L" if chunk_data.len() >= 5 && chunk_data[0] == 0x2f => {
            let dimensions =
                u32::from_le_bytes([chunk_data[1], chunk_data[2], chunk_data[3], chunk_data[4]]);
            Some((1 + (dimensions & 0x3fff), 1 + ((dimensions >> 14) & 0x3fff)))
        }
        _ => None,
    }
}
