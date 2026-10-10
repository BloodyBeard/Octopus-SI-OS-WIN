use super::*;
use crate::TerminalConfig;
use pretty_assertions::assert_eq;

fn apply_to(contents: &str, edits: Vec<TerminalConfigEdit>) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_TOML_FILE);
    std::fs::write(&path, contents).expect("seed config");
    TerminalConfigEditsBuilder::for_config_path(&path)
        .with_edits(edits)
        .apply_blocking()
        .expect("apply");
    std::fs::read_to_string(&path).expect("read back")
}

#[test]
fn writes_only_the_tui_section_and_preserves_the_rest_of_the_file() {
    let contents = "# provider half\nmodel = \"gpt-5.2\" # keep me\n\n[permissions]\nprofile = \"workspace\"\n";
    let updated = apply_to(
        contents,
        vec![
            syntax_theme_edit("one-dark"),
            status_line_items_edit(&["model-with-reasoning".to_string()]),
        ],
    );

    assert!(updated.contains("# provider half"));
    assert!(updated.contains("model = \"gpt-5.2\" # keep me"));
    assert!(updated.contains("[permissions]"));
    assert!(updated.contains("theme = \"one-dark\""));
    assert!(updated.contains("status_line = [\"model-with-reasoning\"]"));
}

#[test]
fn clear_removes_a_keymap_binding() {
    let contents = "[tui.keymap.chat]\nquit = \"ctrl+q\"\n";
    let updated = apply_to(contents, vec![keymap_binding_clear_edit("chat", "quit")]);
    assert!(!updated.contains("ctrl+q"));
}

#[test]
fn a_non_tui_path_is_refused_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(CONFIG_TOML_FILE);
    std::fs::write(&path, "model = \"gpt-5.2\"\n").expect("seed config");
    let err = TerminalConfigEditsBuilder::for_config_path(&path)
        .with_edits(vec![TerminalConfigEdit::SetPath {
            segments: vec!["model".to_string()],
            value: value("hijack"),
        }])
        .apply_blocking()
        .expect_err("must refuse");
    assert!(err.to_string().contains("only edits the [tui] section"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "model = \"gpt-5.2\"\n"
    );
}

#[test]
fn nux_counts_round_trip() {
    let mut counts = std::collections::HashMap::new();
    counts.insert("gpt-5.4".to_string(), 2_u32);
    let updated = apply_to("", model_availability_nux_count_edits(&counts));
    assert!(updated.contains("[tui.model_availability_nux]"));
    assert!(updated.contains("\"gpt-5.4\" = 2"));

    let parsed: toml::Value = toml::from_str(&updated).expect("parse");
    let cfg = TerminalConfig::from_effective_config_value(&parsed).expect("resolve");
    assert_eq!(
        cfg.model_availability_nux.shown_count.get("gpt-5.4"),
        Some(&2)
    );
}
