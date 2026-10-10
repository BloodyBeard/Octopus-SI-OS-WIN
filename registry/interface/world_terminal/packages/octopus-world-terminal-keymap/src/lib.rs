use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as SerdeError;
use std::collections::BTreeMap;

pub const MAX_FUNCTION_KEY: u8 = 24;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(transparent)]
pub struct KeybindingSpec(#[schemars(with = "String")] pub String);

impl KeybindingSpec {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for KeybindingSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let normalized = normalize_keybinding_spec(&raw).map_err(SerdeError::custom)?;
        Ok(Self(normalized))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(untagged)]
pub enum KeybindingsSpec {
    One(KeybindingSpec),
    Many(Vec<KeybindingSpec>),
}

impl KeybindingsSpec {
    pub fn specs(&self) -> Vec<&KeybindingSpec> {
        match self {
            Self::One(spec) => vec![spec],
            Self::Many(specs) => specs.iter().collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiGlobalKeymap {
    pub open_transcript: Option<KeybindingsSpec>,
    pub open_external_editor: Option<KeybindingsSpec>,
    pub copy: Option<KeybindingsSpec>,
    pub clear_terminal: Option<KeybindingsSpec>,
    pub submit: Option<KeybindingsSpec>,
    pub queue: Option<KeybindingsSpec>,
    pub toggle_shortcuts: Option<KeybindingsSpec>,
    pub toggle_vim_mode: Option<KeybindingsSpec>,
    pub toggle_fast_mode: Option<KeybindingsSpec>,
    pub toggle_raw_output: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiChatKeymap {
    pub interrupt_turn: Option<KeybindingsSpec>,
    pub decrease_reasoning_effort: Option<KeybindingsSpec>,
    pub increase_reasoning_effort: Option<KeybindingsSpec>,
    pub edit_queued_message: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiComposerKeymap {
    pub submit: Option<KeybindingsSpec>,
    pub queue: Option<KeybindingsSpec>,
    pub toggle_shortcuts: Option<KeybindingsSpec>,
    pub history_search_previous: Option<KeybindingsSpec>,
    pub history_search_next: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiEditorKeymap {
    pub insert_newline: Option<KeybindingsSpec>,
    pub move_left: Option<KeybindingsSpec>,
    pub move_right: Option<KeybindingsSpec>,
    pub move_up: Option<KeybindingsSpec>,
    pub move_down: Option<KeybindingsSpec>,
    pub move_word_left: Option<KeybindingsSpec>,
    pub move_word_right: Option<KeybindingsSpec>,
    pub move_line_start: Option<KeybindingsSpec>,
    pub move_line_end: Option<KeybindingsSpec>,
    pub delete_backward: Option<KeybindingsSpec>,
    pub delete_forward: Option<KeybindingsSpec>,
    pub delete_backward_word: Option<KeybindingsSpec>,
    pub delete_forward_word: Option<KeybindingsSpec>,
    pub kill_line_start: Option<KeybindingsSpec>,
    pub kill_whole_line: Option<KeybindingsSpec>,
    pub kill_line_end: Option<KeybindingsSpec>,
    pub yank: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TuiVimNormalKeymap {
    pub enter_insert: Option<KeybindingsSpec>,
    pub append_after_cursor: Option<KeybindingsSpec>,
    pub append_line_end: Option<KeybindingsSpec>,
    pub insert_line_start: Option<KeybindingsSpec>,
    pub open_line_below: Option<KeybindingsSpec>,
    pub open_line_above: Option<KeybindingsSpec>,
    pub move_left: Option<KeybindingsSpec>,
    pub move_right: Option<KeybindingsSpec>,
    pub move_up: Option<KeybindingsSpec>,
    pub move_down: Option<KeybindingsSpec>,
    pub move_word_forward: Option<KeybindingsSpec>,
    pub move_word_backward: Option<KeybindingsSpec>,
    pub move_word_end: Option<KeybindingsSpec>,
    pub move_line_start: Option<KeybindingsSpec>,
    pub move_line_end: Option<KeybindingsSpec>,
    pub delete_char: Option<KeybindingsSpec>,
    pub substitute_char: Option<KeybindingsSpec>,
    pub delete_to_line_end: Option<KeybindingsSpec>,
    pub change_to_line_end: Option<KeybindingsSpec>,
    pub yank_line: Option<KeybindingsSpec>,
    pub paste_after: Option<KeybindingsSpec>,
    pub start_delete_operator: Option<KeybindingsSpec>,
    pub start_yank_operator: Option<KeybindingsSpec>,
    pub start_change_operator: Option<KeybindingsSpec>,
    pub cancel_operator: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TuiVimOperatorKeymap {
    pub delete_line: Option<KeybindingsSpec>,
    pub yank_line: Option<KeybindingsSpec>,
    pub motion_left: Option<KeybindingsSpec>,
    pub motion_right: Option<KeybindingsSpec>,
    pub motion_up: Option<KeybindingsSpec>,
    pub motion_down: Option<KeybindingsSpec>,
    pub motion_word_forward: Option<KeybindingsSpec>,
    pub motion_word_backward: Option<KeybindingsSpec>,
    pub motion_word_end: Option<KeybindingsSpec>,
    pub motion_line_start: Option<KeybindingsSpec>,
    pub motion_line_end: Option<KeybindingsSpec>,
    pub select_inner_text_object: Option<KeybindingsSpec>,
    pub select_around_text_object: Option<KeybindingsSpec>,
    pub cancel: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiVimTextObjectKeymap {
    pub word: Option<KeybindingsSpec>,
    pub big_word: Option<KeybindingsSpec>,
    pub parentheses: Option<KeybindingsSpec>,
    pub brackets: Option<KeybindingsSpec>,
    pub braces: Option<KeybindingsSpec>,
    pub double_quote: Option<KeybindingsSpec>,
    pub single_quote: Option<KeybindingsSpec>,
    pub backtick: Option<KeybindingsSpec>,
    pub cancel: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiPagerKeymap {
    pub scroll_up: Option<KeybindingsSpec>,
    pub scroll_down: Option<KeybindingsSpec>,
    pub page_up: Option<KeybindingsSpec>,
    pub page_down: Option<KeybindingsSpec>,
    pub half_page_up: Option<KeybindingsSpec>,
    pub half_page_down: Option<KeybindingsSpec>,
    pub jump_top: Option<KeybindingsSpec>,
    pub jump_bottom: Option<KeybindingsSpec>,
    pub close: Option<KeybindingsSpec>,
    pub close_transcript: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiListKeymap {
    pub move_up: Option<KeybindingsSpec>,
    pub move_down: Option<KeybindingsSpec>,
    pub move_left: Option<KeybindingsSpec>,
    pub move_right: Option<KeybindingsSpec>,
    pub page_up: Option<KeybindingsSpec>,
    pub page_down: Option<KeybindingsSpec>,
    pub jump_top: Option<KeybindingsSpec>,
    pub jump_bottom: Option<KeybindingsSpec>,
    pub accept: Option<KeybindingsSpec>,
    pub cancel: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiApprovalKeymap {
    pub open_fullscreen: Option<KeybindingsSpec>,
    pub open_thread: Option<KeybindingsSpec>,
    pub approve: Option<KeybindingsSpec>,
    pub approve_for_session: Option<KeybindingsSpec>,
    pub approve_for_prefix: Option<KeybindingsSpec>,
    pub deny: Option<KeybindingsSpec>,
    pub decline: Option<KeybindingsSpec>,
    pub cancel: Option<KeybindingsSpec>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TuiKeymap {
    #[serde(default)]
    pub global: TuiGlobalKeymap,
    #[serde(default)]
    pub chat: TuiChatKeymap,
    #[serde(default)]
    pub composer: TuiComposerKeymap,
    #[serde(default)]
    pub editor: TuiEditorKeymap,
    #[serde(default)]
    pub vim_normal: TuiVimNormalKeymap,
    #[serde(default)]
    pub vim_operator: TuiVimOperatorKeymap,
    #[serde(default)]
    pub vim_text_object: TuiVimTextObjectKeymap,
    #[serde(default)]
    pub pager: TuiPagerKeymap,
    #[serde(default)]
    pub list: TuiListKeymap,
    #[serde(default)]
    pub approval: TuiApprovalKeymap,
}

fn normalize_keybinding_spec(raw: &str) -> Result<String, String> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return Err(
            "keybinding cannot be empty. Use values like `ctrl-a` or `shift-enter`.\n\
See the Codex keymap documentation for supported actions and examples."
                .to_string(),
        );
    }

    let segments: Vec<&str> = lower
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return Err(format!(
            "invalid keybinding `{raw}`. Use values like `ctrl-a`, `shift-enter`, or `page-down`."
        ));
    }

    let mut modifiers =
        BTreeMap::<&str, bool>::from([("ctrl", false), ("alt", false), ("shift", false)]);
    let mut key_segments = Vec::new();
    let mut saw_key = false;

    for segment in segments {
        let canonical_mod = match segment {
            "ctrl" | "control" => Some("ctrl"),
            "alt" | "option" => Some("alt"),
            "shift" => Some("shift"),
            _ => None,
        };

        if !saw_key && let Some(modifier) = canonical_mod {
            if modifiers.get(modifier).copied().unwrap_or(false) {
                return Err(format!(
                    "duplicate modifier in keybinding `{raw}`. Use each modifier at most once."
                ));
            }
            modifiers.insert(modifier, true);
            continue;
        }

        saw_key = true;
        key_segments.push(segment);
    }

    if key_segments.is_empty() {
        return Err(format!(
            "missing key in keybinding `{raw}`. Add a key name like `a`, `enter`, or `page-down`."
        ));
    }

    if key_segments
        .iter()
        .any(|segment| matches!(*segment, "ctrl" | "control" | "alt" | "option" | "shift"))
    {
        return Err(format!(
            "invalid keybinding `{raw}`: modifiers must come before the key (for example `ctrl-a`)."
        ));
    }

    let key = normalize_key_name(&key_segments.join("-"), raw)?;
    let mut normalized = Vec::new();
    if modifiers.get("ctrl").copied().unwrap_or(false) {
        normalized.push("ctrl".to_string());
    }
    if modifiers.get("alt").copied().unwrap_or(false) {
        normalized.push("alt".to_string());
    }
    if modifiers.get("shift").copied().unwrap_or(false) {
        normalized.push("shift".to_string());
    }
    normalized.push(key);
    Ok(normalized.join("-"))
}

fn normalize_key_name(key: &str, original: &str) -> Result<String, String> {
    let alias = match key {
        "escape" => "esc",
        "return" => "enter",
        "spacebar" => "space",
        "pgup" | "pageup" => "page-up",
        "pgdn" | "pagedown" => "page-down",
        "del" => "delete",
        other => other,
    };

    if alias.len() == 1 {
        let ch = alias.chars().next().unwrap_or_default();
        if ch.is_ascii() && !ch.is_ascii_control() && ch != '-' {
            return Ok(alias.to_string());
        }
    }

    if matches!(
        alias,
        "enter"
            | "tab"
            | "backspace"
            | "esc"
            | "delete"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "page-up"
            | "page-down"
            | "space"
            | "minus"
    ) {
        return Ok(alias.to_string());
    }

    if let Some(number) = alias.strip_prefix('f')
        && let Ok(number) = number.parse::<u8>()
        && (1..=MAX_FUNCTION_KEY).contains(&number)
    {
        return Ok(alias.to_string());
    }

    Err(format!(
        "unknown key `{key}` in keybinding `{original}`. \
Use a printable character (for example `a`), function keys (`f1`-`f{MAX_FUNCTION_KEY}`), \
or one of: enter, tab, backspace, esc, delete, arrows, home/end, page-up/page-down, space, minus.\n\
See the Codex keymap documentation for supported actions and examples."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn misplaced_action_at_keymap_root_is_rejected() {
        let toml_input = r#"
            open_transcript = "ctrl-s"
        "#;
        let result = toml::from_str::<TuiKeymap>(toml_input);
        assert!(
            result.is_err(),
            "expected error for action at keymap root, got: {result:?}"
        );
    }

    #[test]
    fn misspelled_action_under_context_is_rejected() {
        let toml_input = r#"
            [global]
            open_transcrip = "ctrl-x"
        "#;
        let err = toml::from_str::<TuiKeymap>(toml_input)
            .expect_err("expected unknown action under context");
        assert!(
            err.to_string().contains("open_transcrip"),
            "expected error to mention misspelled field, got: {err}"
        );
    }

    #[test]
    fn misspelled_vim_text_object_action_is_rejected() {
        let toml_input = r#"
            [vim_text_object]
            double_quotes = "shift-quote"
        "#;
        let err = toml::from_str::<TuiKeymap>(toml_input)
            .expect_err("expected unknown vim text object action");
        assert!(
            err.to_string().contains("double_quotes"),
            "expected error to mention misspelled field, got: {err}"
        );
    }

    #[test]
    fn removed_backtrack_actions_are_rejected() {
        for (context, action) in [
            ("global", "edit_previous_message"),
            ("global", "confirm_edit_previous_message"),
            ("chat", "edit_previous_message"),
            ("chat", "confirm_edit_previous_message"),
            ("pager", "edit_previous_message"),
            ("pager", "edit_next_message"),
            ("pager", "confirm_edit_message"),
        ] {
            let toml_input = format!(
                r#"
                [{context}]
                {action} = "ctrl-x"
                "#
            );
            let err = toml::from_str::<TuiKeymap>(&toml_input)
                .expect_err("expected removed backtrack action to be rejected");
            assert!(
                err.to_string().contains(action),
                "expected error to mention removed field {action}, got: {err}"
            );
        }
    }

    #[test]
    fn action_under_global_context_is_accepted() {
        let toml_input = r#"
            [global]
            open_transcript = "ctrl-s"
        "#;
        let keymap: TuiKeymap = toml::from_str(toml_input).expect("valid config");
        assert!(keymap.global.open_transcript.is_some());
    }

    #[test]
    fn minus_bindings_under_global_context_are_accepted() {
        for (spec, expected) in [
            (
                "minus",
                KeybindingsSpec::One(KeybindingSpec("minus".to_string())),
            ),
            (
                "alt-minus",
                KeybindingsSpec::One(KeybindingSpec("alt-minus".to_string())),
            ),
        ] {
            let toml_input = format!(
                r#"
                [global]
                open_transcript = "{spec}"
                "#
            );
            let keymap: TuiKeymap = toml::from_str(&toml_input).expect("valid config");
            let mut expected_keymap = TuiKeymap::default();
            expected_keymap.global.open_transcript = Some(expected);

            assert_eq!(keymap, expected_keymap);
        }
    }

    #[test]
    fn function_keys_through_f24_are_accepted() {
        assert_eq!(normalize_keybinding_spec("F13"), Ok("f13".to_string()));
        assert_eq!(normalize_keybinding_spec("f24"), Ok("f24".to_string()));
        assert!(normalize_keybinding_spec("f25").is_err());
    }
}
