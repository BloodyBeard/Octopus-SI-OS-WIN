#![allow(clippy::too_many_arguments)]

pub mod color;
pub mod custom_terminal;
pub use custom_terminal::Terminal;

pub mod diff_model;
pub mod diff_render;

pub mod exec_command;

pub mod insert_history;
pub use insert_history::insert_history_lines;

pub mod key_hint;
pub mod line_truncation;

pub mod live_wrap;
pub use live_wrap::RowBuilder;

pub mod markdown;
pub mod markdown_render;
pub use markdown_render::render_markdown_text;
pub mod markdown_stream;
pub mod markdown_text_merge;

pub mod motion;

pub mod render;
pub mod selection_list;
pub mod shimmer;
pub mod style;

pub mod terminal_hyperlinks;
pub mod terminal_palette;
pub mod terminal_probe;
pub mod text_formatting;
pub mod transcript_reflow;

pub mod ui_consts;
pub mod width;
pub mod wrapping;

pub mod table_detect;

#[cfg(any(test, feature = "test-support"))]
pub mod test_backend;
