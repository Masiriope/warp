use super::{TaskDialogState, TaskPriority, WorkspaceId};

#[test]
fn default_state_requires_a_workspace_and_title() {
    let state = TaskDialogState::default();

    assert!(!state.can_submit());
}

#[test]
fn workspace_and_nonblank_title_enable_submission() {
    let mut state = TaskDialogState::default();
    state.select_workspace(WorkspaceId::new("workspace-1"));
    state.set_title("  Document the task dialog  ");

    assert!(state.can_submit());
}

#[test]
fn pasted_text_is_appended_to_markdown_context() {
    let mut state = TaskDialogState::default();
    state.set_context(Some("Existing context".into()));
    state.append_pasted_text("\n\nAdditional context");

    assert_eq!(
        state.context(),
        Some("Existing context\n\nAdditional context")
    );
}

#[test]
fn pasted_images_keep_order_and_removing_first_keeps_second() {
    let mut state = TaskDialogState::default();
    state
        .add_pasted_image("image/png", png_bytes(0))
        .expect("first image should be accepted");
    state
        .add_pasted_image("image/png", png_bytes(1))
        .expect("second image should be accepted");

    assert_eq!(state.attachments()[0].file_name(), "pasted-image-1.png");
    assert_eq!(state.attachments()[1].file_name(), "pasted-image-2.png");

    state.remove_attachment(0);

    assert_eq!(state.attachments().len(), 1);
    assert_eq!(state.attachments()[0].file_name(), "pasted-image-2.png");
}

#[test]
fn attachment_limits_invalid_images_and_title_whitespace_are_enforced_in_memory() {
    let mut state = TaskDialogState::default();
    state.select_workspace(WorkspaceId::new("workspace-1"));
    state.set_title("  Keep only the useful title  ");
    state.set_priority(TaskPriority::High);

    assert_eq!(
        state
            .add_pasted_image("text/plain", vec![1])
            .expect_err("non-images must not be accepted"),
        "Only PNG, JPEG, GIF, WebP, or SVG images can be attached."
    );
    assert_eq!(
        state
            .add_pasted_image("image/png", vec![0, 1, 2])
            .expect_err("mismatched image bytes must not be accepted"),
        "Only PNG, JPEG, GIF, WebP, or SVG images can be attached."
    );

    for index in 0..TaskDialogState::MAX_ATTACHMENTS {
        state
            .add_pasted_image("image/png", png_bytes(index as u8))
            .expect("image within the small dialog limit should be accepted");
    }

    assert_eq!(
        state
            .add_pasted_image("image/png", png_bytes(9))
            .expect_err("the dialog has a documented attachment limit"),
        "A task can include at most 8 pasted images."
    );

    let input = state
        .into_new_task_input()
        .expect("valid state should submit");
    assert_eq!(input.title, "Keep only the useful title");
    assert_eq!(input.priority, TaskPriority::High);
    assert_eq!(input.attachments.len(), TaskDialogState::MAX_ATTACHMENTS);
}

fn png_bytes(last_byte: u8) -> Vec<u8> {
    vec![137, 80, 78, 71, 13, 10, 26, 10, last_byte]
}
