//! A single-line editable text field.
//!
//! Separated from the chrome and the window for the same reason as
//! [`crate::history`]: this is the part that can be tested, and text editing is
//! full of small behaviours that are individually obvious and collectively
//! easy to get wrong — typing over a selection, a cursor that lands inside a
//! multi-byte character, word motion that stops in the wrong place.
//!
//! Positions are byte indices into the text and are always on character
//! boundaries. Rust would panic on a slice that split a character, so the
//! failure mode for getting this wrong is a crash rather than mojibake — which
//! is the right way round, but only if the invariant actually holds.

/// A line of editable text with a cursor and a selection.
#[derive(Debug, Clone, Default)]
pub struct Field {
    text: String,
    /// Byte index of the cursor.
    cursor: usize,
    /// The other end of the selection. Equal to the cursor when nothing is
    /// selected, which is why there is no separate "has selection" flag to get
    /// out of step.
    anchor: usize,
}

impl Field {
    /// A field containing `text`, with everything selected.
    ///
    /// Selected because that is what focusing a URL bar does: the common next
    /// action is to replace the URL, not to edit one character of it.
    pub fn with_all_selected(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            anchor: 0,
            text,
        }
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte index of the cursor.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected range, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.cursor != self.anchor)
            .then(|| (self.cursor.min(self.anchor), self.cursor.max(self.anchor)))
    }

    /// Replaces the selection, or inserts at the cursor when there is none.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.anchor = self.cursor;
    }

    /// Deletes the selection, or the character before the cursor.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let Some(previous) = self.previous_boundary(self.cursor) else {
            return;
        };
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
        self.anchor = self.cursor;
    }

    /// Deletes the selection, or the character after the cursor.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let Some(next) = self.next_boundary(self.cursor) else {
            return;
        };
        self.text.replace_range(self.cursor..next, "");
        self.anchor = self.cursor;
    }

    /// Moves the cursor one character left.
    pub fn left(&mut self, extend: bool) {
        // Without a modifier, a left arrow over a selection goes to its start
        // rather than moving from the cursor — collapsing to the near edge is
        // what every text field does and what hands expect.
        if !extend && let Some((start, _)) = self.selection() {
            self.place(start, false);
            return;
        }
        let to = self.previous_boundary(self.cursor).unwrap_or(self.cursor);
        self.place(to, extend);
    }

    /// Moves the cursor one character right.
    pub fn right(&mut self, extend: bool) {
        if !extend && let Some((_, end)) = self.selection() {
            self.place(end, false);
            return;
        }
        let to = self.next_boundary(self.cursor).unwrap_or(self.cursor);
        self.place(to, extend);
    }

    /// Moves to the start of the previous word.
    pub fn word_left(&mut self, extend: bool) {
        let mut at = self.cursor;
        // Skip whatever separators sit immediately behind, then the word.
        while let Some(previous) = self.previous_boundary(at) {
            if !is_separator(self.char_at(previous)) {
                break;
            }
            at = previous;
        }
        while let Some(previous) = self.previous_boundary(at) {
            if is_separator(self.char_at(previous)) {
                break;
            }
            at = previous;
        }
        self.place(at, extend);
    }

    /// Moves to the end of the next word.
    pub fn word_right(&mut self, extend: bool) {
        let mut at = self.cursor;
        while at < self.text.len() && is_separator(self.char_at(at)) {
            at = self.next_boundary(at).unwrap_or(self.text.len());
        }
        while at < self.text.len() && !is_separator(self.char_at(at)) {
            at = self.next_boundary(at).unwrap_or(self.text.len());
        }
        self.place(at, extend);
    }

    /// Moves to the start of the line.
    pub fn home(&mut self, extend: bool) {
        self.place(0, extend);
    }

    /// Moves to the end of the line.
    pub fn end(&mut self, extend: bool) {
        self.place(self.text.len(), extend);
    }

    /// Selects everything.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Places the cursor, extending the selection or collapsing it.
    fn place(&mut self, to: usize, extend: bool) {
        self.cursor = to;
        if !extend {
            self.anchor = to;
        }
    }

    /// Removes the selection. Returns whether there was one.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.anchor = start;
        true
    }

    /// The character starting at `at`, which must be a boundary.
    fn char_at(&self, at: usize) -> char {
        self.text[at..].chars().next().unwrap_or(' ')
    }

    /// The character boundary before `at`, or `None` at the start.
    fn previous_boundary(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    /// The character boundary after `at`, or `None` at the end.
    fn next_boundary(&self, at: usize) -> Option<usize> {
        self.text[at..]
            .chars()
            .next()
            .map(|c| at + c.len_utf8())
            .filter(|next| *next <= self.text.len())
    }
}

/// What word motion stops at.
///
/// URLs, not prose: `/`, `.`, `?`, and `&` are the joints a person actually
/// wants to jump between, and treating only whitespace as a separator would
/// make word motion useless in the one field this is for.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | '.' | ':' | '?' | '&' | '=' | '#' | '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the field as `text` with `|` at the cursor and `[]` around any
    /// selection, so a test reads as what you would see.
    fn show(field: &Field) -> String {
        let mut out = String::new();
        let (start, end) = field.selection().unwrap_or((field.cursor, field.cursor));
        for (index, c) in field.text.char_indices() {
            if index == start && start != end {
                out.push('[');
            }
            if index == field.cursor && start == end {
                out.push('|');
            }
            if index == end && start != end {
                out.push(']');
            }
            out.push(c);
        }
        if field.text.len() == start && start != end {
            out.push('[');
        }
        if field.text.len() == field.cursor && start == end {
            out.push('|');
        }
        if field.text.len() == end && start != end {
            out.push(']');
        }
        out
    }

    #[test]
    fn focusing_selects_everything() {
        // The common next action is to replace the URL, not to edit one
        // character of it.
        let field = Field::with_all_selected("example.com");
        assert_eq!(show(&field), "[example.com]");
        assert_eq!(field.selection(), Some((0, 11)));
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut field = Field::with_all_selected("example.com");
        field.insert("x");
        assert_eq!(show(&field), "x|");
    }

    #[test]
    fn typing_without_a_selection_inserts_at_the_cursor() {
        let mut field = Field::with_all_selected("ab");
        field.left(false);
        field.insert("Z");
        assert_eq!(show(&field), "Z|ab");
    }

    #[test]
    fn backspace_removes_the_selection_or_one_character() {
        let mut field = Field::with_all_selected("abc");
        field.backspace();
        assert_eq!(show(&field), "|");

        let mut field = Field::with_all_selected("abc");
        field.end(false);
        field.backspace();
        assert_eq!(show(&field), "ab|");
        field.backspace();
        assert_eq!(show(&field), "a|");
    }

    #[test]
    fn delete_removes_forwards() {
        let mut field = Field::with_all_selected("abc");
        field.home(false);
        field.delete();
        assert_eq!(show(&field), "|bc");
        // At the end there is nothing to delete, and nothing should happen.
        field.end(false);
        field.delete();
        assert_eq!(show(&field), "bc|");
    }

    #[test]
    fn an_arrow_collapses_a_selection_to_the_side_it_points() {
        // What every text field does, and what hands expect: the arrow leaves
        // the selection rather than moving one character from the cursor.
        let mut field = Field::with_all_selected("abcd");
        field.left(false);
        assert_eq!(show(&field), "|abcd");

        let mut field = Field::with_all_selected("abcd");
        field.right(false);
        assert_eq!(show(&field), "abcd|");
    }

    #[test]
    fn shift_extends_the_selection() {
        let mut field = Field::with_all_selected("abcd");
        field.home(false);
        field.right(true);
        field.right(true);
        assert_eq!(show(&field), "[ab]cd");
        field.left(true);
        assert_eq!(show(&field), "[a]bcd");
    }

    #[test]
    fn the_cursor_never_lands_inside_a_character() {
        // Rust panics on a slice that splits a character, so getting this
        // wrong crashes rather than corrupting — but only if it holds.
        let mut field = Field::with_all_selected("héllo — wörld");
        field.end(false);
        for _ in 0..40 {
            field.left(false);
        }
        assert_eq!(field.cursor(), 0);
        for _ in 0..40 {
            field.right(false);
        }
        assert_eq!(field.cursor(), field.text().len());

        // And deleting through it byte by byte leaves valid text throughout.
        field.end(false);
        while !field.text().is_empty() {
            field.backspace();
        }
        assert_eq!(field.text(), "");
    }

    #[test]
    fn word_motion_stops_at_a_urls_joints() {
        // Whitespace alone would make word motion useless in the one field
        // this is for: a URL has no spaces in it.
        let mut field = Field::with_all_selected("https://example.com/a/b.html");
        field.end(false);
        field.word_left(false);
        assert_eq!(show(&field), "https://example.com/a/b.|html");
        field.word_left(false);
        assert_eq!(show(&field), "https://example.com/a/|b.html");
        field.word_left(false);
        assert_eq!(show(&field), "https://example.com/|a/b.html");
        field.word_left(false);
        assert_eq!(show(&field), "https://example.|com/a/b.html");
    }

    #[test]
    fn word_motion_reaches_both_ends_without_sticking() {
        let mut field = Field::with_all_selected("https://example.com/a");
        field.end(false);
        for _ in 0..20 {
            field.word_left(false);
        }
        assert_eq!(field.cursor(), 0);
        for _ in 0..20 {
            field.word_right(false);
        }
        assert_eq!(field.cursor(), field.text().len());
    }

    #[test]
    fn editing_an_empty_field_does_nothing_untoward() {
        let mut field = Field::default();
        field.backspace();
        field.delete();
        field.left(false);
        field.right(false);
        field.word_left(false);
        field.word_right(false);
        assert_eq!(field.text(), "");
        assert_eq!(field.cursor(), 0);
        assert_eq!(field.selection(), None);
    }

    #[test]
    fn select_all_then_type_replaces_everything() {
        let mut field = Field::with_all_selected("old");
        field.end(false);
        field.select_all();
        field.insert("new");
        assert_eq!(show(&field), "new|");
    }
}
