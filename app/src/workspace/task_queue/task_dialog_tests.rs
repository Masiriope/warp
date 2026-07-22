use std::sync::Arc;

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
        .add_pasted_image("image/png", tiny_png())
        .expect("first image should be accepted");
    state
        .add_pasted_image("image/png", tiny_png())
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
        "Only PNG, JPEG, or WebP images can be attached."
    );
    assert_eq!(
        state
            .add_pasted_image("image/png", vec![0, 1, 2])
            .expect_err("mismatched image bytes must not be accepted"),
        "The pasted image has an invalid or unsupported header."
    );

    for _ in 0..TaskDialogState::MAX_ATTACHMENTS {
        state
            .add_pasted_image("image/png", tiny_png())
            .expect("image within the small dialog limit should be accepted");
    }

    assert_eq!(
        state
            .add_pasted_image("image/png", tiny_png())
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

#[test]
fn pasted_images_are_header_validated_before_they_can_become_attachments_or_previews() {
    let mut state = TaskDialogState::default();

    assert_eq!(
        state
            .add_pasted_image("image/png", png_header(4_097, 4_097))
            .expect_err("a 16 MiPixel limit rejects oversized PNG headers"),
        "The pasted image is too large to attach."
    );
    assert!(state.attachments().is_empty());

    assert_eq!(
        state
            .add_pasted_image("image/png", png_header(u32::MAX, u32::MAX))
            .expect_err("dimension multiplication must never overflow"),
        "The pasted image is too large to attach."
    );
    assert!(state.attachments().is_empty());
}

#[test]
fn rejects_animated_or_vector_and_malformed_image_payloads() {
    let mut state = TaskDialogState::default();

    for (mime_type, bytes) in [
        ("image/gif", b"GIF89a\x01\0\x01\0\0\0\0".as_slice()),
        (
            "image/svg+xml",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".as_slice(),
        ),
        ("image/png", b"\x89PNG\r\n\x1a\nnot-an-ihdr".as_slice()),
    ] {
        assert!(state.add_pasted_image(mime_type, bytes.to_vec()).is_err());
        assert!(state.attachments().is_empty());
    }
}

#[test]
fn submitting_moves_shared_attachment_bytes_without_a_deep_copy() {
    let bytes: Arc<[u8]> = Arc::from(tiny_png());
    let mut state = TaskDialogState::default();
    state.select_workspace(WorkspaceId::new("workspace-1"));
    state.set_title("Preserve image bytes");
    state
        .add_pasted_image("image/png", Arc::clone(&bytes))
        .expect("valid image should be accepted");

    let input = state
        .into_new_task_input()
        .expect("valid state should submit");
    let event_boundary_copy = input.clone();

    assert!(Arc::ptr_eq(
        &bytes,
        input.attachments[0]
            .in_memory_bytes_arc()
            .expect("pasted image should remain in memory until storage")
    ));
    assert!(Arc::ptr_eq(
        &bytes,
        event_boundary_copy.attachments[0]
            .in_memory_bytes_arc()
            .expect("workspace event clones should retain the same image allocation")
    ));
}

/// A complete, valid 1×1 RGBA PNG fixture. Header validation deliberately
/// reads only the signature and IHDR fields; the full fixture guards against
/// accidentally testing an impossible clipboard payload.
fn tiny_png() -> Vec<u8> {
    vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 215, 99, 248, 207, 192, 240, 31,
        0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ]
}

fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes
}
