//! A list of tabs with one of them active.
//!
//! Generic over what a tab holds, so the interesting part — which tab becomes
//! active when the active one closes, and the rule that there is always at
//! least one — is testable without a window, a page, or a display.
//!
//! That rule is the whole reason this is a type rather than a `Vec` and an
//! index: every operation has to leave the index pointing at something, and
//! "closing the last tab" is where that quietly stops being true.

/// Tabs, one of which is always active.
#[derive(Debug, Clone)]
pub struct Tabs<T> {
    items: Vec<T>,
    active: usize,
}

impl<T> Tabs<T> {
    /// One tab, active.
    pub fn new(first: T) -> Self {
        Self {
            items: vec![first],
            active: 0,
        }
    }

    /// How many tabs there are. Never zero.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Never true; there is always a tab.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Index of the active tab.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The active tab.
    pub fn active(&self) -> &T {
        &self.items[self.active]
    }

    /// The active tab, mutably.
    pub fn active_mut(&mut self) -> &mut T {
        &mut self.items[self.active]
    }

    /// Every tab, in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Opens a tab after the active one and makes it active.
    ///
    /// After rather than at the end: a tab opened from a page belongs beside
    /// it, and having it appear at the far end of a long strip is how people
    /// lose track of what they just opened.
    pub fn open(&mut self, tab: T) {
        let at = self.active + 1;
        self.items.insert(at, tab);
        self.active = at;
    }

    /// Selects a tab by index. Out-of-range indices are ignored.
    pub fn select(&mut self, index: usize) {
        if index < self.items.len() {
            self.active = index;
        }
    }

    /// Moves to the next tab, wrapping.
    pub fn next(&mut self) {
        self.active = (self.active + 1) % self.items.len();
    }

    /// Moves to the previous tab, wrapping.
    pub fn previous(&mut self) {
        self.active = (self.active + self.items.len() - 1) % self.items.len();
    }

    /// Closes a tab, returning it.
    ///
    /// `None` when there is only one left: a browser with no tabs has nothing
    /// to show, and the caller wanting to close the window should say so rather
    /// than arriving at it through an empty list.
    pub fn close(&mut self, index: usize) -> Option<T> {
        if self.items.len() <= 1 || index >= self.items.len() {
            return None;
        }
        let closed = self.items.remove(index);
        // Closing a tab to the left of the active one shifts it; closing the
        // active one falls to its right-hand neighbour, or to the new last tab
        // when there is none. That is what every browser does, and the reason
        // is that the neighbour is what the eye was already next to.
        if index < self.active || self.active >= self.items.len() {
            self.active = self.active.saturating_sub(1);
        }
        Some(closed)
    }

    /// Closes the active tab.
    pub fn close_active(&mut self) -> Option<T> {
        self.close(self.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(labels: &[&str]) -> Tabs<String> {
        let mut tabs = Tabs::new(labels[0].to_owned());
        for label in &labels[1..] {
            tabs.open((*label).to_owned());
        }
        tabs
    }

    fn labels(tabs: &Tabs<String>) -> Vec<&str> {
        tabs.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_new_list_has_one_active_tab() {
        let tabs = Tabs::new("a".to_owned());
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.active(), "a");
        assert_eq!(tabs.active_index(), 0);
    }

    #[test]
    fn a_tab_opens_beside_the_one_it_came_from() {
        // Not at the far end: a tab opened from a page belongs next to it, and
        // appearing at the end of a long strip is how people lose track of
        // what they just opened.
        let mut tabs = tabs(&["a", "b", "c"]);
        tabs.select(0);
        tabs.open("new".to_owned());
        assert_eq!(labels(&tabs), vec!["a", "new", "b", "c"]);
        assert_eq!(tabs.active(), "new");
    }

    #[test]
    fn closing_the_active_tab_falls_to_its_right_hand_neighbour() {
        let mut tabs = tabs(&["a", "b", "c"]);
        tabs.select(1);
        assert_eq!(tabs.close_active().as_deref(), Some("b"));
        assert_eq!(labels(&tabs), vec!["a", "c"]);
        assert_eq!(tabs.active(), "c");
    }

    #[test]
    fn closing_the_last_tab_in_the_strip_falls_left() {
        // There is no right-hand neighbour, and the index must still point at
        // something.
        let mut tabs = tabs(&["a", "b", "c"]);
        tabs.select(2);
        tabs.close_active();
        assert_eq!(labels(&tabs), vec!["a", "b"]);
        assert_eq!(tabs.active(), "b");
    }

    #[test]
    fn closing_a_tab_to_the_left_keeps_the_active_one_active() {
        // The active tab moved index, but not in any sense the reader cares
        // about — what they are looking at must not change.
        let mut tabs = tabs(&["a", "b", "c"]);
        tabs.select(2);
        tabs.close(0);
        assert_eq!(labels(&tabs), vec!["b", "c"]);
        assert_eq!(tabs.active(), "c");
    }

    #[test]
    fn closing_a_tab_to_the_right_keeps_the_active_one_active() {
        let mut tabs = tabs(&["a", "b", "c"]);
        tabs.select(0);
        tabs.close(2);
        assert_eq!(labels(&tabs), vec!["a", "b"]);
        assert_eq!(tabs.active(), "a");
    }

    #[test]
    fn the_last_tab_cannot_be_closed() {
        // A browser with no tabs has nothing to show. Wanting to close the
        // window is a different request and the caller should say so.
        let mut tabs = Tabs::new("only".to_owned());
        assert!(tabs.close_active().is_none());
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.active(), "only");
    }

    #[test]
    fn stepping_wraps_in_both_directions() {
        let mut tabs = tabs(&["a", "b", "c"]);
        tabs.select(2);
        tabs.next();
        assert_eq!(tabs.active(), "a");
        tabs.previous();
        assert_eq!(tabs.active(), "c");
    }

    #[test]
    fn selecting_out_of_range_changes_nothing() {
        let mut tabs = tabs(&["a", "b"]);
        tabs.select(0);
        tabs.select(99);
        assert_eq!(tabs.active(), "a");
    }

    #[test]
    fn closing_out_of_range_changes_nothing() {
        let mut tabs = tabs(&["a", "b"]);
        assert!(tabs.close(99).is_none());
        assert_eq!(tabs.len(), 2);
    }

    #[test]
    fn opening_and_closing_repeatedly_always_leaves_a_valid_active_tab() {
        // The invariant this type exists for. Any sequence of operations has
        // to leave the index pointing at something.
        let mut tabs = Tabs::new(0usize);
        for round in 1..30 {
            tabs.open(round);
            if round % 3 == 0 {
                tabs.close_active();
            }
            if round % 4 == 0 {
                tabs.close(0);
            }
            tabs.next();
            assert!(tabs.active_index() < tabs.len(), "round {round}");
        }
        // And `active()` never panics, which is the practical form of it.
        let _ = tabs.active();
    }
}
