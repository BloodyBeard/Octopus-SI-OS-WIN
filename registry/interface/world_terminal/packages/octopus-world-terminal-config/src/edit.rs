use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use octopus_utils_path::resolve_symlink_write_paths;
use octopus_utils_path::write_atomically;
use tokio::task;
use toml_edit::DocumentMut;
use toml_edit::Item as TomlItem;
use toml_edit::Table as TomlTable;
use toml_edit::value;

pub const CONFIG_TOML_FILE: &str = "config.toml";

const TUI_TABLE_KEY: &str = "tui";

#[derive(Clone, Debug)]
pub enum TerminalConfigEdit {
    SetPath {
        segments: Vec<String>,
        value: TomlItem,
    },
    ClearPath {
        segments: Vec<String>,
    },
}

impl TerminalConfigEdit {
    fn segments(&self) -> &[String] {
        match self {
            TerminalConfigEdit::SetPath { segments, .. }
            | TerminalConfigEdit::ClearPath { segments } => segments,
        }
    }
}

pub fn syntax_theme_edit(name: &str) -> TerminalConfigEdit {
    TerminalConfigEdit::SetPath {
        segments: vec![TUI_TABLE_KEY.to_string(), "theme".to_string()],
        value: value(name.to_string()),
    }
}

pub fn tui_pet_edit(name: &str) -> TerminalConfigEdit {
    TerminalConfigEdit::SetPath {
        segments: vec![TUI_TABLE_KEY.to_string(), "pet".to_string()],
        value: value(name.to_string()),
    }
}

pub fn status_line_items_edit(items: &[String]) -> TerminalConfigEdit {
    let array = items.iter().cloned().collect::<toml_edit::Array>();

    TerminalConfigEdit::SetPath {
        segments: vec![TUI_TABLE_KEY.to_string(), "status_line".to_string()],
        value: TomlItem::Value(array.into()),
    }
}

pub fn status_line_use_colors_edit(enabled: bool) -> TerminalConfigEdit {
    TerminalConfigEdit::SetPath {
        segments: vec![
            TUI_TABLE_KEY.to_string(),
            "status_line_use_colors".to_string(),
        ],
        value: value(enabled),
    }
}

pub fn terminal_title_items_edit(items: &[String]) -> TerminalConfigEdit {
    let array = items.iter().cloned().collect::<toml_edit::Array>();

    TerminalConfigEdit::SetPath {
        segments: vec![TUI_TABLE_KEY.to_string(), "terminal_title".to_string()],
        value: TomlItem::Value(array.into()),
    }
}

fn keymap_binding_value(keys: &[String]) -> TomlItem {
    if let [key] = keys {
        value(key.to_string())
    } else {
        let array = keys.iter().cloned().collect::<toml_edit::Array>();
        TomlItem::Value(array.into())
    }
}

pub fn keymap_bindings_edit(context: &str, action: &str, keys: &[String]) -> TerminalConfigEdit {
    TerminalConfigEdit::SetPath {
        segments: vec![
            TUI_TABLE_KEY.to_string(),
            "keymap".to_string(),
            context.to_string(),
            action.to_string(),
        ],
        value: keymap_binding_value(keys),
    }
}

pub fn keymap_binding_edit(context: &str, action: &str, key: &str) -> TerminalConfigEdit {
    keymap_bindings_edit(context, action, &[key.to_string()])
}

pub fn keymap_binding_clear_edit(context: &str, action: &str) -> TerminalConfigEdit {
    TerminalConfigEdit::ClearPath {
        segments: vec![
            TUI_TABLE_KEY.to_string(),
            "keymap".to_string(),
            context.to_string(),
            action.to_string(),
        ],
    }
}

pub fn model_availability_nux_count_edits(
    shown_count: &HashMap<String, u32>,
) -> Vec<TerminalConfigEdit> {
    let mut shown_count_entries: Vec<_> = shown_count.iter().collect();
    shown_count_entries.sort_unstable_by_key(|(left, _)| *left);

    let mut edits = vec![TerminalConfigEdit::ClearPath {
        segments: vec![
            TUI_TABLE_KEY.to_string(),
            "model_availability_nux".to_string(),
        ],
    }];
    for (model_slug, count) in shown_count_entries {
        edits.push(TerminalConfigEdit::SetPath {
            segments: vec![
                TUI_TABLE_KEY.to_string(),
                "model_availability_nux".to_string(),
                model_slug.clone(),
            ],
            value: value(i64::from(*count)),
        });
    }

    edits
}

struct TerminalConfigDocument {
    doc: DocumentMut,
}

#[derive(Copy, Clone)]
enum TraversalMode {
    Create,
    Existing,
}

impl TerminalConfigDocument {
    fn new(doc: DocumentMut) -> Self {
        Self { doc }
    }

    fn apply(&mut self, edit: &TerminalConfigEdit) -> anyhow::Result<bool> {
        anyhow::ensure!(
            edit.segments().first().map(String::as_str) == Some(TUI_TABLE_KEY),
            "terminal config writer only edits the [{TUI_TABLE_KEY}] section (got path {:?})",
            edit.segments()
        );
        match edit {
            TerminalConfigEdit::SetPath { segments, value } => {
                Ok(self.insert(segments, value.clone()))
            }
            TerminalConfigEdit::ClearPath { segments } => Ok(self.remove(segments)),
        }
    }

    fn insert(&mut self, segments: &[String], value: TomlItem) -> bool {
        let Some((last, parents)) = segments.split_last() else {
            return false;
        };

        let Some(parent) = self.descend(parents, TraversalMode::Create) else {
            return false;
        };

        let mut value = value;
        if let Some(existing) = parent.get(last) {
            Self::preserve_decor(existing, &mut value);
        }
        parent[last] = value;
        true
    }

    fn remove(&mut self, segments: &[String]) -> bool {
        let Some((last, parents)) = segments.split_last() else {
            return false;
        };

        let Some(parent) = self.descend(parents, TraversalMode::Existing) else {
            return false;
        };

        parent.remove(last).is_some()
    }

    fn descend(&mut self, segments: &[String], mode: TraversalMode) -> Option<&mut TomlTable> {
        let mut current = self.doc.as_table_mut();

        for segment in segments {
            match mode {
                TraversalMode::Create => {
                    if !current.contains_key(segment.as_str()) {
                        current.insert(segment.as_str(), TomlItem::Table(new_implicit_table()));
                    }

                    let item = current.get_mut(segment.as_str())?;
                    current = ensure_table_for_write(item)?;
                }
                TraversalMode::Existing => {
                    let item = current.get_mut(segment.as_str())?;
                    current = ensure_table_for_read(item)?;
                }
            }
        }

        Some(current)
    }

    fn preserve_decor(existing: &TomlItem, replacement: &mut TomlItem) {
        match (existing, replacement) {
            (TomlItem::Table(existing_table), TomlItem::Table(replacement_table)) => {
                replacement_table
                    .decor_mut()
                    .clone_from(existing_table.decor());
                for (key, existing_item) in existing_table.iter() {
                    if let (Some(existing_key), Some(mut replacement_key)) =
                        (existing_table.key(key), replacement_table.key_mut(key))
                    {
                        replacement_key
                            .leaf_decor_mut()
                            .clone_from(existing_key.leaf_decor());
                        replacement_key
                            .dotted_decor_mut()
                            .clone_from(existing_key.dotted_decor());
                    }
                    if let Some(replacement_item) = replacement_table.get_mut(key) {
                        Self::preserve_decor(existing_item, replacement_item);
                    }
                }
            }
            (TomlItem::Value(existing_value), TomlItem::Value(replacement_value)) => {
                replacement_value
                    .decor_mut()
                    .clone_from(existing_value.decor());
            }
            _ => {}
        }
    }
}

fn new_implicit_table() -> TomlTable {
    let mut table = TomlTable::new();
    table.set_implicit(true);
    table
}

fn ensure_table_for_write(item: &mut TomlItem) -> Option<&mut TomlTable> {
    if item.is_none() {
        *item = TomlItem::Table(new_implicit_table());
    }
    item.as_table_mut()
}

fn ensure_table_for_read(item: &mut TomlItem) -> Option<&mut TomlTable> {
    item.as_table_mut()
}

fn apply_blocking_to_resolved_file(
    resolved_config_file: &Path,
    edits: &[TerminalConfigEdit],
) -> anyhow::Result<()> {
    if edits.is_empty() {
        return Ok(());
    }

    let write_paths = resolve_symlink_write_paths(resolved_config_file)?;
    let serialized = match write_paths.read_path {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err.into()),
        },
        None => String::new(),
    };

    let doc = if serialized.is_empty() {
        DocumentMut::new()
    } else {
        serialized.parse::<DocumentMut>()?
    };

    let mut document = TerminalConfigDocument::new(doc);
    let mut mutated = false;

    for edit in edits {
        mutated |= document.apply(edit)?;
    }

    if !mutated {
        return Ok(());
    }

    write_atomically(&write_paths.write_path, &document.doc.to_string()).with_context(|| {
        format!(
            "failed to persist terminal config at {}",
            write_paths.write_path.display()
        )
    })?;

    Ok(())
}

#[derive(Default)]
pub struct TerminalConfigEditsBuilder {
    config_path: PathBuf,
    edits: Vec<TerminalConfigEdit>,
}

impl TerminalConfigEditsBuilder {
    pub fn for_config_path(config_path: &Path) -> Self {
        Self {
            config_path: config_path.to_path_buf(),
            edits: Vec::new(),
        }
    }

    pub fn set_model_availability_nux_count(mut self, shown_count: &HashMap<String, u32>) -> Self {
        self.edits
            .extend(model_availability_nux_count_edits(shown_count));
        self
    }

    pub fn with_edits<I>(mut self, edits: I) -> Self
    where
        I: IntoIterator<Item = TerminalConfigEdit>,
    {
        self.edits.extend(edits);
        self
    }

    pub fn apply_blocking(self) -> anyhow::Result<()> {
        apply_blocking_to_resolved_file(&self.config_path, &self.edits)
    }

    pub async fn apply(self) -> anyhow::Result<()> {
        task::spawn_blocking(move || {
            apply_blocking_to_resolved_file(&self.config_path, &self.edits)
        })
        .await
        .context("terminal config persistence task panicked")?
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
