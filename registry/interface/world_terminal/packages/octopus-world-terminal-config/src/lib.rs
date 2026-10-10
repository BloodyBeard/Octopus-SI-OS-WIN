use std::collections::HashMap;
use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub use octopus_world_terminal_keymap::TuiKeymap;

pub mod edit;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Notifications {
    Enabled(bool),
    Custom(Vec<String>),
}

impl Default for Notifications {
    fn default() -> Self {
        Self::Enabled(true)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotificationMethod {
    #[default]
    Auto,
    Osc9,
    Bel,
}

impl fmt::Display for NotificationMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotificationMethod::Auto => write!(f, "auto"),
            NotificationMethod::Osc9 => write!(f, "osc9"),
            NotificationMethod::Bel => write!(f, "bel"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotificationCondition {
    #[default]
    Unfocused,
    Always,
}

impl fmt::Display for NotificationCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotificationCondition::Unfocused => write!(f, "unfocused"),
            NotificationCondition::Always => write!(f, "always"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TuiPetAnchor {
    #[default]
    Composer,
    ScreenBottom,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum AltScreenMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl fmt::Display for AltScreenMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AltScreenMode::Auto => write!(f, "auto"),
            AltScreenMode::Always => write!(f, "always"),
            AltScreenMode::Never => write!(f, "never"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TuiNotificationSettings {
    #[serde(default, rename = "notifications")]
    pub notifications: Notifications,

    #[serde(default, rename = "notification_method")]
    pub method: NotificationMethod,

    #[serde(default, rename = "notification_condition")]
    pub condition: NotificationCondition,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelAvailabilityNuxConfig {
    #[serde(default, flatten)]
    pub shown_count: HashMap<String, u32>,
}

pub const DEFAULT_TERMINAL_RESIZE_REFLOW_FALLBACK_MAX_ROWS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalResizeReflowMaxRows {
    #[default]
    Auto,
    Disabled,
    Limit(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalResizeReflowConfig {
    pub max_rows: TerminalResizeReflowMaxRows,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TuiSectionToml {
    #[serde(default, flatten)]
    pub notification_settings: TuiNotificationSettings,

    #[serde(default = "default_true")]
    pub animations: bool,

    #[serde(default = "default_true")]
    pub show_tooltips: bool,

    #[serde(default)]
    pub vim_mode_default: bool,

    #[serde(default)]
    pub raw_output_mode: bool,

    #[serde(default)]
    pub alternate_screen: AltScreenMode,

    #[serde(default)]
    pub status_line: Option<Vec<String>>,

    #[serde(default = "default_true")]
    pub status_line_use_colors: bool,

    #[serde(default)]
    pub terminal_title: Option<Vec<String>>,

    #[serde(default)]
    pub theme: Option<String>,

    #[serde(default)]
    pub pet: Option<String>,

    #[serde(default)]
    pub pet_anchor: TuiPetAnchor,

    #[serde(default)]
    pub keymap: TuiKeymap,

    #[serde(default)]
    pub model_availability_nux: ModelAvailabilityNuxConfig,

    #[serde(default)]
    #[schemars(range(min = 0))]
    pub terminal_resize_reflow_max_rows: Option<usize>,
}

const fn default_true() -> bool {
    true
}

impl Default for TuiSectionToml {
    fn default() -> Self {
        Self {
            notification_settings: TuiNotificationSettings::default(),
            animations: true,
            show_tooltips: true,
            vim_mode_default: false,
            raw_output_mode: false,
            alternate_screen: AltScreenMode::default(),
            status_line: None,
            status_line_use_colors: true,
            terminal_title: None,
            theme: None,
            pet: None,
            pet_anchor: TuiPetAnchor::default(),
            keymap: TuiKeymap::default(),
            model_availability_nux: ModelAvailabilityNuxConfig::default(),
            terminal_resize_reflow_max_rows: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalConfig {
    pub notifications: TuiNotificationSettings,
    pub animations: bool,
    pub show_tooltips: bool,
    pub model_availability_nux: ModelAvailabilityNuxConfig,
    pub vim_mode_default: bool,
    pub raw_output_mode: bool,
    pub alternate_screen: AltScreenMode,
    pub status_line: Option<Vec<String>>,
    pub status_line_use_colors: bool,
    pub terminal_title: Option<Vec<String>>,
    pub theme: Option<String>,
    pub pet: Option<String>,
    pub pet_anchor: TuiPetAnchor,
    pub resize_reflow: TerminalResizeReflowConfig,
    pub keymap: TuiKeymap,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self::from_section(TuiSectionToml::default())
    }
}

impl TerminalConfig {
    pub fn from_section(tui: TuiSectionToml) -> Self {
        Self {
            notifications: tui.notification_settings,
            animations: tui.animations,
            show_tooltips: tui.show_tooltips,
            model_availability_nux: tui.model_availability_nux,
            vim_mode_default: tui.vim_mode_default,
            raw_output_mode: tui.raw_output_mode,
            alternate_screen: tui.alternate_screen,
            status_line: tui.status_line,
            status_line_use_colors: tui.status_line_use_colors,
            terminal_title: tui.terminal_title,
            theme: tui.theme,
            pet: tui.pet,
            pet_anchor: tui.pet_anchor,
            resize_reflow: TerminalResizeReflowConfig {
                max_rows: match tui.terminal_resize_reflow_max_rows {
                    Some(0) => TerminalResizeReflowMaxRows::Disabled,
                    Some(rows) => TerminalResizeReflowMaxRows::Limit(rows),
                    None => TerminalResizeReflowMaxRows::Auto,
                },
            },
            keymap: tui.keymap,
        }
    }

    pub fn from_effective_config_value(root: &toml::Value) -> std::io::Result<Self> {
        let Some(tui_value) = root.get("tui") else {
            return Ok(Self::default());
        };
        let section: TuiSectionToml = tui_value.clone().try_into().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid [tui] config section: {err}"),
            )
        })?;
        Ok(Self::from_section(section))
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
