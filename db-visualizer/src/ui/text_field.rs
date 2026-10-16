//! A text field drawn by hand: the single-line inputs of the connection dialog
//! and the multi-line NQL editor share one implementation.
//!
//! GPUI ships no editor widget. This one keeps selection, undo-free editing,
//! IME composition and keyboard navigation close to what a native text view
//! does, while painting an NQL highlighter and a line-number gutter when asked.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollHandle, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, fill, font,
    point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::nql::{TokenKind, highlight_line};
use crate::theme::{FONT_MONO, FONT_UI, Theme, theme};

actions!(
    text_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteToLineStart,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Newline,
        Submit,
        Tab,
        Cancel,
        ShowCharacterPalette,
    ]
);

/// The key context every text field declares.
pub const KEY_CONTEXT: &str = "TextField";

/// Bindings for the actions above, scoped to focused text fields.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    let c = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("home", LineStart, c),
        KeyBinding::new("cmd-left", LineStart, c),
        KeyBinding::new("ctrl-a", LineStart, c),
        KeyBinding::new("end", LineEnd, c),
        KeyBinding::new("cmd-right", LineEnd, c),
        KeyBinding::new("ctrl-e", LineEnd, c),
        KeyBinding::new("shift-home", SelectLineStart, c),
        KeyBinding::new("cmd-shift-left", SelectLineStart, c),
        KeyBinding::new("shift-end", SelectLineEnd, c),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, c),
        KeyBinding::new("cmd-up", DocStart, c),
        KeyBinding::new("cmd-down", DocEnd, c),
        KeyBinding::new("cmd-shift-up", SelectDocStart, c),
        KeyBinding::new("cmd-shift-down", SelectDocEnd, c),
        KeyBinding::new("alt-left", WordLeft, c),
        KeyBinding::new("alt-right", WordRight, c),
        KeyBinding::new("alt-shift-left", SelectWordLeft, c),
        KeyBinding::new("alt-shift-right", SelectWordRight, c),
        KeyBinding::new("alt-backspace", DeleteWordLeft, c),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, c),
        KeyBinding::new("cmd-a", SelectAll, c),
        KeyBinding::new("cmd-c", Copy, c),
        KeyBinding::new("cmd-x", Cut, c),
        KeyBinding::new("cmd-v", Paste, c),
        KeyBinding::new("enter", Newline, c),
        KeyBinding::new("shift-enter", Newline, c),
        KeyBinding::new("cmd-enter", Submit, c),
        KeyBinding::new("ctrl-enter", Submit, c),
        KeyBinding::new("tab", Tab, c),
        KeyBinding::new("escape", Cancel, c),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, c),
    ]
}

/// What a field reports to its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextFieldEvent {
    /// The text changed through typing, pasting or [`TextField::set_text`].
    Changed,
    /// Enter in a single-line field, or Cmd+Enter anywhere.
    Submit,
    /// Escape.
    Cancel,
}

/// Construction options.
#[derive(Clone, Debug)]
pub struct TextFieldOptions {
    pub multiline: bool,
    pub placeholder: SharedString,
    pub mono: bool,
    pub highlight: bool,
    pub gutter: bool,
    pub masked: bool,
    pub font_size: Pixels,
    pub line_height: Pixels,
    pub padding_x: Pixels,
    pub padding_y: Pixels,
}

impl TextFieldOptions {
    /// A one-line form input.
    #[must_use]
    pub fn input(placeholder: impl Into<SharedString>) -> Self {
        Self {
            multiline: false,
            placeholder: placeholder.into(),
            mono: false,
            highlight: false,
            gutter: false,
            masked: false,
            font_size: px(12.5),
            line_height: px(20.),
            padding_x: px(10.),
            padding_y: px(8.),
        }
    }

    /// The NQL editor.
    #[must_use]
    pub fn editor() -> Self {
        Self {
            multiline: true,
            placeholder: "Type an NQL query…".into(),
            mono: true,
            highlight: true,
            gutter: true,
            masked: false,
            font_size: px(12.5),
            line_height: px(22.),
            padding_x: px(15.),
            padding_y: px(20.),
        }
    }

    /// Hide the characters (API tokens).
    #[must_use]
    pub fn masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    /// Monospace text.
    #[must_use]
    pub fn mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self
    }
}

/// Geometry of the last paint, needed to map the mouse and IME to text offsets.
struct FieldLayout {
    lines: Vec<ShapedLine>,
    line_starts: Vec<usize>,
    text_origin: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
}

/// The editable text state.
pub struct TextField {
    focus_handle: FocusHandle,
    text: String,
    pub options: TextFieldOptions,
    pub disabled: bool,
    /// Highlighted line in the gutter, 1-based.
    pub error_line: Option<usize>,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    selecting: bool,
    goal_x: Option<Pixels>,
    scroll_x: Pixels,
    layout: Option<FieldLayout>,
    scroll: ScrollHandle,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TextField {
    /// A field with the given options and initial text.
    pub fn new(options: TextFieldOptions, text: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            focus_handle: cx.focus_handle(),
            text,
            options,
            disabled: false,
            error_line: None,
            selection: end..end,
            reversed: false,
            marked: None,
            selecting: false,
            goal_x: None,
            scroll_x: px(0.),
            layout: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// The current text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `true` when the field has no text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of lines (at least one).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.text.split('\n').count().max(1)
    }

    /// Replaces the whole text and moves the caret to the end.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        let end = self.text.len();
        self.selection = end..end;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        self.scroll_x = px(0.);
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    /// Focuses the field.
    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    /// `true` while focused.
    #[must_use]
    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    // ---- offsets ------------------------------------------------------------

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn line_starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        starts
    }

    /// `(row, line_start)` of a byte offset.
    fn row_of(&self, offset: usize) -> (usize, usize) {
        let starts = self.line_starts();
        let row = starts.iter().rposition(|&s| s <= offset).unwrap_or(0);
        (row, starts[row])
    }

    fn end_of_line(&self, row: usize) -> usize {
        let starts = self.line_starts();
        if row + 1 < starts.len() {
            starts[row + 1] - 1
        } else {
            self.text.len()
        }
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.text.len())
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn word_left(&self, offset: usize) -> usize {
        let mut chars: Vec<(usize, char)> = self.text[..offset].char_indices().collect();
        // Skip whitespace, then a run of word characters (or one symbol).
        while let Some(&(_, c)) = chars.last() {
            if c.is_whitespace() && c != '\n' {
                chars.pop();
            } else {
                break;
            }
        }
        let Some(&(_, first)) = chars.last() else {
            return 0;
        };
        if first == '\n' {
            return chars.last().map_or(0, |(i, _)| *i);
        }
        if !Self::is_word_char(first) {
            return chars.last().map_or(0, |(i, _)| *i);
        }
        while let Some(&(i, c)) = chars.last() {
            if Self::is_word_char(c) {
                chars.pop();
                if chars.is_empty() {
                    return 0;
                }
                if !chars.last().is_some_and(|(_, c)| Self::is_word_char(*c)) {
                    return i;
                }
            } else {
                return i + c.len_utf8();
            }
        }
        0
    }

    fn word_right(&self, offset: usize) -> usize {
        let rest = &self.text[offset..];
        let mut iter = rest.char_indices().peekable();
        // Skip whitespace (not newlines).
        while let Some(&(_, c)) = iter.peek() {
            if c.is_whitespace() && c != '\n' {
                iter.next();
            } else {
                break;
            }
        }
        let Some(&(start, first)) = iter.peek() else {
            return self.text.len();
        };
        if first == '\n' || !Self::is_word_char(first) {
            return offset + start + first.len_utf8();
        }
        let mut end = start;
        for (i, c) in iter {
            if Self::is_word_char(c) {
                end = i + c.len_utf8();
            } else {
                break;
            }
        }
        offset + end
    }

    fn word_range_at(&self, offset: usize) -> Range<usize> {
        let mut start = offset;
        while start > 0 {
            let prev = self.previous_boundary(start);
            if self.text[prev..start].chars().all(Self::is_word_char) {
                start = prev;
            } else {
                break;
            }
        }
        let mut end = offset;
        while end < self.text.len() {
            let next = self.next_boundary(end);
            if self.text[end..next].chars().all(Self::is_word_char) {
                end = next;
            } else {
                break;
            }
        }
        start..end
    }

    // ---- editing ------------------------------------------------------------

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.text.len());
        self.selection = offset..offset;
        self.reversed = false;
        self.goal_x = None;
        self.ensure_visible();
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.text.len());
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        self.goal_x = None;
        self.ensure_visible();
        cx.notify();
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let start = range.start.min(self.text.len());
        let end = range.end.min(self.text.len()).max(start);
        let mut next = String::with_capacity(self.text.len() + text.len());
        next.push_str(&self.text[..start]);
        next.push_str(text);
        next.push_str(&self.text[end..]);
        self.text = next;
        let caret = start + text.len();
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.goal_x = None;
        self.ensure_visible();
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let text: String = if self.options.multiline {
            text.to_owned()
        } else {
            text.replace(['\n', '\r'], " ")
        };
        self.replace_range(self.selection.clone(), &text, cx);
    }

    /// Keeps the caret inside the scroll viewport (multi-line fields).
    fn ensure_visible(&mut self) {
        if !self.options.multiline {
            return;
        }
        let viewport = self.scroll.bounds().size.height;
        if viewport <= px(0.) {
            return;
        }
        let (row, _) = self.row_of(self.cursor());
        let lh = self.options.line_height;
        let pad = self.options.padding_y;
        #[allow(clippy::cast_precision_loss)]
        let top = pad + lh * (row as f32);
        let bottom = top + lh;
        let offset = -self.scroll.offset().y;
        if top - pad < offset {
            self.scroll.set_offset(point(px(0.), -(top - pad)));
        } else if bottom + pad > offset + viewport {
            self.scroll
                .set_offset(point(px(0.), -(bottom + pad - viewport)));
        }
    }

    fn vertical(&self, offset: usize, delta: i32) -> usize {
        let (row, line_start) = self.row_of(offset);
        let Some(layout) = &self.layout else {
            return offset;
        };
        let target = row as i64 + i64::from(delta);
        if target < 0 {
            return 0;
        }
        let target = usize::try_from(target).unwrap_or(0);
        if target >= layout.lines.len() {
            return self.text.len();
        }
        let x = self.goal_x.unwrap_or_else(|| {
            layout
                .lines
                .get(row)
                .map_or(px(0.), |line| line.x_for_index(offset - line_start))
        });
        let line = &layout.lines[target];
        let column = line.closest_index_for_x(x).min(line.len());
        layout.line_starts[target] + column
    }

    // ---- actions ------------------------------------------------------------

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selection.is_empty() {
            let prev = self.previous_boundary(self.cursor());
            self.replace_range(prev..self.cursor(), "", cx);
        } else {
            self.replace_range(self.selection.clone(), "", cx);
        }
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selection.is_empty() {
            let next = self.next_boundary(self.cursor());
            self.replace_range(self.cursor()..next, "", cx);
        } else {
            self.replace_range(self.selection.clone(), "", cx);
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selection.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            self.move_to(self.next_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selection.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if !self.options.multiline {
            self.move_to(0, cx);
            return;
        }
        let goal = self.current_x();
        let target = self.vertical(self.cursor(), -1);
        self.move_to(target, cx);
        self.goal_x = goal;
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if !self.options.multiline {
            self.move_to(self.text.len(), cx);
            return;
        }
        let goal = self.current_x();
        let target = self.vertical(self.cursor(), 1);
        self.move_to(target, cx);
        self.goal_x = goal;
    }

    fn current_x(&self) -> Option<Pixels> {
        if let Some(x) = self.goal_x {
            return Some(x);
        }
        let (row, line_start) = self.row_of(self.cursor());
        self.layout
            .as_ref()
            .and_then(|l| l.lines.get(row))
            .map(|line| line.x_for_index(self.cursor() - line_start))
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let goal = self.current_x();
        let target = self.vertical(self.cursor(), -1);
        self.select_to(target, cx);
        self.goal_x = goal;
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let goal = self.current_x();
        let target = self.vertical(self.cursor(), 1);
        self.select_to(target, cx);
        self.goal_x = goal;
    }

    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        let (_, start) = self.row_of(self.cursor());
        self.move_to(start, cx);
    }

    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        let (row, _) = self.row_of(self.cursor());
        self.move_to(self.end_of_line(row), cx);
    }

    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        let (_, start) = self.row_of(self.cursor());
        self.select_to(start, cx);
    }

    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        let (row, _) = self.row_of(self.cursor());
        self.select_to(self.end_of_line(row), cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.text.len(), cx);
    }

    fn word_left_action(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_left(self.cursor()), cx);
    }

    fn word_right_action(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_right(self.cursor()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_left(self.cursor()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_right(self.cursor()), cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if self.selection.is_empty() {
            let start = self.word_left(self.cursor());
            self.replace_range(start..self.cursor(), "", cx);
        } else {
            self.replace_range(self.selection.clone(), "", cx);
        }
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let (_, start) = self.row_of(self.cursor());
        if self.selection.is_empty() {
            self.replace_range(start..self.cursor(), "", cx);
        } else {
            self.replace_range(self.selection.clone(), "", cx);
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = 0..self.text.len();
        self.reversed = false;
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_owned(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text[self.selection.clone()].to_owned(),
            ));
            if !self.disabled {
                self.replace_range(self.selection.clone(), "", cx);
            }
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert(&text, cx);
        }
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        if self.options.multiline {
            // Keep the indentation of the current line, the way editors do.
            let (_, start) = self.row_of(self.cursor());
            let indent: String = self.text[start..self.cursor()]
                .chars()
                .take_while(|c| *c == ' ')
                .collect();
            self.insert(&format!("\n{indent}"), cx);
        } else {
            cx.emit(TextFieldEvent::Submit);
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Submit);
    }

    fn tab(&mut self, _: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.options.multiline {
            self.insert("  ", cx);
        } else {
            window.focus_next();
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Cancel);
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    // ---- mouse --------------------------------------------------------------

    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = &self.layout else {
            return self.text.len();
        };
        if layout.lines.is_empty() {
            return 0;
        }
        let local_y = position.y - layout.text_origin.y;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = if local_y < px(0.) {
            0
        } else {
            ((f32::from(local_y) / f32::from(layout.line_height)).floor() as usize)
                .min(layout.lines.len() - 1)
        };
        let line = &layout.lines[row];
        let column = line
            .closest_index_for_x(position.x - layout.text_origin.x)
            .min(line.len());
        (layout.line_starts[row] + self.display_to_text(column)).min(self.text.len())
    }

    /// Maps a column of the painted (possibly masked) line to a text column.
    fn display_to_text(&self, column: usize) -> usize {
        if !self.options.masked {
            return column;
        }
        let chars = column / MASK_CHAR.len_utf8();
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.selecting = true;
        let index = self.index_for_point(event.position);
        match event.click_count {
            2 => {
                let range = self.word_range_at(index);
                self.selection = range;
                self.reversed = false;
                cx.notify();
            }
            n if n >= 3 => {
                let (row, start) = self.row_of(index);
                self.selection = start..self.end_of_line(row);
                self.reversed = false;
                cx.notify();
            }
            _ => {
                if event.modifiers.shift {
                    self.select_to(index, cx);
                } else {
                    self.move_to(index, cx);
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            let index = self.index_for_point(event.position);
            self.select_to(index, cx);
        }
    }

    // ---- UTF-16 (IME) -------------------------------------------------------

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.text.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.text.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selection),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let text: String = if self.options.multiline {
            new_text.to_owned()
        } else {
            new_text.replace(['\n', '\r'], " ")
        };
        self.replace_range(range, &text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        let start = range.start.min(self.text.len());
        let end = range.end.min(self.text.len()).max(start);
        self.text = format!("{}{}{}", &self.text[..start], new_text, &self.text[end..]);
        self.marked = (!new_text.is_empty()).then(|| start..start + new_text.len());
        self.selection = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + start..r.end + start)
            .unwrap_or_else(|| start + new_text.len()..start + new_text.len());
        self.reversed = false;
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let (row, start) = self.row_of(range.start);
        let line = layout.lines.get(row)?;
        #[allow(clippy::cast_precision_loss)]
        let y = layout.text_origin.y + layout.line_height * (row as f32);
        let x0 = layout.text_origin.x + line.x_for_index(range.start - start);
        let x1 = layout.text_origin.x
            + line.x_for_index((range.end.max(range.start) - start).min(line.len()));
        Some(Bounds::from_corners(
            point(x0, y),
            point(x1, y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_point(point);
        Some(self.offset_to_utf16(index))
    }
}

impl Render for TextField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let element = TextFieldElement { field: cx.entity() };
        let multiline = self.options.multiline;
        let root = gpui::div()
            .id("text-field")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .w_full()
            .when(multiline, |d| d.h_full())
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left_action))
            .on_action(cx.listener(Self::word_right_action))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move));
        if multiline {
            root.child(
                gpui::div()
                    .id("text-field-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(element),
            )
        } else {
            root.overflow_hidden().child(element)
        }
    }
}

const MASK_CHAR: char = '•';
const GUTTER_WIDTH: f32 = 56.;

struct TextFieldElement {
    field: Entity<TextField>,
}

struct Prepaint {
    lines: Vec<ShapedLine>,
    line_starts: Vec<usize>,
    gutter: Vec<ShapedLine>,
    text_origin: Point<Pixels>,
    gutter_width: Pixels,
    selection: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    line_height: Pixels,
    gutter_bg: Hsla,
    scroll_x: Pixels,
}

impl IntoElement for TextFieldElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn token_color(kind: TokenKind, t: &Theme) -> Hsla {
    match kind {
        TokenKind::Keyword => t.syntax.keyword,
        TokenKind::Identifier | TokenKind::Whitespace => t.ink,
        TokenKind::TypeName => t.syntax.label,
        TokenKind::String => t.syntax.literal,
        TokenKind::Number => t.syntax.number,
        TokenKind::Param => t.syntax.param,
        TokenKind::Comment => t.syntax.comment,
        TokenKind::Operator => t.syntax.operator,
        TokenKind::Punctuation => t.syntax.punctuation,
    }
}

/// Splits `runs` so the part inside `marked` carries an underline.
fn underline_marked(runs: Vec<TextRun>, marked: Range<usize>, color: Hsla) -> Vec<TextRun> {
    let mut out = Vec::with_capacity(runs.len() + 2);
    let mut offset = 0;
    for run in runs {
        let run_range = offset..offset + run.len;
        offset += run.len;
        let a = run_range.start.max(marked.start);
        let b = run_range.end.min(marked.end);
        if a >= b {
            out.push(run);
            continue;
        }
        if a > run_range.start {
            out.push(TextRun {
                len: a - run_range.start,
                ..run.clone()
            });
        }
        out.push(TextRun {
            len: b - a,
            underline: Some(UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: false,
            }),
            ..run.clone()
        });
        if run_range.end > b {
            out.push(TextRun {
                len: run_range.end - b,
                ..run
            });
        }
    }
    out
}

impl Element for TextFieldElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let field = self.field.read(cx);
        let options = &field.options;
        #[allow(clippy::cast_precision_loss)]
        let content = options.line_height * (field.line_count() as f32) + options.padding_y * 2.;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = content.into();
        if options.multiline {
            style.min_size.height = relative(1.).into();
        }
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let t = *theme(cx);
        let field = self.field.read(cx);
        let options = field.options.clone();
        let text = field.text.clone();
        let selection = field.selection.clone();
        let cursor = field.cursor();
        let marked = field.marked.clone();
        let error_line = field.error_line;
        let mut scroll_x = field.scroll_x;
        let focused = field.focus_handle.is_focused(window);
        let disabled = field.disabled;

        let family = if options.mono { FONT_MONO } else { FONT_UI };
        let base_font = font(family);
        let mono_font = font(FONT_MONO);
        let font_size = options.font_size;
        let line_height = options.line_height;
        let gutter_width = if options.gutter {
            px(GUTTER_WIDTH)
        } else {
            px(0.)
        };
        let text_color = if disabled { t.ink_muted } else { t.ink };

        let mut lines = Vec::new();
        let mut gutter = Vec::new();
        let mut line_starts = Vec::new();
        let mut offset = 0;
        let empty = text.is_empty();
        let source_lines: Vec<&str> = if empty {
            vec![""]
        } else {
            text.split('\n').collect()
        };
        for (row, raw) in source_lines.iter().enumerate() {
            line_starts.push(offset);
            let display: SharedString = if empty {
                options.placeholder.clone()
            } else if options.masked {
                MASK_CHAR.to_string().repeat(raw.chars().count()).into()
            } else {
                SharedString::from((*raw).to_owned())
            };
            let mut runs: Vec<TextRun> = if empty {
                vec![TextRun {
                    len: display.len(),
                    font: base_font.clone(),
                    color: t.ink_muted,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }]
            } else if options.highlight && !options.masked {
                highlight_line(raw)
                    .into_iter()
                    .map(|(range, kind)| TextRun {
                        len: range.len(),
                        font: base_font.clone(),
                        color: if disabled {
                            t.ink_muted
                        } else {
                            token_color(kind, &t)
                        },
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    })
                    .collect()
            } else {
                vec![TextRun {
                    len: display.len(),
                    font: base_font.clone(),
                    color: text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }]
            };
            if let Some(marked) = &marked
                && !options.masked
                && !empty
            {
                let line_range = offset..offset + raw.len();
                let a = marked.start.max(line_range.start);
                let b = marked.end.min(line_range.end);
                if a < b {
                    runs = underline_marked(runs, a - offset..b - offset, text_color);
                }
            }
            if runs.is_empty() {
                runs.push(TextRun {
                    len: 0,
                    font: base_font.clone(),
                    color: text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            lines.push(
                window
                    .text_system()
                    .shape_line(display, font_size, &runs, None),
            );
            if options.gutter {
                let number: SharedString = (row + 1).to_string().into();
                let color = if error_line == Some(row + 1) {
                    t.danger
                } else {
                    t.ink_muted
                };
                let run = TextRun {
                    len: number.len(),
                    font: mono_font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                gutter.push(
                    window
                        .text_system()
                        .shape_line(number, px(12.), &[run], None),
                );
            }
            offset += raw.len() + 1;
        }

        // Single-line fields scroll horizontally to keep the caret visible.
        let (cursor_row, cursor_line_start) = {
            let row = line_starts.iter().rposition(|&s| s <= cursor).unwrap_or(0);
            (row, line_starts[row])
        };
        let display_col = |row: usize, col: usize| -> usize {
            if options.masked {
                let chars = source_lines[row][..col.min(source_lines[row].len())]
                    .chars()
                    .count();
                chars * MASK_CHAR.len_utf8()
            } else {
                col
            }
        };
        let cursor_x_in_line =
            lines[cursor_row].x_for_index(display_col(cursor_row, cursor - cursor_line_start));
        let text_width = bounds.size.width - gutter_width - options.padding_x * 2.;
        if !options.multiline {
            if cursor_x_in_line - scroll_x > text_width {
                scroll_x = cursor_x_in_line - text_width;
            }
            if cursor_x_in_line < scroll_x {
                scroll_x = cursor_x_in_line;
            }
            if scroll_x < px(0.) {
                scroll_x = px(0.);
            }
        } else {
            scroll_x = px(0.);
        }

        let text_origin = point(
            bounds.left() + gutter_width + options.padding_x - scroll_x,
            bounds.top() + options.padding_y,
        );

        // Selection quads, one per covered line.
        let mut selection_quads = Vec::new();
        if !selection.is_empty() && !empty {
            let sel_color = t.mix(t.accent, crate::theme::transparent(), 0.28);
            for (row, &start) in line_starts.iter().enumerate() {
                let end = start + source_lines[row].len();
                let a = selection.start.max(start);
                let b = selection.end.min(end);
                let spans_newline = selection.end > end && selection.start <= end;
                if a > b || (a == b && !spans_newline) {
                    continue;
                }
                let line = &lines[row];
                let x0 = line.x_for_index(display_col(row, a.saturating_sub(start)));
                let mut x1 = line.x_for_index(display_col(row, b.saturating_sub(start)));
                if selection.end > end {
                    x1 += px(6.);
                }
                #[allow(clippy::cast_precision_loss)]
                let y = text_origin.y + line_height * (row as f32);
                selection_quads.push(fill(
                    Bounds::from_corners(
                        point(text_origin.x + x0, y),
                        point(text_origin.x + x1, y + line_height),
                    ),
                    sel_color,
                ));
            }
        }

        let cursor_quad = if focused && !disabled {
            #[allow(clippy::cast_precision_loss)]
            let y = text_origin.y + line_height * (cursor_row as f32);
            Some(fill(
                Bounds::new(
                    point(text_origin.x + cursor_x_in_line, y + px(2.)),
                    size(px(1.5), line_height - px(4.)),
                ),
                t.accent,
            ))
        } else {
            None
        };

        Prepaint {
            lines,
            line_starts,
            gutter,
            text_origin,
            gutter_width,
            selection: selection_quads,
            cursor: cursor_quad,
            line_height,
            gutter_bg: t.surface1,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.field.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );

        if prepaint.gutter_width > px(0.) {
            window.paint_quad(fill(
                Bounds::new(
                    bounds.origin,
                    size(prepaint.gutter_width, bounds.size.height),
                ),
                prepaint.gutter_bg,
            ));
            for (row, number) in prepaint.gutter.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let y = prepaint.text_origin.y + prepaint.line_height * (row as f32);
                let x = bounds.left() + prepaint.gutter_width - px(13.) - number.width;
                number
                    .paint(point(x, y), prepaint.line_height, window, cx)
                    .ok();
            }
        }
        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }
        for (row, line) in prepaint.lines.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let y = prepaint.text_origin.y + prepaint.line_height * (row as f32);
            line.paint(
                point(prepaint.text_origin.x, y),
                prepaint.line_height,
                window,
                cx,
            )
            .ok();
        }
        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        let lines = std::mem::take(&mut prepaint.lines);
        let line_starts = std::mem::take(&mut prepaint.line_starts);
        let text_origin = prepaint.text_origin;
        let line_height = prepaint.line_height;
        let scroll_x = prepaint.scroll_x;
        self.field.update(cx, |field, _| {
            field.scroll_x = scroll_x;
            field.layout = Some(FieldLayout {
                lines,
                line_starts,
                text_origin,
                bounds,
                line_height,
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underline_splits_runs_around_the_marked_range() {
        let run = TextRun {
            len: 10,
            font: font("Menlo"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let out = underline_marked(vec![run], 3..6, gpui::black());
        let lens: Vec<usize> = out.iter().map(|r| r.len).collect();
        assert_eq!(lens, vec![3, 3, 4]);
        assert!(out[1].underline.is_some());
        assert!(out[0].underline.is_none());
    }
}
