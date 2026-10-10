use super::*;
use pretty_assertions::assert_eq;

fn effective(toml_text: &str) -> toml::Value {
    toml::from_str::<toml::Value>(toml_text).expect("valid toml")
}

#[test]
fn missing_tui_section_resolves_to_defaults() {
    let cfg = TerminalConfig::from_effective_config_value(&effective("model = \"gpt-5.2\""))
        .expect("defaults");
    assert_eq!(cfg, TerminalConfig::default());
    assert!(cfg.animations);
    assert!(cfg.show_tooltips);
    assert!(cfg.status_line_use_colors);
    assert_eq!(cfg.alternate_screen, AltScreenMode::Auto);
    assert_eq!(
        cfg.resize_reflow.max_rows,
        TerminalResizeReflowMaxRows::Auto
    );
    assert_eq!(
        cfg.notifications.notifications,
        Notifications::Enabled(true)
    );
}

#[test]
fn tui_section_parses_the_same_knobs_the_fused_config_did() {
    let cfg = TerminalConfig::from_effective_config_value(&effective(
        r#"
model = "provider-owned"

[tui]
notifications = ["agent-turn-complete"]
notification_method = "bel"
notification_condition = "always"
animations = false
show_tooltips = false
vim_mode_default = true
raw_output_mode = true
alternate_screen = "never"
status_line = ["model-with-reasoning"]
status_line_use_colors = false
terminal_title = []
theme = "gruvbox-dark"
pet = "octopus"
pet_anchor = "screen-bottom"
terminal_resize_reflow_max_rows = 250

[tui.keymap.global]
"#,
    ))
    .expect("parse");

    assert_eq!(
        cfg.notifications.notifications,
        Notifications::Custom(vec!["agent-turn-complete".to_string()])
    );
    assert_eq!(cfg.notifications.method, NotificationMethod::Bel);
    assert_eq!(cfg.notifications.condition, NotificationCondition::Always);
    assert!(!cfg.animations);
    assert!(!cfg.show_tooltips);
    assert!(cfg.vim_mode_default);
    assert!(cfg.raw_output_mode);
    assert_eq!(cfg.alternate_screen, AltScreenMode::Never);
    assert_eq!(
        cfg.status_line,
        Some(vec!["model-with-reasoning".to_string()])
    );
    assert!(!cfg.status_line_use_colors);
    assert_eq!(cfg.terminal_title, Some(Vec::new()));
    assert_eq!(cfg.theme.as_deref(), Some("gruvbox-dark"));
    assert_eq!(cfg.pet.as_deref(), Some("octopus"));
    assert_eq!(cfg.pet_anchor, TuiPetAnchor::ScreenBottom);
    assert_eq!(
        cfg.resize_reflow.max_rows,
        TerminalResizeReflowMaxRows::Limit(250)
    );
}

#[test]
fn resize_reflow_zero_disables_the_cap() {
    let cfg = TerminalConfig::from_effective_config_value(&effective(
        "[tui]\nterminal_resize_reflow_max_rows = 0",
    ))
    .expect("parse");
    assert_eq!(
        cfg.resize_reflow.max_rows,
        TerminalResizeReflowMaxRows::Disabled
    );
}

#[test]
fn malformed_tui_section_fails_closed() {
    let err = TerminalConfig::from_effective_config_value(&effective(
        "[tui]\nanimations = \"not-a-bool\"",
    ))
    .expect_err("must fail closed");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn provider_sections_are_invisible_to_the_terminal_owner() {
    let cfg = TerminalConfig::from_effective_config_value(&effective(
        "[permissions]\ngibberish = 3\n\n[tui]\ntheme = \"one-dark\"",
    ))
    .expect("parse");
    assert_eq!(cfg.theme.as_deref(), Some("one-dark"));
}
