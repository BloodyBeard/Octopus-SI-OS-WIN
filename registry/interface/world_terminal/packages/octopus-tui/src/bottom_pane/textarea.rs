use crate::key_hint::KeyBindingListExt;
use crate::key_hint::is_altgr;
use crate::keymap::EditorKeymap;
use crate::keymap::RuntimeKeymap;
use crate::keymap::VimNormalKeymap;
use crate::keymap::VimOperatorKeymap;
use crate::keymap::VimTextObjectKeymap;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use octopus_world_terminal_client::ByteRange;
use octopus_world_terminal_client::TextElement as UserTextElement;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;
use std::cell::Ref;
use std::cell::RefCell;
use std::ops::Range;
use textwrap::Options;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod vim;
use self::vim::VimMode;
use self::vim::VimMotion;
use self::vim::VimOperator;
use self::vim::VimPending;
use self::vim::VimTextObjectScope;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

fn split_word_pieces(run: &str) -> Vec<(usize, &str)> {
    let mut pieces = Vec::new();
    for (segment_start, segment) in run.split_word_bound_indices() {
        let mut piece_start = 0;
        let mut chars = segment.char_indices();
        let Some((_, first_char)) = chars.next() else {
            continue;
        };
        let mut in_separator = is_word_separator(first_char);

        for (idx, ch) in chars {
            let is_separator = is_word_separator(ch);
            if is_separator == in_separator {
                continue;
            }
            pieces.push((segment_start + piece_start, &segment[piece_start..idx]));
            piece_start = idx;
            in_separator = is_separator;
        }

        pieces.push((segment_start + piece_start, &segment[piece_start..]));
    }

    pieces
}

#[derive(Debug, Clone)]
struct TextElement {
    id: u64,
    range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextElementSnapshot {
    pub(crate) id: u64,
    pub(crate) range: Range<usize>,
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) struct TextArea {
    text: String,
    cursor_pos: usize,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_col: Option<usize>,
    elements: Vec<TextElement>,
    next_element_id: u64,
    kill_buffer: String,
    kill_buffer_kind: KillBufferKind,
    vim_enabled: bool,
    vim_mode: VimMode,
    vim_pending: VimPending,
    editor_keymap: EditorKeymap,
    vim_normal_keymap: VimNormalKeymap,
    vim_operator_keymap: VimOperatorKeymap,
    vim_text_object_keymap: VimTextObjectKeymap,
}

#[derive(Debug, Clone)]
struct WrapCache {
    width: u16,
    lines: Vec<Range<usize>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TextAreaState {
    scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KillBufferKind {
    Characterwise,
    Linewise,
}

impl TextArea {
    pub fn new() -> Self {
        let defaults = RuntimeKeymap::defaults();
        Self {
            text: String::new(),
            cursor_pos: 0,
            wrap_cache: RefCell::new(None),
            preferred_col: None,
            elements: Vec::new(),
            next_element_id: 1,
            kill_buffer: String::new(),
            kill_buffer_kind: KillBufferKind::Characterwise,
            vim_enabled: false,
            vim_mode: VimMode::Insert,
            vim_pending: VimPending::None,
            editor_keymap: defaults.editor,
            vim_normal_keymap: defaults.vim_normal,
            vim_operator_keymap: defaults.vim_operator,
            vim_text_object_keymap: defaults.vim_text_object,
        }
    }

    pub fn set_keymap_bindings(&mut self, keymap: &RuntimeKeymap) {
        self.editor_keymap = keymap.editor.clone();
        self.vim_normal_keymap = keymap.vim_normal.clone();
        self.vim_operator_keymap = keymap.vim_operator.clone();
        self.vim_text_object_keymap = keymap.vim_text_object.clone();
    }

    pub fn set_text_clearing_elements(&mut self, text: &str) {
        self.set_text_inner(text, None);
    }

    pub fn set_text_with_elements(&mut self, text: &str, elements: &[UserTextElement]) {
        self.set_text_inner(text, Some(elements));
    }

    fn set_text_inner(&mut self, text: &str, elements: Option<&[UserTextElement]>) {
        self.text = text.to_string();
        self.cursor_pos = self.cursor_pos.clamp(0, self.text.len());
        self.elements.clear();
        if let Some(elements) = elements {
            for elem in elements {
                let mut start = elem.byte_range.start.min(self.text.len());
                let mut end = elem.byte_range.end.min(self.text.len());
                start = self.clamp_pos_to_char_boundary(start);
                end = self.clamp_pos_to_char_boundary(end);
                if start >= end {
                    continue;
                }
                let id = self.next_element_id();
                self.elements.push(TextElement {
                    id,
                    range: start..end,
                });
            }
            self.elements.sort_by_key(|e| e.range.start);
        }
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
    }

    pub(crate) fn set_vim_enabled(&mut self, enabled: bool) {
        self.vim_enabled = enabled;
        self.vim_pending = VimPending::None;
        self.vim_mode = if enabled {
            VimMode::Normal
        } else {
            VimMode::Insert
        };
    }

    pub(crate) fn is_vim_enabled(&self) -> bool {
        self.vim_enabled
    }

    pub(crate) fn is_vim_normal_mode(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Normal
    }

    pub(crate) fn vim_normal_end_cursor(&self) -> usize {
        if self.text.is_empty() {
            0
        } else {
            self.prev_atomic_boundary(self.text.len())
        }
    }

    pub(crate) fn is_vim_operator_pending(&self) -> bool {
        !matches!(self.vim_pending, VimPending::None)
    }

    pub(crate) fn enter_vim_insert_mode(&mut self) {
        if self.vim_enabled {
            self.vim_mode = VimMode::Insert;
            self.vim_pending = VimPending::None;
        }
    }

    pub(crate) fn enter_vim_normal_mode(&mut self) {
        if self.vim_enabled {
            self.vim_mode = VimMode::Normal;
            self.vim_pending = VimPending::None;
            self.preferred_col = None;
        }
    }

    pub(crate) fn allows_paste_burst(&self) -> bool {
        !self.vim_enabled || self.vim_mode == VimMode::Insert
    }

    pub(crate) fn uses_vim_insert_cursor(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Insert
    }

    pub(crate) fn should_handle_vim_insert_escape(&self, event: KeyEvent) -> bool {
        self.vim_enabled
            && self.vim_mode == VimMode::Insert
            && event.code == KeyCode::Esc
            && event.modifiers == KeyModifiers::NONE
            && matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
    }

    pub(crate) fn vim_mode_label(&self) -> Option<&'static str> {
        if !self.vim_enabled {
            return None;
        }
        Some(match self.vim_mode {
            VimMode::Normal => "Normal",
            VimMode::Insert => "Insert",
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn insert_str(&mut self, text: &str) {
        self.insert_str_at(self.cursor_pos, text);
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        let pos = self.clamp_pos_for_insertion(pos);
        self.text.insert_str(pos, text);
        self.wrap_cache.replace(None);
        if pos <= self.cursor_pos {
            self.cursor_pos += text.len();
        }
        self.shift_elements(pos, 0, text.len());
        self.preferred_col = None;
    }

    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        let range = self.expand_range_to_element_boundaries(range);
        self.replace_range_raw(range, text);
    }

    fn replace_range_raw(&mut self, range: std::ops::Range<usize>, text: &str) {
        assert!(range.start <= range.end);
        let start = range.start.clamp(0, self.text.len());
        let end = range.end.clamp(0, self.text.len());
        let removed_len = end - start;
        let inserted_len = text.len();
        if removed_len == 0 && inserted_len == 0 {
            return;
        }
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, text);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.update_elements_after_replace(start, end, inserted_len);

        self.cursor_pos = if self.cursor_pos < start {
            self.cursor_pos
        } else if self.cursor_pos <= end {
            start + inserted_len
        } else {
            ((self.cursor_pos as isize) + diff) as usize
        }
        .min(self.text.len());

        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = pos.clamp(0, self.text.len());
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_lines(width).len() as u16
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_state(area, TextAreaState::default())
    }

    pub fn cursor_pos_with_state(&self, area: Rect, state: TextAreaState) -> Option<(u16, u16)> {
        let lines = self.wrapped_lines(area.width);
        let effective_scroll = self.effective_scroll(area.height, &lines, state.scroll);
        let i = Self::wrapped_line_index_by_start(&lines, self.cursor_pos)?;
        let ls = &lines[i];
        let col = self.text[ls.start..self.cursor_pos].width() as u16;
        let screen_row = i
            .saturating_sub(effective_scroll as usize)
            .try_into()
            .unwrap_or(0);
        Some((area.x + col, area.y + screen_row))
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn current_display_col(&self) -> usize {
        let bol = self.beginning_of_current_line();
        self.text[bol..self.cursor_pos].width()
    }

    fn wrapped_line_index_by_start(lines: &[Range<usize>], pos: usize) -> Option<usize> {
        let idx = lines.partition_point(|r| r.start <= pos);
        if idx == 0 { None } else { Some(idx - 1) }
    }

    fn move_to_display_col_on_line(
        &mut self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) {
        let mut width_so_far = 0usize;
        for (i, g) in self.text[line_start..line_end].grapheme_indices(true) {
            width_so_far += g.width();
            if width_so_far > target_col {
                self.cursor_pos = line_start + i;
                self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
                return;
            }
        }
        self.cursor_pos = line_end;
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    fn beginning_of_line(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
    fn beginning_of_current_line(&self) -> usize {
        self.beginning_of_line(self.cursor_pos)
    }

    fn first_non_blank_of_current_line(&self) -> usize {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        self.text[bol..eol]
            .char_indices()
            .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(bol + offset))
            .unwrap_or(eol)
    }

    fn end_of_line(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map(|i| i + pos)
            .unwrap_or(self.text.len())
    }
    fn end_of_current_line(&self) -> usize {
        self.end_of_line(self.cursor_pos)
    }

    pub fn input(&mut self, event: KeyEvent) {
        if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if self.vim_enabled {
            self.handle_vim_input(event);
        } else {
            let keymap = self.editor_keymap.clone();
            self.input_with_keymap(event, &keymap);
        }
    }

    pub fn input_with_keymap(&mut self, event: KeyEvent, keymap: &EditorKeymap) {
        if keymap.insert_newline.is_pressed(event) {
            self.insert_str("\n");
            return;
        }

        if keymap.delete_backward_word.is_pressed(event) {
            self.delete_backward_word();
            return;
        }

        if let KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            ..
        } = event
            && is_altgr(modifiers)
        {
            self.insert_str(&c.to_string());
            return;
        }

        if keymap.delete_backward.is_pressed(event) {
            self.delete_backward(1);
            return;
        }
        if keymap.delete_forward_word.is_pressed(event) {
            self.delete_forward_word();
            return;
        }
        if keymap.delete_forward.is_pressed(event) {
            self.delete_forward(1);
            return;
        }
        if keymap.kill_line_start.is_pressed(event) {
            self.kill_to_beginning_of_line();
            return;
        }
        if keymap.kill_whole_line.is_pressed(event) {
            self.kill_current_line();
            return;
        }
        if keymap.kill_line_end.is_pressed(event) {
            self.kill_to_end_of_line();
            return;
        }
        if keymap.yank.is_pressed(event) {
            self.yank();
            return;
        }
        if keymap.move_word_left.is_pressed(event) {
            self.set_cursor(self.beginning_of_previous_word());
            return;
        }
        if keymap.move_word_right.is_pressed(event) {
            self.set_cursor(self.end_of_next_word());
            return;
        }
        if keymap.move_left.is_pressed(event) {
            self.move_cursor_left();
            return;
        }
        if keymap.move_right.is_pressed(event) {
            self.move_cursor_right();
            return;
        }
        if keymap.move_up.is_pressed(event) {
            self.move_cursor_up();
            return;
        }
        if keymap.move_down.is_pressed(event) {
            self.move_cursor_down();
            return;
        }
        if keymap.move_line_start.is_pressed(event) {
            let move_up_at_bol = matches!(
                event,
                KeyEvent {
                    code: KeyCode::Char('a'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
            );
            self.move_cursor_to_beginning_of_line(move_up_at_bol);
            return;
        }
        if keymap.move_line_end.is_pressed(event) {
            let move_down_at_eol = matches!(
                event,
                KeyEvent {
                    code: KeyCode::Char('e'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
            );
            self.move_cursor_to_end_of_line(move_down_at_eol);
            return;
        }

        if let KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
            ..
        } = event
        {
            if c.is_ascii_control() {
                return;
            }
            self.insert_str(&c.to_string());
        }

        tracing::debug!("Unhandled key event in TextArea: {:?}", event);
    }

    fn handle_vim_input(&mut self, event: KeyEvent) {
        match self.vim_mode {
            VimMode::Insert => self.handle_vim_insert(event),
            VimMode::Normal => self.handle_vim_normal(event),
        }
    }

    fn handle_vim_insert(&mut self, event: KeyEvent) {
        if matches!(event.code, KeyCode::Esc) {
            let bol = self.beginning_of_current_line();
            if self.cursor_pos > bol {
                self.cursor_pos = self.prev_atomic_boundary(self.cursor_pos).max(bol);
            }
            self.enter_vim_normal_mode();
            return;
        }
        let keymap = self.editor_keymap.clone();
        self.input_with_keymap(event, &keymap);
    }

    fn handle_vim_normal(&mut self, event: KeyEvent) {
        let pending = std::mem::replace(&mut self.vim_pending, VimPending::None);
        match pending {
            VimPending::None => {}
            VimPending::Operator(op) => {
                self.handle_vim_operator(op, event);
                return;
            }
            VimPending::TextObject { operator, scope } => {
                self.handle_vim_text_object(operator, scope, event);
                return;
            }
        }

        if self.vim_normal_keymap.enter_insert.is_pressed(event) {
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.append_after_cursor.is_pressed(event) {
            let next = self.next_atomic_boundary(self.cursor_pos);
            self.set_cursor(next);
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.append_line_end.is_pressed(event) {
            self.set_cursor(self.end_of_current_line());
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.insert_line_start.is_pressed(event) {
            self.set_cursor(self.first_non_blank_of_current_line());
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.open_line_below.is_pressed(event) {
            let eol = self.end_of_current_line();
            let old_len = self.text.len();
            let insert_at = if eol < old_len { eol + 1 } else { eol };
            self.insert_str_at(insert_at, "\n");
            let cursor = if eol < old_len {
                insert_at
            } else {
                insert_at + 1
            };
            self.set_cursor(cursor);
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.open_line_above.is_pressed(event) {
            let bol = self.beginning_of_current_line();
            self.insert_str_at(bol, "\n");
            self.set_cursor(bol);
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.move_left.is_pressed(event) {
            self.move_cursor_left();
            return;
        }
        if self.vim_normal_keymap.move_right.is_pressed(event) {
            self.move_cursor_right();
            return;
        }
        if self.vim_normal_keymap.move_down.is_pressed(event) {
            self.move_cursor_down();
            return;
        }
        if self.vim_normal_keymap.move_up.is_pressed(event) {
            self.move_cursor_up();
            return;
        }
        if self.vim_normal_keymap.move_word_forward.is_pressed(event) {
            self.set_cursor(self.beginning_of_next_word());
            return;
        }
        if self.vim_normal_keymap.move_word_backward.is_pressed(event) {
            self.set_cursor(self.beginning_of_previous_word());
            return;
        }
        if self.vim_normal_keymap.move_word_end.is_pressed(event) {
            self.set_cursor(self.vim_word_end_cursor());
            return;
        }
        if self.vim_normal_keymap.move_line_start.is_pressed(event) {
            self.set_cursor(self.beginning_of_current_line());
            return;
        }
        if self.vim_normal_keymap.move_line_end.is_pressed(event) {
            self.set_cursor(self.vim_line_end_cursor());
            return;
        }
        if self.vim_normal_keymap.delete_char.is_pressed(event) {
            self.delete_forward_kill(1);
            return;
        }
        if self.vim_normal_keymap.substitute_char.is_pressed(event) {
            if self.cursor_pos < self.end_of_current_line() {
                self.delete_forward_kill(1);
            }
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.delete_to_line_end.is_pressed(event) {
            self.vim_kill_to_end_of_line();
            return;
        }
        if self.vim_normal_keymap.change_to_line_end.is_pressed(event) {
            self.vim_kill_to_end_of_line();
            self.vim_mode = VimMode::Insert;
            return;
        }
        if self.vim_normal_keymap.yank_line.is_pressed(event) {
            self.yank_current_line();
            return;
        }
        if self.vim_normal_keymap.paste_after.is_pressed(event) {
            self.paste_after_cursor();
            return;
        }
        if self
            .vim_normal_keymap
            .start_delete_operator
            .is_pressed(event)
        {
            self.vim_pending = VimPending::Operator(VimOperator::Delete);
            return;
        }
        if self.vim_normal_keymap.start_yank_operator.is_pressed(event) {
            self.vim_pending = VimPending::Operator(VimOperator::Yank);
            return;
        }
        if self
            .vim_normal_keymap
            .start_change_operator
            .is_pressed(event)
        {
            self.vim_pending = VimPending::Operator(VimOperator::Change);
            return;
        }
        if self.vim_normal_keymap.cancel_operator.is_pressed(event) {
            self.vim_pending = VimPending::None;
        }
    }

    fn handle_vim_operator(&mut self, op: VimOperator, event: KeyEvent) -> bool {
        if op == VimOperator::Delete && self.vim_operator_keymap.delete_line.is_pressed(event) {
            self.kill_current_line();
            return true;
        }
        if op == VimOperator::Yank && self.vim_operator_keymap.yank_line.is_pressed(event) {
            self.yank_current_line();
            return true;
        }
        if self.vim_operator_keymap.cancel.is_pressed(event) {
            return true;
        }
        if let Some(scope) = self.vim_text_object_scope_for_event(event) {
            self.vim_pending = VimPending::TextObject {
                operator: op,
                scope,
            };
            return true;
        }

        if op != VimOperator::Change
            && let Some(motion) = self.vim_motion_for_event(event)
        {
            self.apply_vim_operator(op, motion);
            return true;
        }
        false
    }

    fn handle_vim_text_object(
        &mut self,
        op: VimOperator,
        scope: VimTextObjectScope,
        event: KeyEvent,
    ) -> bool {
        if self.vim_text_object_keymap.cancel.is_pressed(event) {
            return true;
        }
        let Some(object) = self.vim_text_object_for_event(event) else {
            return false;
        };
        if let Some(range) = self.text_object_range(object, scope) {
            self.apply_vim_operator_to_range(op, range);
        }
        true
    }

    fn vim_motion_for_event(&self, event: KeyEvent) -> Option<VimMotion> {
        if self.vim_operator_keymap.motion_left.is_pressed(event) {
            return Some(VimMotion::Left);
        }
        if self.vim_operator_keymap.motion_right.is_pressed(event) {
            return Some(VimMotion::Right);
        }
        if self.vim_operator_keymap.motion_down.is_pressed(event) {
            return Some(VimMotion::Down);
        }
        if self.vim_operator_keymap.motion_up.is_pressed(event) {
            return Some(VimMotion::Up);
        }
        if self
            .vim_operator_keymap
            .motion_word_forward
            .is_pressed(event)
        {
            return Some(VimMotion::WordForward);
        }
        if self
            .vim_operator_keymap
            .motion_word_backward
            .is_pressed(event)
        {
            return Some(VimMotion::WordBackward);
        }
        if self.vim_operator_keymap.motion_word_end.is_pressed(event) {
            return Some(VimMotion::WordEnd);
        }
        if self.vim_operator_keymap.motion_line_start.is_pressed(event) {
            return Some(VimMotion::LineStart);
        }
        if self.vim_operator_keymap.motion_line_end.is_pressed(event) {
            return Some(VimMotion::LineEnd);
        }
        None
    }

    fn apply_vim_operator(&mut self, op: VimOperator, motion: VimMotion) {
        let Some(range) = self.range_for_motion(motion) else {
            return;
        };
        match op {
            VimOperator::Delete => self.kill_range(range),
            VimOperator::Yank => self.yank_range(range),
            VimOperator::Change => {}
        }
    }

    fn apply_vim_operator_to_range(&mut self, op: VimOperator, range: Range<usize>) {
        match op {
            VimOperator::Delete => self.kill_range(range),
            VimOperator::Yank => self.yank_range(range),
            VimOperator::Change => {
                self.kill_range(range);
                self.vim_mode = VimMode::Insert;
            }
        }
    }

    fn range_for_motion(&mut self, motion: VimMotion) -> Option<Range<usize>> {
        if matches!(motion, VimMotion::Up | VimMotion::Down) {
            return self.linewise_range_for_vertical_motion(motion);
        }
        let start = self.cursor_pos;
        let target = self.target_for_motion(motion);
        if start == target {
            return None;
        }
        let (range_start, range_end) = if target < start {
            (target, start)
        } else {
            (start, target)
        };
        Some(range_start..range_end)
    }

    fn linewise_range_for_vertical_motion(&self, motion: VimMotion) -> Option<Range<usize>> {
        let current = self.current_line_range_with_newline();
        let range = match motion {
            VimMotion::Up => {
                let start = if current.start == 0 {
                    current.start
                } else {
                    self.beginning_of_line(current.start.saturating_sub(1))
                };
                start..current.end
            }
            VimMotion::Down => {
                let end = if current.end >= self.text.len() {
                    current.end
                } else {
                    let next_eol = self.end_of_line(current.end);
                    if next_eol < self.text.len() {
                        next_eol + 1
                    } else {
                        next_eol
                    }
                };
                current.start..end
            }
            VimMotion::Left
            | VimMotion::Right
            | VimMotion::WordForward
            | VimMotion::WordBackward
            | VimMotion::WordEnd
            | VimMotion::LineStart
            | VimMotion::LineEnd => return None,
        };
        (range.start < range.end).then_some(range)
    }

    fn target_for_motion(&mut self, motion: VimMotion) -> usize {
        let original_cursor = self.cursor_pos;
        let original_preferred = self.preferred_col;
        match motion {
            VimMotion::Left => self.move_cursor_left(),
            VimMotion::Right => self.move_cursor_right(),
            VimMotion::Up => self.move_cursor_up(),
            VimMotion::Down => self.move_cursor_down(),
            VimMotion::WordForward => self.set_cursor(self.beginning_of_next_word()),
            VimMotion::WordBackward => self.set_cursor(self.beginning_of_previous_word()),
            VimMotion::WordEnd => self.set_cursor(self.vim_word_end_exclusive()),
            VimMotion::LineStart => self.set_cursor(self.beginning_of_current_line()),
            VimMotion::LineEnd => self.set_cursor(self.end_of_current_line()),
        }
        let target = self.cursor_pos;
        self.cursor_pos = original_cursor;
        self.preferred_col = original_preferred;
        target
    }

    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos == 0 {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.prev_atomic_boundary(target);
            if target == 0 {
                break;
            }
        }
        self.replace_range(target..self.cursor_pos, "");
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.replace_range(self.cursor_pos..target, "");
    }

    pub fn delete_forward_kill(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.kill_range(self.cursor_pos..target);
    }

    pub fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor_pos);
    }

    pub fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor_pos {
            self.kill_range(self.cursor_pos..end);
        }
    }

    pub fn kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        let range = if self.cursor_pos == eol {
            if eol < self.text.len() {
                Some(self.cursor_pos..eol + 1)
            } else {
                None
            }
        } else {
            Some(self.cursor_pos..eol)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    fn vim_kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        if self.cursor_pos < eol {
            self.kill_range(self.cursor_pos..eol);
        }
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let range = if self.cursor_pos == bol {
            if bol > 0 { Some(bol - 1..bol) } else { None }
        } else {
            Some(bol..self.cursor_pos)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    fn kill_range(&mut self, range: Range<usize>) {
        self.kill_range_with_kind(range, KillBufferKind::Characterwise);
    }

    fn kill_line_range(&mut self, range: Range<usize>) {
        self.kill_range_with_kind(range, KillBufferKind::Linewise);
    }

    fn kill_range_with_kind(&mut self, range: Range<usize>, kind: KillBufferKind) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }

        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() {
            return;
        }

        self.store_kill_buffer(removed, kind);
        self.replace_range_raw(range, "");
    }

    fn yank_range(&mut self, range: Range<usize>) {
        self.yank_range_with_kind(range, KillBufferKind::Characterwise);
    }

    fn yank_line_range(&mut self, range: Range<usize>) {
        self.yank_range_with_kind(range, KillBufferKind::Linewise);
    }

    fn yank_range_with_kind(&mut self, range: Range<usize>, kind: KillBufferKind) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }
        let removed = self.text[range].to_string();
        if removed.is_empty() {
            return;
        }
        self.store_kill_buffer(removed, kind);
    }

    fn store_kill_buffer(&mut self, text: String, kind: KillBufferKind) {
        self.kill_buffer = text;
        self.kill_buffer_kind = kind;
    }

    fn paste_after_cursor(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        if self.kill_buffer_kind == KillBufferKind::Linewise {
            self.paste_line_after_current_line();
            return;
        }
        let insert_at = self.next_atomic_boundary(self.cursor_pos);
        self.set_cursor(insert_at);
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    fn paste_line_after_current_line(&mut self) {
        let eol = self.end_of_current_line();
        let insert_at = if eol < self.text.len() { eol + 1 } else { eol };
        let cursor = if eol < self.text.len() {
            insert_at
        } else {
            insert_at + 1
        };
        let text = if eol < self.text.len() {
            if self.kill_buffer.ends_with('\n') {
                self.kill_buffer.clone()
            } else {
                format!("{}\n", self.kill_buffer)
            }
        } else {
            format!("\n{}", self.kill_buffer.trim_end_matches('\n'))
        };
        self.insert_str_at(insert_at, &text);
        self.set_cursor(cursor.min(self.text.len()));
    }

    fn yank_current_line(&mut self) {
        let range = self.current_line_range_with_newline();
        self.yank_line_range(range);
    }

    fn kill_current_line(&mut self) {
        let range = self.current_line_range_with_newline();
        self.kill_line_range(range);
    }

    fn current_line_range_with_newline(&self) -> Range<usize> {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        let end = if eol < self.text.len() { eol + 1 } else { eol };
        bol..end
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = self.prev_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = self.next_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_up(&mut self) {
        if let Some((target_col, maybe_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx > 0 {
                        let prev = &lines[idx - 1];
                        let line_start = prev.start;
                        let line_end = prev.end.saturating_sub(1);
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match maybe_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    self.cursor_pos = 0;
                    self.preferred_col = None;
                    return;
                }
            }
        }

        if let Some(prev_nl) = self.text[..self.cursor_pos].rfind('\n') {
            let target_col = match self.preferred_col {
                Some(c) => c,
                None => {
                    let c = self.current_display_col();
                    self.preferred_col = Some(c);
                    c
                }
            };
            let prev_line_start = self.text[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev_line_end = prev_nl;
            self.move_to_display_col_on_line(prev_line_start, prev_line_end, target_col);
        } else {
            self.cursor_pos = 0;
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_down(&mut self) {
        if let Some((target_col, move_to_last)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx + 1 < lines.len() {
                        let next = &lines[idx + 1];
                        let line_start = next.start;
                        let line_end = next.end.saturating_sub(1);
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match move_to_last {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    self.cursor_pos = self.text.len();
                    self.preferred_col = None;
                    return;
                }
            }
        }

        let target_col = match self.preferred_col {
            Some(c) => c,
            None => {
                let c = self.current_display_col();
                self.preferred_col = Some(c);
                c
            }
        };
        if let Some(next_nl) = self.text[self.cursor_pos..]
            .find('\n')
            .map(|i| i + self.cursor_pos)
        {
            let next_line_start = next_nl + 1;
            let next_line_end = self.text[next_line_start..]
                .find('\n')
                .map(|i| i + next_line_start)
                .unwrap_or(self.text.len());
            self.move_to_display_col_on_line(next_line_start, next_line_end, target_col);
        } else {
            self.cursor_pos = self.text.len();
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_to_beginning_of_line(&mut self, move_up_at_bol: bool) {
        let bol = self.beginning_of_current_line();
        if move_up_at_bol && self.cursor_pos == bol {
            self.set_cursor(self.beginning_of_line(self.cursor_pos.saturating_sub(1)));
        } else {
            self.set_cursor(bol);
        }
        self.preferred_col = None;
    }

    pub fn move_cursor_to_end_of_line(&mut self, move_down_at_eol: bool) {
        let eol = self.end_of_current_line();
        if move_down_at_eol && self.cursor_pos == eol {
            let next_pos = (self.cursor_pos.saturating_add(1)).min(self.text.len());
            self.set_cursor(self.end_of_line(next_pos));
        } else {
            self.set_cursor(eol);
        }
    }

    pub fn element_payloads(&self) -> Vec<String> {
        self.elements
            .iter()
            .filter_map(|e| self.text.get(e.range.clone()).map(str::to_string))
            .collect()
    }

    pub fn text_elements(&self) -> Vec<UserTextElement> {
        self.elements
            .iter()
            .map(|e| {
                let placeholder = self.text.get(e.range.clone()).map(str::to_string);
                UserTextElement::new(
                    ByteRange {
                        start: e.range.start,
                        end: e.range.end,
                    },
                    placeholder,
                )
            })
            .collect()
    }

    pub(crate) fn text_element_snapshots(&self) -> Vec<TextElementSnapshot> {
        self.elements
            .iter()
            .filter_map(|element| {
                self.text
                    .get(element.range.clone())
                    .map(|text| TextElementSnapshot {
                        id: element.id,
                        range: element.range.clone(),
                        text: text.to_string(),
                    })
            })
            .collect()
    }

    pub(crate) fn element_id_for_exact_range(&self, range: Range<usize>) -> Option<u64> {
        self.elements
            .iter()
            .find(|element| element.range == range)
            .map(|element| element.id)
    }

    pub fn replace_element_payload(&mut self, old: &str, new: &str) -> bool {
        let Some(idx) = self
            .elements
            .iter()
            .position(|e| self.text.get(e.range.clone()) == Some(old))
        else {
            return false;
        };

        let range = self.elements[idx].range.clone();
        let start = range.start;
        let end = range.end;
        if start > end || end > self.text.len() {
            return false;
        }

        let removed_len = end - start;
        let inserted_len = new.len();
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, new);
        self.wrap_cache.replace(None);
        self.preferred_col = None;

        self.elements[idx].range = start..(start + inserted_len);

        if diff != 0 {
            for (j, e) in self.elements.iter_mut().enumerate() {
                if j == idx {
                    continue;
                }
                if e.range.end <= start {
                    continue;
                }
                if e.range.start >= end {
                    e.range.start = ((e.range.start as isize) + diff) as usize;
                    e.range.end = ((e.range.end as isize) + diff) as usize;
                    continue;
                }

                e.range.start = start.min(e.range.start);
                e.range.end = (start + inserted_len).max(e.range.end.saturating_add_signed(diff));
            }
        }

        self.cursor_pos = if self.cursor_pos < start {
            self.cursor_pos
        } else if self.cursor_pos <= end {
            start + inserted_len
        } else {
            ((self.cursor_pos as isize) + diff) as usize
        };
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);

        self.elements.sort_by_key(|e| e.range.start);

        true
    }

    pub fn insert_element(&mut self, text: &str) -> u64 {
        let start = self.clamp_pos_for_insertion(self.cursor_pos);
        self.insert_str_at(start, text);
        let end = start + text.len();
        let id = self.add_element(start..end);
        self.set_cursor(end);
        id
    }

    fn add_element(&mut self, range: Range<usize>) -> u64 {
        let id = self.next_element_id();
        self.elements.push(TextElement { id, range });
        self.elements.sort_by_key(|e| e.range.start);
        id
    }

    pub fn add_element_range(&mut self, range: Range<usize>) -> Option<u64> {
        let start = self.clamp_pos_to_char_boundary(range.start.min(self.text.len()));
        let end = self.clamp_pos_to_char_boundary(range.end.min(self.text.len()));
        if start >= end {
            return None;
        }
        if self
            .elements
            .iter()
            .any(|e| e.range.start == start && e.range.end == end)
        {
            return None;
        }
        if self
            .elements
            .iter()
            .any(|e| start < e.range.end && end > e.range.start)
        {
            return None;
        }
        let id = self.add_element(start..end);
        Some(id)
    }

    pub fn remove_element_range(&mut self, range: Range<usize>) -> bool {
        let start = self.clamp_pos_to_char_boundary(range.start.min(self.text.len()));
        let end = self.clamp_pos_to_char_boundary(range.end.min(self.text.len()));
        if start >= end {
            return false;
        }
        let len_before = self.elements.len();
        self.elements
            .retain(|elem| elem.range.start != start || elem.range.end != end);
        len_before != self.elements.len()
    }

    fn next_element_id(&mut self) -> u64 {
        let id = self.next_element_id;
        self.next_element_id = self.next_element_id.saturating_add(1);
        id
    }
    fn find_element_containing(&self, pos: usize) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| pos > e.range.start && pos < e.range.end)
    }

    fn clamp_pos_to_char_boundary(&self, pos: usize) -> usize {
        let pos = pos.min(self.text.len());
        if self.text.is_char_boundary(pos) {
            return pos;
        }
        let mut prev = pos;
        while prev > 0 && !self.text.is_char_boundary(prev) {
            prev -= 1;
        }
        let mut next = pos;
        while next < self.text.len() && !self.text.is_char_boundary(next) {
            next += 1;
        }
        if pos.saturating_sub(prev) <= next.saturating_sub(pos) {
            prev
        } else {
            next
        }
    }

    fn clamp_pos_to_nearest_boundary(&self, pos: usize) -> usize {
        let pos = self.clamp_pos_to_char_boundary(pos);
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                self.clamp_pos_to_char_boundary(e.range.start)
            } else {
                self.clamp_pos_to_char_boundary(e.range.end)
            }
        } else {
            pos
        }
    }

    fn clamp_pos_for_insertion(&self, pos: usize) -> usize {
        let pos = self.clamp_pos_to_char_boundary(pos);
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                self.clamp_pos_to_char_boundary(e.range.start)
            } else {
                self.clamp_pos_to_char_boundary(e.range.end)
            }
        } else {
            pos
        }
    }

    fn expand_range_to_element_boundaries(&self, mut range: Range<usize>) -> Range<usize> {
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range.start = new_start;
                        range.end = new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
            } else if e.range.start >= end {
                e.range.start = ((e.range.start as isize) + diff) as usize;
                e.range.end = ((e.range.end as isize) + diff) as usize;
            } else {
                let new_start = at.min(e.range.start);
                let new_end = at + inserted.max(e.range.end.saturating_sub(end));
                e.range.start = new_start;
                e.range.end = new_end;
            }
        }
    }

    fn update_elements_after_replace(&mut self, start: usize, end: usize, inserted_len: usize) {
        self.shift_elements(start, end.saturating_sub(start), inserted_len);
    }

    fn prev_atomic_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos > e.range.start && pos <= e.range.end)
        {
            return self.elements[idx].range.start;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.prev_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.start
                } else {
                    b
                }
            }
            Ok(None) => 0,
            Err(_) => pos.saturating_sub(1),
        }
    }

    fn next_atomic_boundary(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos >= e.range.start && pos < e.range.end)
        {
            return self.elements[idx].range.end;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.next_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.end
                } else {
                    b
                }
            }
            Ok(None) => self.text.len(),
            Err(_) => pos.saturating_add(1),
        }
    }

    pub(crate) fn beginning_of_previous_word(&self) -> usize {
        let prefix = &self.text[..self.cursor_pos];
        let Some((first_non_ws_idx, ch)) = prefix
            .char_indices()
            .rev()
            .find(|&(_, ch)| !ch.is_whitespace())
        else {
            return 0;
        };
        let run_start = prefix[..first_non_ws_idx]
            .char_indices()
            .rev()
            .find(|&(_, ch)| ch.is_whitespace())
            .map_or(0, |(idx, ch)| idx + ch.len_utf8());
        let run_end = first_non_ws_idx + ch.len_utf8();
        let pieces = split_word_pieces(&prefix[run_start..run_end]);
        let mut pieces = pieces.into_iter().rev().peekable();
        let Some((piece_start, piece)) = pieces.next() else {
            return run_start;
        };
        let mut start = run_start + piece_start;

        if piece.chars().all(is_word_separator) {
            while let Some((idx, piece)) = pieces.peek() {
                if !piece.chars().all(is_word_separator) {
                    break;
                }
                start = run_start + *idx;
                pieces.next();
            }
        }

        self.adjust_pos_out_of_elements(start, true)
    }

    pub(crate) fn end_of_next_word(&self) -> usize {
        self.end_of_next_word_from(self.cursor_pos)
    }

    fn end_of_next_word_from(&self, cursor_pos: usize) -> usize {
        let suffix = &self.text[cursor_pos..];
        let Some(first_non_ws) = suffix.find(|ch: char| !ch.is_whitespace()) else {
            return self.text.len();
        };
        let run = &suffix[first_non_ws..];
        let run = &run[..run.find(char::is_whitespace).unwrap_or(run.len())];
        let mut pieces = split_word_pieces(run).into_iter().peekable();
        let Some((start, piece)) = pieces.next() else {
            return cursor_pos + first_non_ws;
        };
        let word_start = cursor_pos + first_non_ws + start;
        let mut end = word_start + piece.len();
        if piece.chars().all(is_word_separator) {
            while let Some((idx, piece)) = pieces.peek() {
                if !piece.chars().all(is_word_separator) {
                    break;
                }
                end = cursor_pos + first_non_ws + *idx + piece.len();
                pieces.next();
            }
        }

        self.adjust_pos_out_of_elements(end, false)
    }

    fn vim_word_end_exclusive(&self) -> usize {
        let end = self.end_of_next_word();
        let target = if end > self.cursor_pos {
            self.prev_atomic_boundary(end)
        } else {
            end
        };
        if target == self.cursor_pos && end < self.text.len() {
            self.end_of_next_word_from(end)
        } else {
            end
        }
    }

    fn vim_word_end_cursor(&self) -> usize {
        let end = self.vim_word_end_exclusive();
        if end > self.cursor_pos {
            self.prev_atomic_boundary(end)
        } else {
            end
        }
    }

    fn vim_line_end_cursor(&self) -> usize {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        if eol > bol {
            self.prev_atomic_boundary(eol).max(bol)
        } else {
            eol
        }
    }

    pub(crate) fn beginning_of_next_word(&self) -> usize {
        let Some(first_non_ws) = self.text[self.cursor_pos..].find(|c: char| !c.is_whitespace())
        else {
            return self.text.len();
        };
        let word_start = self.cursor_pos + first_non_ws;
        if word_start != self.cursor_pos {
            return self.adjust_pos_out_of_elements(word_start, true);
        }
        let end = self.end_of_next_word();
        if end >= self.text.len() {
            return self.text.len();
        }
        let Some(next_non_ws) = self.text[end..].find(|c: char| !c.is_whitespace()) else {
            return self.text.len();
        };
        self.adjust_pos_out_of_elements(end + next_non_ws, true)
    }

    fn adjust_pos_out_of_elements(&self, pos: usize, prefer_start: bool) -> usize {
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            if prefer_start {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    #[expect(clippy::unwrap_used)]
    fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {
        {
            let mut cache = self.wrap_cache.borrow_mut();
            let needs_recalc = match cache.as_ref() {
                Some(c) => c.width != width,
                None => true,
            };
            if needs_recalc {
                let lines = crate::wrapping::wrap_ranges(
                    &self.text,
                    Options::new(width as usize).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
                );
                *cache = Some(WrapCache { width, lines });
            }
        }

        let cache = self.wrap_cache.borrow();
        Ref::map(cache, |c| &c.as_ref().unwrap().lines)
    }

    fn effective_scroll(
        &self,
        area_height: u16,
        lines: &[Range<usize>],
        current_scroll: u16,
    ) -> u16 {
        let total_lines = lines.len() as u16;
        if area_height >= total_lines {
            return 0;
        }

        let cursor_line_idx =
            Self::wrapped_line_index_by_start(lines, self.cursor_pos).unwrap_or(0) as u16;

        let max_scroll = total_lines.saturating_sub(area_height);
        let mut scroll = current_scroll.min(max_scroll);

        if cursor_line_idx < scroll {
            scroll = cursor_line_idx;
        } else if cursor_line_idx >= scroll + area_height {
            scroll = cursor_line_idx + 1 - area_height;
        }
        scroll
    }
}

impl WidgetRef for &TextArea {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.wrapped_lines(area.width);
        self.render_lines(area, buf, &lines, 0..lines.len(), Style::default(), &[]);
    }
}

impl StatefulWidgetRef for &TextArea {
    type State = TextAreaState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines(area, buf, &lines, start..end, Style::default(), &[]);
    }
}

impl TextArea {
    pub(crate) fn render_ref_masked(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut TextAreaState,
        mask_char: char,
    ) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines_masked(area, buf, &lines, start..end, mask_char);
    }

    pub(crate) fn render_ref_styled_with_highlights(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut TextAreaState,
        base_style: Style,
        highlights: &[(Range<usize>, Style)],
    ) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines(area, buf, &lines, start..end, base_style, highlights);
    }

    fn render_lines(
        &self,
        area: Rect,
        buf: &mut Buffer,
        lines: &[Range<usize>],
        range: std::ops::Range<usize>,
        base_style: Style,
        highlights: &[(Range<usize>, Style)],
    ) {
        for (row, idx) in range.enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let line_range = r.start..r.end - 1;
            buf.set_style(Rect::new(area.x, y, area.width, 1), base_style);
            buf.set_string(area.x, y, &self.text[line_range.clone()], base_style);

            for elem in &self.elements {
                let overlap_start = elem.range.start.max(line_range.start);
                let overlap_end = elem.range.end.min(line_range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let styled = &self.text[overlap_start..overlap_end];
                let x_off = self.text[line_range.start..overlap_start].width() as u16;
                let style = base_style.fg(Color::Cyan);
                buf.set_string(area.x + x_off, y, styled, style);
            }

            for (highlight_range, style) in highlights {
                let overlap_start = highlight_range.start.max(line_range.start);
                let overlap_end = highlight_range.end.min(line_range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let highlighted = &self.text[overlap_start..overlap_end];
                let x_off = self.text[line_range.start..overlap_start].width() as u16;
                buf.set_string(area.x + x_off, y, highlighted, *style);
            }
        }
    }

    fn render_lines_masked(
        &self,
        area: Rect,
        buf: &mut Buffer,
        lines: &[Range<usize>],
        range: std::ops::Range<usize>,
        mask_char: char,
    ) {
        for (row, idx) in range.enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let line_range = r.start..r.end - 1;
            let masked = self.text[line_range.clone()]
                .chars()
                .map(|_| mask_char)
                .collect::<String>();
            buf.set_string(area.x, y, &masked, Style::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_hint;
    use pretty_assertions::assert_eq;
    use rand::prelude::*;

    fn rand_grapheme(rng: &mut rand::rngs::StdRng) -> String {
        let r: u8 = rng.random_range(0..100);
        match r {
            0..=4 => "\n".to_string(),
            5..=12 => " ".to_string(),
            13..=35 => (rng.random_range(b'a'..=b'z') as char).to_string(),
            36..=45 => (rng.random_range(b'A'..=b'Z') as char).to_string(),
            46..=52 => (rng.random_range(b'0'..=b'9') as char).to_string(),
            53..=65 => {
                let choices = ["👍", "😊", "🐍", "🚀", "🧪", "🌟"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
            66..=75 => {
                let choices = ["漢", "字", "測", "試", "你", "好", "界", "编", "码"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
            76..=85 => {
                let base = ["e", "a", "o", "n", "u"][rng.random_range(0..5)];
                let marks = ["\u{0301}", "\u{0308}", "\u{0302}", "\u{0303}"];
                format!("{base}{}", marks[rng.random_range(0..marks.len())])
            }
            86..=92 => {
                let choices = ["Ω", "β", "Ж", "ю", "ש", "م", "ह"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
            _ => {
                let choices = ["👩\u{200D}💻", "👨\u{200D}💻", "🏳️\u{200D}🌈"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
        }
    }

    fn ta_with(text: &str) -> TextArea {
        let mut t = TextArea::new();
        t.insert_str(text);
        t
    }

    #[test]
    fn insert_and_replace_update_cursor_and_text() {
        let mut t = ta_with("hello");
        t.set_cursor(5);
        t.insert_str("!");
        assert_eq!(t.text(), "hello!");
        assert_eq!(t.cursor(), 6);

        t.insert_str_at(0, "X");
        assert_eq!(t.text(), "Xhello!");
        assert_eq!(t.cursor(), 7);

        t.set_cursor(1);
        let end = t.text().len();
        t.insert_str_at(end, "Y");
        assert_eq!(t.text(), "Xhello!Y");
        assert_eq!(t.cursor(), 1);

        let mut t = ta_with("abcd");
        t.set_cursor(1);
        t.replace_range(2..3, "Z");
        assert_eq!(t.text(), "abZd");
        assert_eq!(t.cursor(), 1);

        let mut t = ta_with("abcd");
        t.set_cursor(2);
        t.replace_range(1..3, "Q");
        assert_eq!(t.text(), "aQd");
        assert_eq!(t.cursor(), 2);

        let mut t = ta_with("abcd");
        t.set_cursor(4);
        t.replace_range(0..1, "AA");
        assert_eq!(t.text(), "AAbcd");
        assert_eq!(t.cursor(), 5);
    }

    #[test]
    fn insert_str_at_clamps_to_char_boundary() {
        let mut t = TextArea::new();
        t.insert_str("你");
        t.set_cursor(0);
        t.insert_str_at(1, "A");
        assert_eq!(t.text(), "A你");
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn set_text_clamps_cursor_to_char_boundary() {
        let mut t = TextArea::new();
        t.insert_str("abcd");
        t.set_cursor(1);
        t.set_text_clearing_elements("你");
        assert_eq!(t.cursor(), 0);
        t.insert_str("a");
        assert_eq!(t.text(), "a你");
    }

    #[test]
    fn delete_backward_and_forward_edges() {
        let mut t = ta_with("abc");
        t.set_cursor(1);
        t.delete_backward(1);
        assert_eq!(t.text(), "bc");
        assert_eq!(t.cursor(), 0);

        t.set_cursor(0);
        t.delete_backward(1);
        assert_eq!(t.text(), "bc");
        assert_eq!(t.cursor(), 0);

        t.set_cursor(1);
        t.delete_forward(1);
        assert_eq!(t.text(), "b");
        assert_eq!(t.cursor(), 1);

        t.set_cursor(t.text().len());
        t.delete_forward(1);
        assert_eq!(t.text(), "b");
    }

    #[test]
    fn delete_forward_deletes_element_at_left_edge() {
        let mut t = TextArea::new();
        t.insert_str("a");
        t.insert_element("<element>");
        t.insert_str("b");

        let elem_start = t.elements[0].range.start;
        t.set_cursor(elem_start);
        t.delete_forward(1);

        assert_eq!(t.text(), "ab");
        assert_eq!(t.cursor(), elem_start);
    }

    #[test]
    fn vim_insert_and_escape() {
        let mut t = TextArea::new();
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(t.text(), "h");
        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn vim_insert_key_enters_insert_mode() {
        let mut t = TextArea::new();
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Insert, KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));

        assert_eq!(t.text(), "h");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
    }

    #[test]
    fn vim_normal_arrow_keys_move_cursor() {
        let mut t = ta_with("ab\ncd");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(t.cursor(), 2);

        t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(t.cursor(), 5);

        t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(t.cursor(), 4);

        t.input(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn vim_escape_from_insert_at_start_does_not_underflow() {
        let mut t = TextArea::new();
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn vim_escape_from_insert_at_line_start_stays_on_line() {
        let mut t = ta_with("one\ntwo");
        t.set_cursor("one\n".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.cursor(), "one\n".len());
    }

    #[test]
    fn vim_escape_moves_by_grapheme_boundary() {
        let mut t = ta_with("👍👍");
        t.set_cursor(t.text().len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.cursor(), "👍".len());
    }

    #[test]
    fn vim_escape_respects_atomic_element_boundary() {
        let mut t = TextArea::new();
        t.insert_str("a");
        t.insert_element("<element>");
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn vim_shift_i_enters_insert_at_first_non_blank_with_shift_only_binding() {
        let mut t = ta_with("hello\n  world");
        t.vim_normal_keymap.insert_line_start = vec![key_hint::shift(KeyCode::Char('i'))];
        t.set_cursor("hello\n  wor".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE));

        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), "hello\n  ".len());
    }

    #[test]
    fn vim_shift_a_enters_insert_at_line_end_with_shift_only_binding() {
        let mut t = ta_with("hello\nworld");
        t.vim_normal_keymap.append_line_end = vec![key_hint::shift(KeyCode::Char('a'))];
        t.set_cursor(8);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));

        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), 11);
    }

    #[test]
    fn vim_shift_c_changes_to_line_end_and_enters_insert_mode() {
        let mut t = ta_with("hello world\nnext line");
        t.set_cursor(6);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SHIFT));

        assert_eq!(t.text(), "hello \nnext line");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), 6);
        assert_eq!(t.kill_buffer, "world");
    }

    #[test]
    fn vim_uppercase_c_changes_to_line_end() {
        let mut t = ta_with("hello world\nnext line");
        t.set_cursor(6);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello \nnext line");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), 6);
    }

    #[test]
    fn vim_s_substitutes_current_character_and_enters_insert_mode() {
        let mut t = ta_with("abc");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        assert_eq!(t.text(), "ac");
        assert_eq!(t.cursor(), 1);
        assert_eq!(t.vim_mode_label(), Some("Insert"));

        t.input(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));

        assert_eq!(t.text(), "aXc");
        assert_eq!(t.cursor(), 2);
        assert_eq!(t.vim_mode_label(), Some("Insert"));
    }

    #[test]
    fn vim_s_on_empty_line_enters_insert_without_deleting_newline() {
        let mut t = ta_with("before\n\nnext");
        t.set_cursor("before\n".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        assert_eq!(t.text(), "before\n\nnext");
        assert_eq!(t.cursor(), "before\n".len());
        assert_eq!(t.vim_mode_label(), Some("Insert"));
    }

    #[test]
    fn vim_d_at_line_end_does_not_remove_newline() {
        let mut t = ta_with("hello\nworld");
        t.set_cursor("hello".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello\nworld");
        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.kill_buffer, "");
    }

    #[test]
    fn vim_c_at_line_end_enters_insert_without_removing_newline() {
        let mut t = ta_with("hello\nworld");
        t.set_cursor("hello".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello\nworld");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), "hello".len());
        assert_eq!(t.kill_buffer, "");
    }

    #[test]
    fn vim_shift_o_opens_line_above_with_shift_only_binding() {
        let mut t = ta_with("hello\nworld");
        t.vim_normal_keymap.open_line_above = vec![key_hint::shift(KeyCode::Char('o'))];
        t.set_cursor(8);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello\n\nworld");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), 6);
    }

    #[test]
    fn vim_o_opens_line_below_on_inserted_line() {
        let mut t = ta_with("one\ntwo");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        assert_eq!(t.text(), "one\n\ntwo");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), "one\n".len());
    }

    #[test]
    fn vim_o_opens_line_below_final_line_and_moves_to_new_line() {
        let mut t = ta_with("one");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        assert_eq!(t.text(), "one\n");
        assert_eq!(t.vim_mode_label(), Some("Insert"));
        assert_eq!(t.cursor(), "one\n".len());
    }

    #[test]
    fn vim_delete_word() {
        let mut t = ta_with("hello world");
        t.set_cursor(0);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));

        assert_eq!(t.text(), "world");
        assert_eq!(t.kill_buffer, "hello ");
    }

    #[test]
    fn vim_change_inner_word_deletes_word_and_enters_insert() {
        let mut t = ta_with("hello world");
        t.set_cursor("hello ".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello ");
        assert_eq!(t.kill_buffer, "world");
        assert_eq!(t.cursor(), "hello ".len());
        assert_eq!(t.vim_mode_label(), Some("Insert"));
    }

    #[test]
    fn vim_word_text_objects_cover_delete_yank_and_big_word() {
        let mut t = ta_with("hello world");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello world");
        assert_eq!(t.kill_buffer, "hello ");
        assert_eq!(t.vim_mode_label(), Some("Normal"));

        let mut t = ta_with("foo.bar/baz qux");
        t.set_cursor("foo.".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::NONE));

        assert_eq!(t.text(), " qux");
        assert_eq!(t.kill_buffer, "foo.bar/baz");
    }

    #[test]
    fn vim_word_text_objects_accept_cursor_at_word_end() {
        let mut t = ta_with("hello world");
        t.set_cursor("hello".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));

        assert_eq!(t.text(), "world");
        assert_eq!(t.kill_buffer, "hello ");

        let mut t = ta_with("foo bar");
        t.set_cursor(t.text().len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::NONE));

        assert_eq!(t.text(), "foo ");
        assert_eq!(t.kill_buffer, "bar");
        assert_eq!(t.cursor(), "foo ".len());
        assert_eq!(t.vim_mode_label(), Some("Insert"));
    }

    #[test]
    fn vim_delimiter_text_objects_select_innermost_pair_and_aliases() {
        let mut t = ta_with("a(b(c)d)e");
        t.set_cursor("a(b(".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));

        assert_eq!(t.text(), "a(b()d)e");
        assert_eq!(t.kill_buffer, "c");
        assert_eq!(t.vim_mode_label(), Some("Insert"));

        let mut t = ta_with("a [b] c");
        t.set_cursor("a [".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));

        assert_eq!(t.text(), "a  c");
        assert_eq!(t.kill_buffer, "[b]");
    }

    #[test]
    fn vim_empty_inner_text_objects_are_valid_targets() {
        let mut t = ta_with("call()");
        t.set_cursor("call(".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('('), KeyModifiers::NONE));

        assert_eq!(t.text(), "call()");
        assert_eq!(t.kill_buffer, "");
        assert_eq!(t.cursor(), "call(".len());
        assert_eq!(t.vim_mode_label(), Some("Insert"));

        let mut t = ta_with(r#"say "" now"#);
        t.set_cursor(r#"say ""#.len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('"'), KeyModifiers::NONE));

        assert_eq!(t.text(), r#"say "" now"#);
        assert_eq!(t.kill_buffer, "");
        assert_eq!(t.cursor(), r#"say ""#.len());
        assert_eq!(t.vim_mode_label(), Some("Insert"));
    }

    #[test]
    fn vim_quote_text_objects_are_line_local_and_handle_escapes() {
        let mut t = ta_with(r#"say "a \"b\" c" now"#);
        t.set_cursor(r#"say "a \"#.len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('"'), KeyModifiers::SHIFT));

        assert_eq!(t.text(), r#"say "" now"#);
        assert_eq!(t.kill_buffer, r#"a \"b\" c"#);
        assert_eq!(t.vim_mode_label(), Some("Insert"));

        let mut t = ta_with("one \"two\nthree\" four");
        t.set_cursor("one \"two\n".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('"'), KeyModifiers::NONE));

        assert_eq!(t.text(), "one \"two\nthree\" four");
        assert_eq!(t.kill_buffer, "");
    }

    #[test]
    fn vim_text_object_cancellation_and_unsupported_change_motions_do_not_edit() {
        let mut t = ta_with("hello world");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello world");
        assert_eq!(t.kill_buffer, "");
        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert!(!t.is_vim_operator_pending());

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(t.is_vim_operator_pending());
        t.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(t.text(), "hello world");
        assert_eq!(t.kill_buffer, "");
        assert!(!t.is_vim_operator_pending());
    }

    #[test]
    fn vim_operator_invalid_motion_is_consumed() {
        let mut t = ta_with("hello");
        t.set_cursor(0);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(t.is_vim_operator_pending());

        t.input(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));

        assert_eq!(t.text(), "hello");
        assert_eq!(t.vim_mode_label(), Some("Normal"));
        assert_eq!(t.cursor(), 0);
        assert!(!t.is_vim_operator_pending());
    }

    #[test]
    fn vim_e_lands_on_word_end_character() {
        let mut t = ta_with("abc");
        t.set_cursor(0);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        assert_eq!(t.cursor(), 2);

        t.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(t.text(), "ab");
        assert_eq!(t.kill_buffer, "c");
    }

    #[test]
    fn vim_e_advances_from_each_word_end() {
        let mut t = ta_with("alpha beta gamma");
        t.set_cursor("alph".len());
        t.set_vim_enabled(true);
        let mut states = Vec::new();

        for _ in 0..3 {
            t.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
            states.push(format!("{}\n{}^", t.text(), " ".repeat(t.cursor())));
        }

        insta::assert_snapshot!("vim_e_advances_from_each_word_end", states.join("\n\n"));
    }

    #[test]
    fn vim_delete_to_word_end_advances_from_existing_word_end() {
        let mut t = ta_with("alpha beta gamma");
        t.set_cursor("alph".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        assert_eq!(t.text(), "alph gamma");
        assert_eq!(t.kill_buffer, "a beta");
    }

    #[test]
    fn vim_e_from_word_end_can_land_on_trailing_space() {
        let mut t = ta_with("alpha   ");
        t.set_cursor("alph".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        assert_eq!(t.cursor(), "alpha  ".len());
    }

    #[test]
    fn vim_e_advances_across_atomic_element_word_ends() {
        let mut t = TextArea::new();
        t.insert_str("alpha ");
        t.insert_element("<element>");
        t.insert_str(" gamma");
        let element_start = t.elements[0].range.start;
        t.set_cursor("alph".len());
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), element_start);

        t.input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), "alpha <element> gamm".len());
    }

    #[test]
    fn vim_dollar_lands_on_line_end_character() {
        let mut t = ta_with("abc\n123");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));

        assert_eq!(t.cursor(), 2);

        t.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(t.text(), "ab\n123");
        assert_eq!(t.kill_buffer, "c");
    }

    #[test]
    fn vim_linewise_yank_pastes_below_current_line() {
        let mut t = ta_with("abc\n123\nxyz");
        t.set_cursor(1);
        t.set_vim_enabled(true);

        t.input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        t.input(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));

        assert_eq!(t.text(), "abc\nabc\n123\nxyz");
        assert_eq!(t.cursor(), "abc\n".len());
        assert_eq!(t.kill_buffer, "abc\n");
        assert_eq!(t.kill_buffer_kind, KillBufferKind::Linewise);
    }

    #[test]
    fn delete_backward_word_and_kill_line_variants() {
        let mut t = ta_with("hello   world  ");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "hello   ");
        assert_eq!(t.cursor(), 8);

        let mut t = ta_with("foo bar");
        t.set_cursor(6);
        t.delete_backward_word();
        assert_eq!(t.text(), "foo r");
        assert_eq!(t.cursor(), 4);

        let mut t = ta_with("foo bar");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "foo ");
        assert_eq!(t.cursor(), 4);

        let mut t = ta_with("abc\ndef");
        t.set_cursor(1);
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "a\ndef");
        assert_eq!(t.cursor(), 1);

        let mut t = ta_with("abc\ndef");
        t.set_cursor(3);
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "abcdef");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("abc\ndef");
        t.set_cursor(5);
        t.kill_to_beginning_of_line();
        assert_eq!(t.text(), "abc\nef");

        let mut t = ta_with("abc\ndef");
        t.set_cursor(4);
        t.kill_to_beginning_of_line();
        assert_eq!(t.text(), "abcdef");
        assert_eq!(t.cursor(), 3);
    }

    #[test]
    fn kill_current_line_removes_current_line_linewise() {
        let mut t = ta_with("abc\ndef\nghi");
        t.set_cursor(5);

        t.kill_current_line();

        assert_eq!(t.text(), "abc\nghi");
        assert_eq!(t.cursor(), 4);
        assert_eq!(t.kill_buffer, "def\n");
        assert_eq!(t.kill_buffer_kind, KillBufferKind::Linewise);
    }

    #[test]
    fn kill_current_line_keeps_previous_newline_for_final_line() {
        let mut t = ta_with("abc\ndef");
        t.set_cursor(5);

        t.kill_current_line();

        assert_eq!(t.text(), "abc\n");
        assert_eq!(t.cursor(), 4);
        assert_eq!(t.kill_buffer, "def");
        assert_eq!(t.kill_buffer_kind, KillBufferKind::Linewise);
    }

    #[test]
    fn kill_whole_line_keymap_dispatch_uses_linewise_kill() {
        let mut t = ta_with("abc\ndef\nghi");
        t.set_cursor(5);
        let mut keymap = RuntimeKeymap::defaults().editor;
        keymap.kill_line_start.clear();
        keymap.kill_whole_line = vec![key_hint::ctrl(KeyCode::Char('u'))];

        t.input_with_keymap(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &keymap,
        );

        assert_eq!(t.text(), "abc\nghi");
        assert_eq!(t.cursor(), 4);
        assert_eq!(t.kill_buffer, "def\n");
        assert_eq!(t.kill_buffer_kind, KillBufferKind::Linewise);
    }

    #[test]
    fn delete_forward_word_variants() {
        let mut t = ta_with("hello   world ");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), "   world ");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with("hello   world ");
        t.set_cursor(1);
        t.delete_forward_word();
        assert_eq!(t.text(), "h   world ");
        assert_eq!(t.cursor(), 1);

        let mut t = ta_with("hello   world");
        t.set_cursor(t.text().len());
        t.delete_forward_word();
        assert_eq!(t.text(), "hello   world");
        assert_eq!(t.cursor(), t.text().len());

        let mut t = ta_with("foo   \nbar");
        t.set_cursor(3);
        t.delete_forward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("foo\nbar");
        t.set_cursor(3);
        t.delete_forward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("hello   world ");
        t.set_cursor(t.text().len() + 10);
        t.delete_forward_word();
        assert_eq!(t.text(), "hello   world ");
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn delete_forward_word_handles_atomic_elements() {
        let mut t = TextArea::new();
        t.insert_element("<element>");
        t.insert_str(" tail");

        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), " tail");
        assert_eq!(t.cursor(), 0);

        let mut t = TextArea::new();
        t.insert_str("   ");
        t.insert_element("<element>");
        t.insert_str(" tail");

        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), " tail");
        assert_eq!(t.cursor(), 0);

        let mut t = TextArea::new();
        t.insert_str("prefix ");
        t.insert_element("<element>");
        t.insert_str(" tail");

        let elem_range = t.elements[0].range.clone();
        t.cursor_pos = elem_range.start + (elem_range.len() / 2);
        t.delete_forward_word();
        assert_eq!(t.text(), "prefix  tail");
        assert_eq!(t.cursor(), elem_range.start);
    }

    #[test]
    fn delete_backward_word_respects_word_separators() {
        let mut t = ta_with("path/to/file");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "path/to/");
        assert_eq!(t.cursor(), t.text().len());

        t.delete_backward_word();
        assert_eq!(t.text(), "path/to");
        assert_eq!(t.cursor(), t.text().len());

        let mut t = ta_with("foo/ ");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("foo /");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "foo ");
        assert_eq!(t.cursor(), 4);
    }

    #[test]
    fn delete_forward_word_respects_word_separators() {
        let mut t = ta_with("path/to/file");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), "/to/file");
        assert_eq!(t.cursor(), 0);

        t.delete_forward_word();
        assert_eq!(t.text(), "to/file");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with("/ foo");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), " foo");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with(" /foo");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn yank_restores_last_kill() {
        let mut t = ta_with("hello");
        t.set_cursor(0);
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "");
        assert_eq!(t.cursor(), 0);

        t.yank();
        assert_eq!(t.text(), "hello");
        assert_eq!(t.cursor(), 5);

        let mut t = ta_with("hello world");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor(), 6);

        t.yank();
        assert_eq!(t.text(), "hello world");
        assert_eq!(t.cursor(), 11);

        let mut t = ta_with("hello");
        t.set_cursor(5);
        t.kill_to_beginning_of_line();
        assert_eq!(t.text(), "");
        assert_eq!(t.cursor(), 0);

        t.yank();
        assert_eq!(t.text(), "hello");
        assert_eq!(t.cursor(), 5);
    }

    #[test]
    fn kill_buffer_persists_across_set_text() {
        let mut t = ta_with("restore me");
        t.set_cursor(0);
        t.kill_to_end_of_line();
        assert!(t.text().is_empty());

        t.set_text_clearing_elements("/diff");
        t.set_text_clearing_elements("");
        t.yank();

        assert_eq!(t.text(), "restore me");
        assert_eq!(t.cursor(), "restore me".len());
    }

    #[test]
    fn cursor_left_and_right_handle_graphemes() {
        let mut t = ta_with("a👍b");
        t.set_cursor(t.text().len());

        t.move_cursor_left();
        let after_first_left = t.cursor();
        t.move_cursor_left();
        let after_second_left = t.cursor();
        t.move_cursor_left();
        let after_third_left = t.cursor();

        assert!(after_first_left < t.text().len());
        assert!(after_second_left < after_first_left);
        assert!(after_third_left < after_second_left);

        t.move_cursor_right();
        t.move_cursor_right();
        t.move_cursor_right();
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn control_b_and_f_move_cursor() {
        let mut t = ta_with("abcd");
        t.set_cursor(1);

        t.input(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 2);

        t.input(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn control_b_f_fallback_control_chars_move_cursor() {
        let mut t = ta_with("abcd");
        t.set_cursor(2);

        t.input(KeyEvent::new(KeyCode::Char('\u{0002}'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), 1);

        t.input(KeyEvent::new(KeyCode::Char('\u{0006}'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), 2);
    }

    #[test]
    fn c0_line_feed_inserts_newline_through_insert_newline_keymap() {
        let mut t = ta_with("ab");
        t.set_cursor(1);

        t.input(KeyEvent::new(KeyCode::Char('\u{000a}'), KeyModifiers::NONE));

        assert_eq!(t.text(), "a\nb");
        assert_eq!(t.cursor(), 2);
    }

    #[test]
    fn c0_control_chars_respect_unbound_editor_movement() {
        let mut t = ta_with("a\nb");
        t.set_cursor(2);
        let mut keymap = RuntimeKeymap::defaults().editor;
        keymap.move_up.clear();

        t.input_with_keymap(
            KeyEvent::new(KeyCode::Char('\u{0010}'), KeyModifiers::NONE),
            &keymap,
        );

        assert_eq!(t.cursor(), 2);
    }

    #[test]
    fn c0_control_chars_respect_remapped_editor_movement() {
        let mut t = ta_with("a\nb");
        t.set_cursor(0);
        let mut keymap = RuntimeKeymap::defaults().editor;
        keymap.move_up.clear();
        keymap.move_down = vec![crate::key_hint::ctrl(KeyCode::Char('p'))];

        t.input_with_keymap(
            KeyEvent::new(KeyCode::Char('\u{0010}'), KeyModifiers::NONE),
            &keymap,
        );

        assert_eq!(t.cursor(), 2);
    }

    #[test]
    fn delete_backward_word_alt_keys() {
        let mut t = ta_with("hello world");
        t.set_cursor(t.text().len());
        t.input(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor(), 6);

        let mut t = ta_with("hello world");
        t.set_cursor(t.text().len());
        t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor(), 6);
    }

    #[test]
    fn shift_backspace_and_shift_delete_keep_grapheme_delete_behavior() {
        let mut t = ta_with("abc");
        t.set_cursor(2);

        t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::SHIFT));
        assert_eq!(t.text(), "ac");
        assert_eq!(t.cursor(), 1);

        let mut t = ta_with("abc");
        t.set_cursor(1);

        t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::SHIFT));
        assert_eq!(t.text(), "ac");
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn control_backspace_variants_delete_backward_word() {
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            let mut t = ta_with("hello world");
            t.set_cursor(t.text().len());

            t.input(KeyEvent::new(KeyCode::Backspace, modifiers));
            assert_eq!(t.text(), "hello ");
            assert_eq!(t.cursor(), 6);
        }
    }

    #[test]
    fn control_delete_variants_delete_forward_word() {
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            let mut t = ta_with("hello world");
            t.set_cursor(0);

            t.input(KeyEvent::new(KeyCode::Delete, modifiers));
            assert_eq!(t.text(), " world");
            assert_eq!(t.cursor(), 0);
        }
    }

    #[test]
    fn delete_backward_word_handles_narrow_no_break_space() {
        let mut t = ta_with("32\u{202F}AM");
        t.set_cursor(t.text().len());
        t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        pretty_assertions::assert_eq!(t.text(), "32\u{202F}");
        pretty_assertions::assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn delete_forward_word_with_without_alt_modifier() {
        let mut t = ta_with("hello world");
        t.set_cursor(0);
        t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT));
        assert_eq!(t.text(), " world");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with("hello");
        t.set_cursor(0);
        t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(t.text(), "ello");
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn delete_forward_word_alt_d() {
        let mut t = ta_with("hello world");
        t.set_cursor(6);
        t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        pretty_assertions::assert_eq!(t.text(), "hello ");
        pretty_assertions::assert_eq!(t.cursor(), 6);
    }

    #[test]
    fn control_h_backspace() {
        let mut t = ta_with("12345");
        t.set_cursor(3);
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "1245");
        assert_eq!(t.cursor(), 2);

        t.set_cursor(0);
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "1245");
        assert_eq!(t.cursor(), 0);

        t.set_cursor(t.text().len());
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "124");
        assert_eq!(t.cursor(), 3);
    }

    #[cfg_attr(not(windows), ignore = "AltGr modifier only applies on Windows")]
    #[test]
    fn altgr_ctrl_alt_char_inserts_literal() {
        let mut t = ta_with("");
        t.input(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(t.text(), "c");
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn cursor_vertical_movement_across_lines_and_bounds() {
        let mut t = ta_with("short\nloooooooooong\nmid");
        let second_line_start = 6;
        t.set_cursor(second_line_start + 5);

        t.move_cursor_up();
        assert_eq!(t.cursor(), 5);

        t.move_cursor_up();
        assert_eq!(t.cursor(), 0);

        t.move_cursor_down();
        let pos_after_down = t.cursor();
        assert!(pos_after_down >= second_line_start);

        t.move_cursor_down();
        let third_line_start = t.text().find("mid").unwrap();
        let third_line_end = third_line_start + 3;
        assert!(t.cursor() >= third_line_start && t.cursor() <= third_line_end);

        t.move_cursor_down();
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn home_end_and_emacs_style_home_end() {
        let mut t = ta_with("one\ntwo\nthree");
        let second_line_start = t.text().find("two").unwrap();
        t.set_cursor(second_line_start + 1);

        t.move_cursor_to_beginning_of_line(false);
        assert_eq!(t.cursor(), second_line_start);

        t.move_cursor_to_beginning_of_line(true);
        assert_eq!(t.cursor(), 0);

        t.move_cursor_to_end_of_line(false);
        assert_eq!(t.cursor(), 3);

        t.move_cursor_to_end_of_line(true);
        let end_second_nl = t.text().find("\nthree").unwrap();
        assert_eq!(t.cursor(), end_second_nl);
    }

    #[test]
    fn end_of_line_or_down_at_end_of_text() {
        let mut t = ta_with("one\ntwo");
        t.set_cursor(t.text().len());
        t.move_cursor_to_end_of_line(true);
        assert_eq!(t.cursor(), t.text().len());

        let eol_first_line = 3;
        t.set_cursor(eol_first_line);
        t.move_cursor_to_end_of_line(true);
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn word_navigation_helpers() {
        let t = ta_with("  alpha  beta   gamma");
        let mut t = t;
        let after_alpha = t.text().find("alpha").unwrap() + "alpha".len();
        t.set_cursor(after_alpha);
        assert_eq!(t.beginning_of_previous_word(), 2);

        let beta_start = t.text().find("beta").unwrap();
        t.set_cursor(beta_start);
        assert_eq!(t.end_of_next_word(), beta_start + "beta".len());

        t.set_cursor(t.text().len());
        assert_eq!(t.end_of_next_word(), t.text().len());
    }

    #[test]
    fn word_navigation_cjk_each_char_is_boundary() {
        let text = "你好世界";
        let mut t = ta_with(text);

        t.set_cursor(text.len());
        assert_eq!(t.beginning_of_previous_word(), 9);

        t.set_cursor(9);
        assert_eq!(t.beginning_of_previous_word(), 6);

        t.set_cursor(6);
        assert_eq!(t.beginning_of_previous_word(), 3);

        t.set_cursor(3);
        assert_eq!(t.beginning_of_previous_word(), 0);
    }

    #[test]
    fn word_navigation_cjk_forward() {
        let text = "你好世界";
        let mut t = ta_with(text);

        t.set_cursor(0);
        assert_eq!(t.end_of_next_word(), 3);

        t.set_cursor(3);
        assert_eq!(t.end_of_next_word(), 6);

        t.set_cursor(6);
        assert_eq!(t.end_of_next_word(), 9);

        t.set_cursor(9);
        assert_eq!(t.end_of_next_word(), 12);
    }

    #[test]
    fn word_navigation_mixed_ascii_cjk() {
        let text = "hello你好";
        let mut t = ta_with(text);

        t.set_cursor(0);
        assert_eq!(t.end_of_next_word(), 5);

        t.set_cursor(5);
        assert_eq!(t.end_of_next_word(), 8);

        t.set_cursor(text.len());
        assert_eq!(t.beginning_of_previous_word(), 8);

        t.set_cursor(8);
        assert_eq!(t.beginning_of_previous_word(), 5);

        t.set_cursor(5);
        assert_eq!(t.beginning_of_previous_word(), 0);
    }

    #[test]
    fn word_navigation_preserves_separator_breaks_within_unicode_segments() {
        let mut t = ta_with("can't 32.3 foo.bar");

        t.set_cursor(5);
        assert_eq!(t.beginning_of_previous_word(), 4);

        t.set_cursor(4);
        assert_eq!(t.beginning_of_previous_word(), 3);

        t.set_cursor(10);
        assert_eq!(t.beginning_of_previous_word(), 9);

        t.set_cursor(18);
        assert_eq!(t.beginning_of_previous_word(), 15);
    }

    #[test]
    fn wrapping_and_cursor_positions() {
        let mut t = ta_with("hello world here");
        let area = Rect::new(0, 0, 6, 10);
        assert!(t.desired_height(area.width) >= 3);

        let world_start = t.text().find("world").unwrap();
        t.set_cursor(world_start + 3);
        let (_x, y) = t.cursor_pos(area).unwrap();
        assert_eq!(y, 1);

        let mut state = TextAreaState::default();
        let small_area = Rect::new(0, 0, 6, 1);
        let (_x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
        assert_eq!(y, 0);

        let mut buf = Buffer::empty(small_area);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), small_area, &mut buf, &mut state);
        let effective_lines = t.desired_height(small_area.width);
        assert!(state.scroll < effective_lines);
    }

    #[test]
    fn render_highlights_apply_style_without_mutating_text() {
        let t = ta_with("hello world");
        let area = Rect::new(0, 0, 20, 1);
        let mut state = TextAreaState::default();
        let mut buf = Buffer::empty(area);
        let highlight_style = Style::default().add_modifier(ratatui::style::Modifier::REVERSED);

        t.render_ref_styled_with_highlights(
            area,
            &mut buf,
            &mut state,
            Style::default(),
            &[(6..11, highlight_style)],
        );

        assert_eq!(t.text(), "hello world");
        assert!(
            !buf[(0, 0)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            buf[(6, 0)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            buf[(10, 0)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }

    #[test]
    fn cursor_pos_with_state_basic_and_scroll_behaviors() {
        let mut t = ta_with("hello world");
        t.set_cursor(3);
        let area = Rect::new(2, 5, 20, 3);
        let bad_state = TextAreaState { scroll: 999 };
        let (x1, y1) = t.cursor_pos(area).unwrap();
        let (x2, y2) = t.cursor_pos_with_state(area, bad_state).unwrap();
        assert_eq!((x2, y2), (x1, y1));

        let mut t = ta_with("one two three four five six");
        let wrap_width = 4;
        let _ = t.desired_height(wrap_width);
        t.set_cursor(t.text().len().saturating_sub(2));
        let small_area = Rect::new(0, 0, wrap_width, 2);
        let state = TextAreaState { scroll: 0 };
        let (_x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
        assert_eq!(y, small_area.y + small_area.height - 1);

        let mut t = ta_with("alpha beta gamma delta epsilon zeta");
        let wrap_width = 5;
        let lines = t.desired_height(wrap_width);
        t.set_cursor(1);
        let area = Rect::new(0, 0, wrap_width, 3);
        let state = TextAreaState {
            scroll: lines.saturating_mul(2),
        };
        let (_x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!(y, area.y);
    }

    #[test]
    fn wrapped_navigation_across_visual_lines() {
        let mut t = ta_with("abcdefghij");
        let _ = t.desired_height(4);

        t.set_cursor(0);
        t.move_cursor_down();
        assert_eq!(t.cursor(), 4);

        t.set_cursor(4);
        let area = Rect::new(0, 0, 4, 10);
        let (x, y) = t.cursor_pos(area).unwrap();
        assert_eq!((x, y), (0, 1));

        let small_area = Rect::new(0, 0, 4, 1);
        let state = TextAreaState::default();
        let (x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
        assert_eq!((x, y), (0, 0));

        t.set_cursor(6);
        t.move_cursor_up();
        assert_eq!(t.cursor(), 2);

        t.move_cursor_down();
        assert_eq!(t.cursor(), 6);

        t.move_cursor_down();
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn cursor_pos_with_state_after_movements() {
        let mut t = ta_with("abcdefghij");
        let _ = t.desired_height(4);
        let area = Rect::new(0, 0, 4, 2);
        let mut state = TextAreaState::default();
        let mut buf = Buffer::empty(area);

        t.set_cursor(0);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 0));

        t.move_cursor_down();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 1));

        t.move_cursor_down();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 1));

        t.move_cursor_up();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 0));

        t.set_cursor(2);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x0, y0) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x0, y0), (2, 0));
        t.move_cursor_down();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x1, y1) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x1, y1), (2, 1));
    }

    #[test]
    fn wrapped_navigation_with_newlines_and_spaces() {
        let mut t = ta_with("word1  word2\nword3");
        let _ = t.desired_height(6);

        let start_word2 = t.text().find("word2").unwrap();
        t.set_cursor(start_word2 + 1);

        t.move_cursor_up();
        assert_eq!(t.cursor(), 1);

        t.move_cursor_down();
        assert_eq!(t.cursor(), start_word2 + 1);

        t.move_cursor_down();
        let start_word3 = t.text().find("word3").unwrap();
        assert!(t.cursor() >= start_word3 && t.cursor() <= start_word3 + "word3".len());
    }

    #[test]
    fn wrapped_navigation_with_wide_graphemes() {
        let mut t = ta_with("👍👍👍👍");
        let _ = t.desired_height(3);

        t.set_cursor("👍👍".len());

        t.move_cursor_down();
        let pos_after_down = t.cursor();
        assert!(pos_after_down >= "👍👍".len());

        t.move_cursor_up();
        assert_eq!(t.cursor(), "👍👍".len());
    }

    #[test]
    fn fuzz_textarea_randomized() {
        let pst_today_seed: u64 = (chrono::Utc::now() - chrono::Duration::hours(8))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as u64;
        let mut rng = rand::rngs::StdRng::seed_from_u64(pst_today_seed);

        for _case in 0..500 {
            let mut ta = TextArea::new();
            let mut state = TextAreaState::default();
            let mut elem_texts: Vec<String> = Vec::new();
            let mut next_elem_id: usize = 0;
            let base_len = rng.random_range(0..30);
            let mut base = String::new();
            for _ in 0..base_len {
                base.push_str(&rand_grapheme(&mut rng));
            }
            ta.set_text_clearing_elements(&base);
            let mut boundaries: Vec<usize> = vec![0];
            boundaries.extend(ta.text().char_indices().map(|(i, _)| i).skip(1));
            boundaries.push(ta.text().len());
            let init = boundaries[rng.random_range(0..boundaries.len())];
            ta.set_cursor(init);

            let mut width: u16 = rng.random_range(1..=12);
            let mut height: u16 = rng.random_range(1..=4);

            for _step in 0..60 {
                if rng.random_bool(0.1) {
                    width = rng.random_range(1..=12);
                }
                if rng.random_bool(0.1) {
                    height = rng.random_range(1..=4);
                }

                match rng.random_range(0..18) {
                    0 => {
                        let len = rng.random_range(0..6);
                        let mut s = String::new();
                        for _ in 0..len {
                            s.push_str(&rand_grapheme(&mut rng));
                        }
                        ta.insert_str(&s);
                    }
                    1 => {
                        let mut b: Vec<usize> = vec![0];
                        b.extend(ta.text().char_indices().map(|(i, _)| i).skip(1));
                        b.push(ta.text().len());
                        let i1 = rng.random_range(0..b.len());
                        let i2 = rng.random_range(0..b.len());
                        let (start, end) = if b[i1] <= b[i2] {
                            (b[i1], b[i2])
                        } else {
                            (b[i2], b[i1])
                        };
                        let insert_len = rng.random_range(0..=4);
                        let mut s = String::new();
                        for _ in 0..insert_len {
                            s.push_str(&rand_grapheme(&mut rng));
                        }
                        let before = ta.text().len();
                        let intersects_element = elem_texts.iter().any(|payload| {
                            if let Some(pstart) = ta.text().find(payload) {
                                let pend = pstart + payload.len();
                                pstart < end && pend > start
                            } else {
                                false
                            }
                        });
                        ta.replace_range(start..end, &s);
                        if !intersects_element {
                            let after = ta.text().len();
                            assert_eq!(
                                after as isize,
                                before as isize + (s.len() as isize) - ((end - start) as isize)
                            );
                        }
                    }
                    2 => ta.delete_backward(rng.random_range(0..=3)),
                    3 => ta.delete_forward(rng.random_range(0..=3)),
                    4 => ta.delete_backward_word(),
                    5 => ta.kill_to_beginning_of_line(),
                    6 => ta.kill_to_end_of_line(),
                    7 => ta.move_cursor_left(),
                    8 => ta.move_cursor_right(),
                    9 => ta.move_cursor_up(),
                    10 => ta.move_cursor_down(),
                    11 => ta.move_cursor_to_beginning_of_line(true),
                    12 => ta.move_cursor_to_end_of_line(true),
                    13 => {
                        let payload =
                            format!("[[EL#{}:{}]]", next_elem_id, rng.random_range(1000..9999));
                        next_elem_id += 1;
                        ta.insert_element(&payload);
                        elem_texts.push(payload);
                    }
                    14 => {
                        if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                            && let Some(start) = ta.text().find(&payload)
                        {
                            let end = start + payload.len();
                            if end - start > 2 {
                                let pos = rng.random_range(start + 1..end - 1);
                                let ins = rand_grapheme(&mut rng);
                                ta.insert_str_at(pos, &ins);
                            }
                        }
                    }
                    15 => {
                        if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                            && let Some(start) = ta.text().find(&payload)
                        {
                            let end = start + payload.len();
                            let mut s = start.saturating_sub(rng.random_range(0..=2));
                            let mut e = (end + rng.random_range(0..=2)).min(ta.text().len());
                            let txt = ta.text();
                            while s > 0 && !txt.is_char_boundary(s) {
                                s -= 1;
                            }
                            while e < txt.len() && !txt.is_char_boundary(e) {
                                e += 1;
                            }
                            if s < e {
                                let mut srep = String::new();
                                for _ in 0..rng.random_range(0..=2) {
                                    srep.push_str(&rand_grapheme(&mut rng));
                                }
                                ta.replace_range(s..e, &srep);
                            }
                        }
                    }
                    16 => {
                        if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                            && let Some(start) = ta.text().find(&payload)
                        {
                            let end = start + payload.len();
                            if end - start > 2 {
                                let pos = rng.random_range(start + 1..end - 1);
                                ta.set_cursor(pos);
                            }
                        }
                    }
                    _ => {
                        if rng.random_bool(0.5) {
                            let p = ta.beginning_of_previous_word();
                            ta.set_cursor(p);
                        } else {
                            let p = ta.end_of_next_word();
                            ta.set_cursor(p);
                        }
                    }
                }

                assert!(ta.cursor() <= ta.text().len());

                for payload in &elem_texts {
                    if let Some(start) = ta.text().find(payload) {
                        let end = start + payload.len();
                        assert_eq!(&ta.text()[start..end], payload);
                        let c = ta.cursor();
                        assert!(
                            c <= start || c >= end,
                            "cursor inside element: {start}..{end} at {c}"
                        );
                    }
                }

                let area = Rect::new(0, 0, width, height);
                let total_lines = ta.desired_height(width);
                let full_area = Rect::new(0, 0, width, total_lines.max(1));
                let mut buf = Buffer::empty(full_area);
                ratatui::widgets::WidgetRef::render_ref(&(&ta), full_area, &mut buf);

                let _ = ta.cursor_pos(area);

                let (_x, _y) = ta
                    .cursor_pos_with_state(area, state)
                    .unwrap_or((area.x, area.y));

                let mut sbuf = Buffer::empty(area);
                ratatui::widgets::StatefulWidgetRef::render_ref(
                    &(&ta),
                    area,
                    &mut sbuf,
                    &mut state,
                );

                let total_lines = total_lines as usize;
                if (height as usize) >= total_lines {
                    assert_eq!(state.scroll, 0);
                }
            }
        }
    }
}
