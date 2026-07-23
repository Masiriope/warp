use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, MutexGuard};
use std::time::Duration;

use warp_core::channel::{Channel, ChannelState};

static SCHEMA_GENERATION_TEST_LOCK: Mutex<()> = Mutex::new(());

fn schema_generation_test_guard() -> MutexGuard<'static, ()> {
    SCHEMA_GENERATION_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    let _test_guard = schema_generation_test_guard();
    let original_channel = ChannelState::channel();

    let (stable_schema, _) = generate_schema("stable");
    assert!(!vertical_tabs_default(&stable_schema));
    assert_eq!(ChannelState::channel(), original_channel);

    let (oss_schema, _) = generate_schema("oss");
    assert!(vertical_tabs_default(&oss_schema));
    assert_eq!(ChannelState::channel(), original_channel);
}

#[test]
fn concurrent_schema_generation_keeps_channel_specific_defaults_and_restores_state() {
    let _test_guard = schema_generation_test_guard();
    let original_channel = ChannelState::channel();
    let start = Arc::new(Barrier::new(2));

    let stable_start = start.clone();
    let stable = std::thread::spawn(move || {
        stable_start.wait();
        generate_schema("stable").0
    });
    let oss_start = start.clone();
    let oss = std::thread::spawn(move || {
        oss_start.wait();
        generate_schema("oss").0
    });

    let stable_schema = stable.join().expect("stable generation should not panic");
    let oss_schema = oss.join().expect("OSS generation should not panic");

    assert!(!vertical_tabs_default(&stable_schema));
    assert!(vertical_tabs_default(&oss_schema));
    assert_eq!(ChannelState::channel(), original_channel);
}

#[test]
fn schema_channel_state_scope_serializes_concurrent_calls() {
    let _test_guard = schema_generation_test_guard();
    let original_channel = ChannelState::channel();
    let start = Arc::new(Barrier::new(2));
    let active_scopes = Arc::new(AtomicUsize::new(0));

    let spawn_scope = |channel| {
        let start = start.clone();
        let active_scopes = active_scopes.clone();
        std::thread::spawn(move || {
            start.wait();
            with_schema_channel_state(channel, || {
                assert_eq!(
                    active_scopes.fetch_add(1, Ordering::SeqCst),
                    0,
                    "schema channel state scopes must not overlap"
                );
                assert_eq!(ChannelState::channel(), channel);
                std::thread::sleep(Duration::from_millis(25));
                active_scopes.fetch_sub(1, Ordering::SeqCst);
            });
        })
    };

    let stable = spawn_scope(Channel::Stable);
    let oss = spawn_scope(Channel::Oss);
    stable.join().expect("stable scope should not panic");
    oss.join().expect("OSS scope should not panic");

    assert_eq!(ChannelState::channel(), original_channel);
}
