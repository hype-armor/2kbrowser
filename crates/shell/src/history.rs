//! Where the browser has been, and where it can go back to.
//!
//! Separated from the window because it is the part that can be tested: the
//! event loop needs a display, and this needs nothing. Back and forward are
//! easy to get subtly wrong — the classic bug is a forward entry surviving a
//! new navigation, so "forward" takes you somewhere you never chose — and that
//! bug is invisible until someone hits it.

/// A back/forward stack.
///
/// Always non-empty: a browser is always *somewhere*, even if that somewhere is
/// a blank page.
#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<String>,
    /// Index of the current entry. Always in range.
    index: usize,
}

impl History {
    /// Starts at `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            entries: vec![url.into()],
            index: 0,
        }
    }

    /// Where the browser is now.
    pub fn current(&self) -> &str {
        &self.entries[self.index]
    }

    /// Follows a link.
    ///
    /// Anything ahead of the current entry is discarded, which is what every
    /// browser does and what people expect: having gone back and then somewhere
    /// new, "forward" must not still lead to the branch you abandoned.
    ///
    /// Navigating to where you already are does not add an entry, so a page
    /// that links to itself does not fill the stack.
    pub fn visit(&mut self, url: impl Into<String>) {
        let url = url.into();
        if url == self.current() {
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(url);
        self.index = self.entries.len() - 1;
    }

    /// Steps back, returning where to go.
    pub fn back(&mut self) -> Option<&str> {
        self.index = self.index.checked_sub(1)?;
        Some(&self.entries[self.index])
    }

    /// Steps forward, returning where to go.
    pub fn forward(&mut self) -> Option<&str> {
        if self.index + 1 >= self.entries.len() {
            return None;
        }
        self.index += 1;
        Some(&self.entries[self.index])
    }

    /// Whether there is anywhere to go back to.
    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    /// Whether there is anywhere to go forward to.
    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    /// How many entries there are, for tests and for a future history view.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Never true; a browser is always somewhere.
    pub fn is_empty(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_history_is_at_its_first_page_and_can_go_nowhere() {
        let history = History::new("a");
        assert_eq!(history.current(), "a");
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());
    }

    #[test]
    fn visiting_moves_forward_and_back_returns() {
        let mut history = History::new("a");
        history.visit("b");
        history.visit("c");
        assert_eq!(history.current(), "c");
        assert_eq!(history.back(), Some("b"));
        assert_eq!(history.back(), Some("a"));
        assert_eq!(history.back(), None, "nothing before the first page");
        assert_eq!(history.current(), "a", "a refused step does not move");
    }

    #[test]
    fn forward_retraces_exactly_what_back_undid() {
        let mut history = History::new("a");
        history.visit("b");
        history.back();
        assert_eq!(history.forward(), Some("b"));
        assert_eq!(history.forward(), None);
        assert_eq!(history.current(), "b");
    }

    #[test]
    fn a_new_visit_discards_the_branch_that_was_abandoned() {
        // The classic bug: having gone back and then somewhere new, "forward"
        // still leads to a page you deliberately left. Invisible until someone
        // hits it, and then baffling.
        let mut history = History::new("a");
        history.visit("b");
        history.visit("c");
        history.back();
        assert_eq!(history.current(), "b");

        history.visit("d");
        assert_eq!(history.current(), "d");
        assert!(!history.can_go_forward(), "c must be gone");
        assert_eq!(history.len(), 3, "a, b, d");
        assert_eq!(history.back(), Some("b"));
    }

    #[test]
    fn revisiting_the_current_page_adds_nothing() {
        // A page linking to itself, or a click on the link you just followed.
        let mut history = History::new("a");
        history.visit("b");
        history.visit("b");
        assert_eq!(history.len(), 2);
        assert_eq!(history.back(), Some("a"));
    }

    #[test]
    fn going_back_and_forward_repeatedly_stays_consistent() {
        let mut history = History::new("a");
        for url in ["b", "c", "d"] {
            history.visit(url);
        }
        for _ in 0..5 {
            history.back();
        }
        assert_eq!(history.current(), "a");
        for _ in 0..5 {
            history.forward();
        }
        assert_eq!(history.current(), "d");
        assert_eq!(history.len(), 4, "wandering does not create entries");
    }
}
