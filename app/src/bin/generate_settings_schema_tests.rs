use super::*;
use warp_core::channel::ChannelState;

fn vertical_tabs_default(schema: &Value) -> bool {
    schema
        .pointer("/properties/appearance/properties/vertical_tabs/properties/enabled/default")
        .and_then(Value::as_bool)
        .expect("vertical tabs default should be a boolean")
}

#[test]
fn surface_annotation_matches_setting_schema_entry_metadata() {
    ensure_settings_linked();

    for entry in inventory::iter::<SettingSchemaEntry> {
        let surfaces = (entry.surfaces_fn)();
        let annotation = setting_surface_names(surfaces);
        let annotation_names: HashSet<&str> = annotation.iter().filter_map(Value::as_str).collect();

        assert_eq!(
            annotation_names.contains("gui"),
            surfaces.includes(SettingsMode::Gui),
            "GUI surface mismatch for {}",
            entry.storage_key
        );
        assert_eq!(
            annotation_names.contains("tui"),
            surfaces.includes(SettingsMode::Tui),
            "TUI surface mismatch for {}",
            entry.storage_key
        );
        assert_eq!(
            annotation_names.len(),
            usize::from(surfaces.includes(SettingsMode::Gui))
                + usize::from(surfaces.includes(SettingsMode::Tui)),
            "unexpected surface annotation for {}",
            entry.storage_key
        );
    }
}

#[test]
fn generated_schema_uses_the_requested_channel_without_leaking_channel_state() {
    let original_channel = ChannelState::channel();

    let (stable_schema, _) = generate_schema("stable");
    assert!(!vertical_tabs_default(&stable_schema));
    assert_eq!(ChannelState::channel(), original_channel);

    let (oss_schema, _) = generate_schema("oss");
    assert!(vertical_tabs_default(&oss_schema));
    assert_eq!(ChannelState::channel(), original_channel);
}
