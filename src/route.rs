//! Navigation history with a real forward stack.
//!
//! The previous shell had a back button that popped a stack and a forward button
//! that always navigated to Home — pressing it after one Back was a lie. This is
//! the usual three-stack browser model, kept generic so it can be tested against
//! a plain enum instead of the generated Slint one.

/// Back/forward history. `T` is the destination type; it only needs to be
/// comparable and copyable, which both the generated Slint enum and a test enum
/// are.
pub struct History<T> {
    back: Vec<T>,
    forward: Vec<T>,
    limit: usize,
}

impl<T: PartialEq + Copy> History<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Record a navigation from `current` to `next`.
    ///
    /// Navigating to where you already are is a no-op: re-clicking the active
    /// nav item must not push a duplicate entry the user has to press Back twice
    /// to escape. Any new navigation invalidates the forward stack, as in every
    /// browser.
    pub fn visit(&mut self, current: T, next: T) {
        if current == next {
            return;
        }
        self.back.push(current);
        if self.back.len() > self.limit {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Pop one step back and offer it, given the page being left.
    pub fn back(&mut self, current: T) -> Option<T> {
        let target = self.back.pop()?;
        self.forward.push(current);
        if self.forward.len() > self.limit {
            self.forward.remove(0);
        }
        Some(target)
    }

    /// Pop one step forward and offer it, given the page being left.
    pub fn forward(&mut self, current: T) -> Option<T> {
        let target = self.forward.pop()?;
        self.back.push(current);
        if self.back.len() > self.limit {
            self.back.remove(0);
        }
        Some(target)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    #[cfg(test)]
    pub fn back_depth(&self) -> usize {
        self.back.len()
    }
}

#[cfg(test)]
mod tests {
    use super::History;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Page {
        Home,
        Discovery,
        Search,
        Detail,
    }

    #[test]
    fn forward_returns_to_where_back_came_from() {
        let mut history = History::new(16);
        history.visit(Page::Home, Page::Discovery);
        history.visit(Page::Discovery, Page::Search);

        assert_eq!(history.back(Page::Search), Some(Page::Discovery));
        assert_eq!(history.back(Page::Discovery), Some(Page::Home));
        assert_eq!(history.forward(Page::Home), Some(Page::Discovery));
        assert_eq!(history.forward(Page::Discovery), Some(Page::Search));
        assert_eq!(history.forward(Page::Search), None);
    }

    #[test]
    fn a_new_navigation_clears_forward() {
        let mut history = History::new(16);
        history.visit(Page::Home, Page::Discovery);
        history.back(Page::Discovery);
        assert!(history.can_go_forward());
        history.visit(Page::Home, Page::Search);
        assert!(!history.can_go_forward());
    }

    #[test]
    fn revisiting_the_current_page_does_not_grow_the_stack() {
        let mut history = History::new(16);
        history.visit(Page::Home, Page::Home);
        assert_eq!(history.back_depth(), 0);
        assert!(!history.can_go_back());
    }

    #[test]
    fn the_stack_is_bounded() {
        let mut history = History::new(3);
        for page in [
            Page::Discovery,
            Page::Search,
            Page::Detail,
            Page::Home,
            Page::Discovery,
            Page::Search,
        ] {
            let current = *history.back.last().unwrap_or(&Page::Home);
            history.visit(current, page);
        }
        assert_eq!(history.back_depth(), 3);
    }

    #[test]
    fn an_empty_history_has_nothing_to_offer() {
        let mut history: History<Page> = History::new(8);
        assert_eq!(history.back(Page::Home), None);
        assert_eq!(history.forward(Page::Home), None);
    }
}
