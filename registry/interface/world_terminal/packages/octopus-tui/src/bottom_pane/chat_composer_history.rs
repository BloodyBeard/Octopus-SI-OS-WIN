use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::MentionBinding;
use crate::mention_codec::decode_history_mentions_with_at_mentions;
use octopus_world_terminal_client::TextElement;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HistoryEntry {
    pub(crate) text: String,
    pub(crate) text_elements: Vec<TextElement>,
    pub(crate) local_image_paths: Vec<PathBuf>,
    pub(crate) remote_image_urls: Vec<String>,
    pub(crate) mention_bindings: Vec<MentionBinding>,
    pub(crate) pending_pastes: Vec<(String, String)>,
}

impl HistoryEntry {
    pub(crate) fn new(text: String) -> Self {
        Self::new_with_at_mentions(text, true)
    }

    pub(crate) fn new_with_at_mentions(text: String, at_mentions_enabled: bool) -> Self {
        let decoded = decode_history_mentions_with_at_mentions(&text, at_mentions_enabled);
        Self {
            text: decoded.text,
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            mention_bindings: decoded
                .mentions
                .into_iter()
                .map(|mention| MentionBinding {
                    sigil: mention.sigil,
                    mention: mention.mention,
                    path: mention.path,
                })
                .collect(),
            pending_pastes: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_pending(
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        pending_pastes: Vec<(String, String)>,
    ) -> Self {
        Self {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls: Vec::new(),
            mention_bindings: Vec::new(),
            pending_pastes,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_pending_and_remote(
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        pending_pastes: Vec<(String, String)>,
        remote_image_urls: Vec<String>,
    ) -> Self {
        Self {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings: Vec::new(),
            pending_pastes,
        }
    }
}

pub(crate) struct ChatComposerHistory {
    persistent_log_id: Option<u64>,
    persistent_entry_count: usize,

    local_history: Vec<HistoryEntry>,
    replay_seeded_history: Vec<HistoryEntry>,

    fetched_history: HashMap<usize, HistoryEntry>,

    history_cursor: Option<isize>,
    pending_navigation_direction: Option<HistorySearchDirection>,

    last_history_text: Option<String>,

    search: Option<HistorySearchState>,
    at_mention_restore_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistorySearchDirection {
    Older,
    Newer,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HistorySearchResult {
    Found(HistoryEntry),
    Pending,
    AtBoundary,
    NotFound,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HistoryEntryResponse {
    Found(HistoryEntry),
    Search(HistorySearchResult),
    Ignored,
}

#[derive(Clone, Debug)]
struct HistorySearchState {
    query: String,
    query_lower: String,
    selected_offset: Option<usize>,
    unique_matches: Vec<UniqueHistoryMatch>,
    selected_match_index: Option<usize>,
    seen_texts: HashSet<String>,
    awaiting: Option<PendingHistorySearch>,
    exhausted_older: bool,
    exhausted_newer: bool,
}

#[derive(Clone, Debug)]
struct UniqueHistoryMatch {
    offset: usize,
    entry: HistoryEntry,
}

#[derive(Clone, Copy, Debug)]
struct PendingHistorySearch {
    offset: usize,
    direction: HistorySearchDirection,
    boundary_if_exhausted: bool,
}

impl ChatComposerHistory {
    pub fn new() -> Self {
        Self {
            persistent_log_id: None,
            persistent_entry_count: 0,
            local_history: Vec::new(),
            replay_seeded_history: Vec::new(),
            fetched_history: HashMap::new(),
            history_cursor: None,
            pending_navigation_direction: None,
            last_history_text: None,
            search: None,
            at_mention_restore_enabled: false,
        }
    }

    pub fn set_at_mention_restore_enabled(&mut self, enabled: bool) {
        if self.at_mention_restore_enabled == enabled {
            return;
        }
        self.at_mention_restore_enabled = enabled;
        self.fetched_history.clear();
        self.history_cursor = None;
        self.last_history_text = None;
        self.search = None;
    }

    pub fn set_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.persistent_log_id = Some(log_id);
        self.persistent_entry_count = entry_count;
        self.fetched_history.clear();
        self.local_history.clear();
        self.replay_seeded_history.clear();
        self.history_cursor = None;
        self.pending_navigation_direction = None;
        self.last_history_text = None;
        self.search = None;
    }

    pub fn record_local_submission(&mut self, entry: HistoryEntry) {
        self.record_local_submission_inner(entry);
    }

    pub fn record_replayed_submission(&mut self, entry: HistoryEntry) {
        if self.record_local_submission_inner(entry.clone()) {
            self.replay_seeded_history.push(entry);
        }
    }

    fn record_local_submission_inner(&mut self, entry: HistoryEntry) -> bool {
        if entry.text.is_empty()
            && entry.text_elements.is_empty()
            && entry.local_image_paths.is_empty()
            && entry.remote_image_urls.is_empty()
            && entry.mention_bindings.is_empty()
            && entry.pending_pastes.is_empty()
        {
            return false;
        }
        self.history_cursor = None;
        self.pending_navigation_direction = None;
        self.last_history_text = None;
        self.search = None;

        if self.local_history.last().is_some_and(|prev| prev == &entry) {
            return false;
        }

        self.local_history.push(entry);
        true
    }

    pub fn reset_navigation(&mut self) {
        self.history_cursor = None;
        self.pending_navigation_direction = None;
        self.last_history_text = None;
        self.search = None;
    }

    pub fn reset_search(&mut self) {
        self.search = None;
    }

    pub fn should_handle_navigation(&self, text: &str, cursor: usize) -> bool {
        if self.persistent_entry_count == 0 && self.local_history.is_empty() {
            return false;
        }

        if text.is_empty() {
            return true;
        }

        if cursor != 0 && cursor != text.len() {
            return false;
        }

        matches!(&self.last_history_text, Some(prev) if prev == text)
    }

    pub fn navigate_up(&mut self, app_event_tx: &AppEventSender) -> Option<HistoryEntry> {
        self.search = None;
        let total_entries = self.persistent_entry_count + self.local_history.len();
        if total_entries == 0 {
            return None;
        }

        let next_idx = match self.history_cursor {
            None => (total_entries as isize) - 1,
            Some(0) => return None,
            Some(idx) => idx - 1,
        };

        self.history_cursor = Some(next_idx);
        self.populate_history_at_index(
            next_idx as usize,
            HistorySearchDirection::Older,
            app_event_tx,
        )
    }

    pub fn navigate_down(&mut self, app_event_tx: &AppEventSender) -> Option<HistoryEntry> {
        self.search = None;
        let total_entries = self.persistent_entry_count + self.local_history.len();
        if total_entries == 0 {
            return None;
        }

        let next_idx_opt = match self.history_cursor {
            None => return None,
            Some(idx) if (idx as usize) + 1 >= total_entries => None,
            Some(idx) => Some(idx + 1),
        };

        match next_idx_opt {
            Some(idx) => {
                self.history_cursor = Some(idx);
                self.populate_history_at_index(
                    idx as usize,
                    HistorySearchDirection::Newer,
                    app_event_tx,
                )
            }
            None => {
                self.history_cursor = None;
                self.pending_navigation_direction = None;
                self.last_history_text = None;
                Some(HistoryEntry::new(String::new()))
            }
        }
    }

    pub fn on_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
        app_event_tx: &AppEventSender,
    ) -> HistoryEntryResponse {
        if self.persistent_log_id != Some(log_id) {
            return HistoryEntryResponse::Ignored;
        }

        let entry = entry.map(|entry| {
            HistoryEntry::new_with_at_mentions(entry, self.at_mention_restore_enabled)
        });
        if let Some(entry) = entry.clone() {
            self.fetched_history.insert(offset, entry);
        }

        if self
            .search
            .as_ref()
            .and_then(|search| search.awaiting)
            .is_some_and(|pending| pending.offset == offset)
        {
            let pending = self
                .search
                .as_ref()
                .and_then(|search| search.awaiting)
                .unwrap_or(PendingHistorySearch {
                    offset,
                    direction: HistorySearchDirection::Older,
                    boundary_if_exhausted: false,
                });
            if let Some(entry) = entry
                && self.search_matches(&entry)
                && self.search_result_is_unique(&entry)
            {
                return HistoryEntryResponse::Search(self.search_match(offset, entry));
            }
            return HistoryEntryResponse::Search(self.advance_search_after(
                offset,
                pending.direction,
                pending.boundary_if_exhausted,
                app_event_tx,
            ));
        }

        if self.history_cursor == Some(offset as isize) {
            let direction = self.pending_navigation_direction.take();
            let Some(entry) = entry else {
                return HistoryEntryResponse::Ignored;
            };
            if self.persistent_entry_duplicates_local(&entry)
                && let Some(direction) = direction
            {
                let Some(offset) = self.next_history_offset(offset, direction) else {
                    return HistoryEntryResponse::Ignored;
                };
                self.history_cursor = Some(offset as isize);
                return self
                    .populate_history_at_index(offset, direction, app_event_tx)
                    .map(HistoryEntryResponse::Found)
                    .unwrap_or(HistoryEntryResponse::Ignored);
            }
            self.last_history_text = Some(entry.text.clone());
            return HistoryEntryResponse::Found(entry);
        }

        HistoryEntryResponse::Ignored
    }

    pub fn search(
        &mut self,
        query: &str,
        direction: HistorySearchDirection,
        restart: bool,
        app_event_tx: &AppEventSender,
    ) -> HistorySearchResult {
        let total_entries = self.total_entries();
        if total_entries == 0 {
            self.search = Some(HistorySearchState::new(query));
            return HistorySearchResult::NotFound;
        }

        let query_changed = self
            .search
            .as_ref()
            .is_none_or(|search| search.query != query);
        if !query_changed
            && !restart
            && self
                .search
                .as_ref()
                .and_then(|search| search.awaiting)
                .is_some()
        {
            return HistorySearchResult::Pending;
        }

        if query_changed || restart || self.search.is_none() {
            self.search = Some(HistorySearchState::new(query));
        } else if let Some(search) = self.search.as_mut() {
            search.awaiting = None;
        }

        let boundary_if_exhausted = !restart
            && self
                .search
                .as_ref()
                .and_then(|search| search.selected_offset)
                .is_some();
        if !restart
            && !query_changed
            && let Some(result) = self.select_cached_unique_match(direction)
        {
            return result;
        }
        if boundary_if_exhausted
            && self
                .search
                .as_ref()
                .is_some_and(|search| search.is_exhausted(direction))
        {
            return HistorySearchResult::AtBoundary;
        }

        let start_offset =
            self.search_start_offset(total_entries, direction, query_changed || restart);
        let Some(start_offset) = start_offset else {
            return self.exhausted_search_result(direction, boundary_if_exhausted);
        };

        let result =
            self.advance_search_from(start_offset, direction, boundary_if_exhausted, app_event_tx);
        if matches!(result, HistorySearchResult::NotFound) {
            self.exhausted_search_result(direction, boundary_if_exhausted)
        } else {
            result
        }
    }

    fn total_entries(&self) -> usize {
        self.persistent_entry_count + self.local_history.len()
    }

    fn search_start_offset(
        &self,
        total_entries: usize,
        direction: HistorySearchDirection,
        restart: bool,
    ) -> Option<usize> {
        let selected = self
            .search
            .as_ref()
            .and_then(|search| search.selected_offset);
        match direction {
            HistorySearchDirection::Older => {
                if restart {
                    total_entries.checked_sub(1)
                } else {
                    selected.and_then(|offset| offset.checked_sub(1))
                }
            }
            HistorySearchDirection::Newer => {
                if restart {
                    Some(0)
                } else {
                    selected
                        .and_then(|offset| offset.checked_add(1))
                        .filter(|offset| *offset < total_entries)
                }
            }
        }
    }

    fn advance_search_after(
        &mut self,
        offset: usize,
        direction: HistorySearchDirection,
        boundary_if_exhausted: bool,
        app_event_tx: &AppEventSender,
    ) -> HistorySearchResult {
        let next_offset = match direction {
            HistorySearchDirection::Older => offset.checked_sub(1),
            HistorySearchDirection::Newer => offset
                .checked_add(1)
                .filter(|next| *next < self.total_entries()),
        };
        let Some(next_offset) = next_offset else {
            return self.exhausted_search_result(direction, boundary_if_exhausted);
        };
        let result =
            self.advance_search_from(next_offset, direction, boundary_if_exhausted, app_event_tx);
        if matches!(result, HistorySearchResult::NotFound) {
            self.exhausted_search_result(direction, boundary_if_exhausted)
        } else {
            result
        }
    }

    fn advance_search_from(
        &mut self,
        mut offset: usize,
        direction: HistorySearchDirection,
        boundary_if_exhausted: bool,
        app_event_tx: &AppEventSender,
    ) -> HistorySearchResult {
        let total_entries = self.total_entries();
        while offset < total_entries {
            if let Some(entry) = self.entry_at_cached_offset(offset) {
                if self.search_matches(&entry) && self.search_result_is_unique(&entry) {
                    return self.search_match(offset, entry);
                }
            } else if offset < self.persistent_entry_count
                && let Some(log_id) = self.persistent_log_id
            {
                if let Some(search) = self.search.as_mut() {
                    search.awaiting = Some(PendingHistorySearch {
                        offset,
                        direction,
                        boundary_if_exhausted,
                    });
                }
                app_event_tx.send(AppEvent::LookupMessageHistoryEntry { offset, log_id });
                return HistorySearchResult::Pending;
            }

            let next_offset = match direction {
                HistorySearchDirection::Older => offset.checked_sub(1),
                HistorySearchDirection::Newer => {
                    offset.checked_add(1).filter(|next| *next < total_entries)
                }
            };
            let Some(next_offset) = next_offset else {
                return HistorySearchResult::NotFound;
            };
            offset = next_offset;
        }

        HistorySearchResult::NotFound
    }

    fn entry_at_cached_offset(&self, offset: usize) -> Option<HistoryEntry> {
        if offset >= self.persistent_entry_count {
            self.local_history
                .get(offset - self.persistent_entry_count)
                .cloned()
        } else {
            self.fetched_history.get(&offset).cloned()
        }
    }

    fn search_matches(&self, entry: &HistoryEntry) -> bool {
        let Some(search) = self.search.as_ref() else {
            return false;
        };
        search.query.is_empty() || entry.text.to_lowercase().contains(&search.query_lower)
    }

    fn search_result_is_unique(&self, entry: &HistoryEntry) -> bool {
        self.search
            .as_ref()
            .is_none_or(|search| !search.seen_texts.contains(entry.text.as_str()))
    }

    fn search_match(&mut self, offset: usize, entry: HistoryEntry) -> HistorySearchResult {
        self.history_cursor = Some(offset as isize);
        self.last_history_text = Some(entry.text.clone());
        if let Some(search) = self.search.as_mut() {
            search.selected_offset = Some(offset);
            search.record_match(offset, &entry);
            search.awaiting = None;
            search.exhausted_older = false;
            search.exhausted_newer = false;
        }
        HistorySearchResult::Found(entry)
    }

    fn select_cached_unique_match(
        &mut self,
        direction: HistorySearchDirection,
    ) -> Option<HistorySearchResult> {
        let next_index = {
            let search = self.search.as_ref()?;
            let selected_index = search.selected_match_index?;
            match direction {
                HistorySearchDirection::Older => {
                    let next_index = selected_index + 1;
                    (next_index < search.unique_matches.len()).then_some(next_index)?
                }
                HistorySearchDirection::Newer => selected_index.checked_sub(1)?,
            }
        };

        let history_match = self.search.as_ref()?.unique_matches[next_index].clone();
        self.history_cursor = Some(history_match.offset as isize);
        self.last_history_text = Some(history_match.entry.text.clone());
        if let Some(search) = self.search.as_mut() {
            search.select_match(next_index);
        }
        Some(HistorySearchResult::Found(history_match.entry))
    }

    fn exhausted_search_result(
        &mut self,
        direction: HistorySearchDirection,
        boundary_if_exhausted: bool,
    ) -> HistorySearchResult {
        if let Some(search) = self.search.as_mut() {
            search.awaiting = None;
            if boundary_if_exhausted {
                search.mark_exhausted(direction);
            }
        }

        if boundary_if_exhausted {
            HistorySearchResult::AtBoundary
        } else {
            HistorySearchResult::NotFound
        }
    }

    fn populate_history_at_index(
        &mut self,
        global_idx: usize,
        direction: HistorySearchDirection,
        app_event_tx: &AppEventSender,
    ) -> Option<HistoryEntry> {
        let mut global_idx = global_idx;
        loop {
            if let Some(entry) = self.entry_at_cached_offset(global_idx) {
                if global_idx < self.persistent_entry_count
                    && self.persistent_entry_duplicates_local(&entry)
                {
                    let Some(next_idx) = self.next_history_offset(global_idx, direction) else {
                        self.pending_navigation_direction = None;
                        return None;
                    };
                    self.history_cursor = Some(next_idx as isize);
                    global_idx = next_idx;
                    continue;
                }
                self.pending_navigation_direction = None;
                self.last_history_text = Some(entry.text.clone());
                return Some(entry);
            }

            if global_idx >= self.persistent_entry_count {
                return None;
            }

            if let Some(log_id) = self.persistent_log_id {
                self.pending_navigation_direction = Some(direction);
                app_event_tx.send(AppEvent::LookupMessageHistoryEntry {
                    offset: global_idx,
                    log_id,
                });
            }
            return None;
        }
    }

    fn next_history_offset(
        &self,
        offset: usize,
        direction: HistorySearchDirection,
    ) -> Option<usize> {
        match direction {
            HistorySearchDirection::Older => offset.checked_sub(1),
            HistorySearchDirection::Newer => offset
                .checked_add(1)
                .filter(|next| *next < self.total_entries()),
        }
    }

    fn persistent_entry_duplicates_local(&self, entry: &HistoryEntry) -> bool {
        self.replay_seeded_history.iter().any(|local_entry| {
            local_entry.text == entry.text && local_entry.mention_bindings == entry.mention_bindings
        })
    }
}

impl HistorySearchState {
    fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
            query_lower: query.to_lowercase(),
            selected_offset: None,
            unique_matches: Vec::new(),
            selected_match_index: None,
            seen_texts: HashSet::new(),
            awaiting: None,
            exhausted_older: false,
            exhausted_newer: false,
        }
    }

    fn is_exhausted(&self, direction: HistorySearchDirection) -> bool {
        match direction {
            HistorySearchDirection::Older => self.exhausted_older,
            HistorySearchDirection::Newer => self.exhausted_newer,
        }
    }

    fn mark_exhausted(&mut self, direction: HistorySearchDirection) {
        match direction {
            HistorySearchDirection::Older => self.exhausted_older = true,
            HistorySearchDirection::Newer => self.exhausted_newer = true,
        }
    }

    fn record_match(&mut self, offset: usize, entry: &HistoryEntry) {
        if let Some(index) = self
            .unique_matches
            .iter()
            .position(|history_match| history_match.offset == offset)
        {
            self.select_match(index);
            return;
        }

        self.seen_texts.insert(entry.text.clone());
        let insert_index = self
            .unique_matches
            .partition_point(|history_match| history_match.offset > offset);
        self.unique_matches.insert(
            insert_index,
            UniqueHistoryMatch {
                offset,
                entry: entry.clone(),
            },
        );
        self.select_match(insert_index);
    }

    fn select_match(&mut self, index: usize) {
        let Some(history_match) = self.unique_matches.get(index) else {
            return;
        };
        self.selected_offset = Some(history_match.offset);
        self.selected_match_index = Some(index);
        self.awaiting = None;
        self.exhausted_older = false;
        self.exhausted_newer = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn duplicate_submissions_are_not_recorded() {
        let mut history = ChatComposerHistory::new();

        history.record_local_submission(HistoryEntry::new(String::new()));
        assert_eq!(history.local_history.len(), 0);

        history.record_local_submission(HistoryEntry::new("hello".to_string()));
        assert_eq!(history.local_history.len(), 1);
        assert_eq!(
            history.local_history.last().unwrap(),
            &HistoryEntry::new("hello".to_string())
        );

        history.record_local_submission(HistoryEntry::new("hello".to_string()));
        assert_eq!(history.local_history.len(), 1);

        history.record_local_submission(HistoryEntry::new("world".to_string()));
        assert_eq!(history.local_history.len(), 2);
        assert_eq!(
            history.local_history.last().unwrap(),
            &HistoryEntry::new("world".to_string())
        );
    }

    #[test]
    fn persistent_restore_gates_at_mentions() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);
        let mut history = ChatComposerHistory::new();
        history.set_metadata(42, 1);

        assert!(history.navigate_up(&tx).is_none());
        let disabled = history.on_entry_response(
            42,
            0,
            Some("[$sample](/tmp/user/sample/SKILL.md) and [$figma](app://figma)".to_string()),
            &tx,
        );
        assert_eq!(
            disabled,
            HistoryEntryResponse::Found(HistoryEntry {
                text: "$sample and $figma".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
                mention_bindings: vec![
                    MentionBinding {
                        sigil: '$',
                        mention: "sample".to_string(),
                        path: "/tmp/user/sample/SKILL.md".to_string(),
                    },
                    MentionBinding {
                        sigil: '$',
                        mention: "figma".to_string(),
                        path: "app://figma".to_string(),
                    },
                ],
                pending_pastes: Vec::new(),
            })
        );

        history.set_at_mention_restore_enabled(true);
        assert!(history.navigate_up(&tx).is_none());
        let enabled = history.on_entry_response(
            42,
            0,
            Some("[@sample](/tmp/user/sample/SKILL.md) and [$figma](app://figma)".to_string()),
            &tx,
        );
        assert_eq!(
            enabled,
            HistoryEntryResponse::Found(HistoryEntry {
                text: "@sample and $figma".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
                mention_bindings: vec![
                    MentionBinding {
                        sigil: '@',
                        mention: "sample".to_string(),
                        path: "/tmp/user/sample/SKILL.md".to_string(),
                    },
                    MentionBinding {
                        sigil: '$',
                        mention: "figma".to_string(),
                        path: "app://figma".to_string(),
                    },
                ],
                pending_pastes: Vec::new(),
            })
        );
    }

    #[test]
    fn navigation_with_async_fetch() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.set_metadata(1, 3);
        history.record_local_submission(HistoryEntry::new("latest".to_string()));

        assert!(history.should_handle_navigation("", 0));
        assert_eq!(
            Some(HistoryEntry::new("latest".to_string())),
            history.navigate_up(&tx)
        );

        assert!(history.navigate_up(&tx).is_none());

        let event = rx.try_recv().expect("expected AppEvent to be sent");
        let AppEvent::LookupMessageHistoryEntry { offset, log_id } = event else {
            panic!("unexpected event variant");
        };
        assert_eq!(offset, 2);
        assert_eq!(log_id, 1);

        assert_eq!(
            HistoryEntryResponse::Found(HistoryEntry::new("latest".to_string())),
            history.on_entry_response(1, 2, Some("latest".into()), &tx)
        );

        assert!(history.navigate_up(&tx).is_none());

        let event2 = rx.try_recv().expect("expected second event");
        let AppEvent::LookupMessageHistoryEntry { offset, log_id } = event2 else {
            panic!("unexpected event variant");
        };
        assert_eq!(offset, 1);
        assert_eq!(log_id, 1);

        assert_eq!(
            HistoryEntryResponse::Found(HistoryEntry::new("older".to_string())),
            history.on_entry_response(1, 1, Some("older".into()), &tx)
        );
    }

    #[test]
    fn search_matches_local_history_and_stops_at_boundaries() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.record_local_submission(HistoryEntry::new("git status".to_string()));
        history.record_local_submission(HistoryEntry::new("cargo test -p codex-tui".to_string()));
        history.record_local_submission(HistoryEntry::new("git diff".to_string()));

        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git diff".to_string())),
            history.search("git", HistorySearchDirection::Older, true, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git status".to_string())),
            history.search("git", HistorySearchDirection::Older, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::AtBoundary,
            history.search("git", HistorySearchDirection::Older, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::AtBoundary,
            history.search("git", HistorySearchDirection::Older, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git diff".to_string())),
            history.search("git", HistorySearchDirection::Newer, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::AtBoundary,
            history.search("git", HistorySearchDirection::Newer, false, &tx)
        );
    }

    #[test]
    fn search_skips_duplicate_local_matches() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.record_local_submission(HistoryEntry::new("git status".to_string()));
        history.record_local_submission(HistoryEntry::new("cargo test -p codex-tui".to_string()));
        history.record_local_submission(HistoryEntry::new("git status".to_string()));
        history.record_local_submission(HistoryEntry::new("git diff".to_string()));

        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git diff".to_string())),
            history.search("git", HistorySearchDirection::Older, true, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git status".to_string())),
            history.search("git", HistorySearchDirection::Older, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::AtBoundary,
            history.search("git", HistorySearchDirection::Older, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git diff".to_string())),
            history.search("git", HistorySearchDirection::Newer, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("git status".to_string())),
            history.search("git", HistorySearchDirection::Older, false, &tx)
        );
    }

    #[test]
    fn repeated_boundary_search_does_not_refetch_persistent_history() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.set_metadata(1, 3);

        assert_eq!(
            HistorySearchResult::Pending,
            history.search("needle", HistorySearchDirection::Older, true, &tx)
        );
        let _ = rx.try_recv().expect("expected latest lookup");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Found(HistoryEntry::new(
                "needle latest".to_string()
            ))),
            history.on_entry_response(1, 2, Some("needle latest".into()), &tx,)
        );

        assert_eq!(
            HistorySearchResult::Pending,
            history.search("needle", HistorySearchDirection::Older, false, &tx)
        );
        let _ = rx.try_recv().expect("expected next older lookup");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Pending),
            history.on_entry_response(1, 1, Some("not a match".into()), &tx,)
        );
        let _ = rx.try_recv().expect("expected oldest lookup");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::AtBoundary),
            history.on_entry_response(1, 0, Some("also not a match".into()), &tx,)
        );
        assert!(rx.try_recv().is_err());

        assert_eq!(
            HistorySearchResult::AtBoundary,
            history.search("needle", HistorySearchDirection::Older, false, &tx)
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn search_fetches_persistent_history_until_match() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.set_metadata(1, 3);

        assert_eq!(
            HistorySearchResult::Pending,
            history.search("older", HistorySearchDirection::Older, true, &tx)
        );
        let AppEvent::LookupMessageHistoryEntry { offset, log_id } =
            rx.try_recv().expect("expected latest lookup")
        else {
            panic!("unexpected event variant");
        };
        assert_eq!(offset, 2);
        assert_eq!(log_id, 1);

        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Pending),
            history.on_entry_response(1, 2, Some("latest".into()), &tx)
        );
        let AppEvent::LookupMessageHistoryEntry { offset, log_id } =
            rx.try_recv().expect("expected next lookup")
        else {
            panic!("unexpected event variant");
        };
        assert_eq!(offset, 1);
        assert_eq!(log_id, 1);

        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Found(HistoryEntry::new(
                "older command".to_string()
            ))),
            history.on_entry_response(1, 1, Some("older command".into()), &tx)
        );
    }

    #[test]
    fn search_skips_duplicate_persistent_matches() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.set_metadata(1, 4);

        assert_eq!(
            HistorySearchResult::Pending,
            history.search("needle", HistorySearchDirection::Older, true, &tx)
        );
        let _ = rx.try_recv().expect("expected latest lookup");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Found(HistoryEntry::new(
                "needle same".to_string()
            ))),
            history.on_entry_response(1, 3, Some("needle same".into()), &tx,)
        );

        assert_eq!(
            HistorySearchResult::Pending,
            history.search("needle", HistorySearchDirection::Older, false, &tx)
        );
        let _ = rx.try_recv().expect("expected duplicate lookup");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Pending),
            history.on_entry_response(1, 2, Some("needle same".into()), &tx,)
        );
        let _ = rx.try_recv().expect("expected next lookup after duplicate");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Pending),
            history.on_entry_response(1, 1, Some("not a match".into()), &tx,)
        );
        let _ = rx.try_recv().expect("expected oldest lookup");
        assert_eq!(
            HistoryEntryResponse::Search(HistorySearchResult::Found(HistoryEntry::new(
                "needle older".to_string()
            ))),
            history.on_entry_response(1, 0, Some("needle older".into()), &tx,)
        );
        assert_eq!(
            HistorySearchResult::AtBoundary,
            history.search("needle", HistorySearchDirection::Older, false, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("needle same".to_string())),
            history.search("needle", HistorySearchDirection::Newer, false, &tx)
        );
    }

    #[test]
    fn search_is_case_insensitive_and_empty_query_finds_latest() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.record_local_submission(HistoryEntry::new("Build Release".to_string()));

        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("Build Release".to_string())),
            history.search("release", HistorySearchDirection::Older, true, &tx)
        );
        assert_eq!(
            HistorySearchResult::Found(HistoryEntry::new("Build Release".to_string())),
            history.search("", HistorySearchDirection::Older, true, &tx)
        );
    }

    #[test]
    fn reset_navigation_resets_cursor() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx);

        let mut history = ChatComposerHistory::new();
        history.set_metadata(1, 3);
        history
            .fetched_history
            .insert(1, HistoryEntry::new("command2".to_string()));
        history
            .fetched_history
            .insert(2, HistoryEntry::new("command3".to_string()));

        assert_eq!(
            Some(HistoryEntry::new("command3".to_string())),
            history.navigate_up(&tx)
        );
        assert_eq!(
            Some(HistoryEntry::new("command2".to_string())),
            history.navigate_up(&tx)
        );

        history.reset_navigation();
        assert!(history.history_cursor.is_none());
        assert!(history.last_history_text.is_none());

        assert_eq!(
            Some(HistoryEntry::new("command3".to_string())),
            history.navigate_up(&tx)
        );
    }

    #[test]
    fn should_handle_navigation_when_cursor_is_at_line_boundaries() {
        let mut history = ChatComposerHistory::new();
        history.record_local_submission(HistoryEntry::new("hello".to_string()));
        history.last_history_text = Some("hello".to_string());

        assert!(history.should_handle_navigation("hello", 0));
        assert!(history.should_handle_navigation("hello", "hello".len()));
        assert!(!history.should_handle_navigation("hello", 1));
        assert!(!history.should_handle_navigation("other", 0));
    }
}
