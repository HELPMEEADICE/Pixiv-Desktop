//! Navigation history with a real forward stack, over parameterised routes.
//!
//! The previous shell had a back button that popped a stack and a forward button
//! that always navigated to Home -- pressing it after one Back was a lie. This is
//! the usual three-stack browser model, kept generic so it can be tested against
//! a plain enum instead of the generated Slint one.
//!
//! The second half of the story is Route: a bare page name cannot say *which*
//! artist a profile page is showing, so two visits to two different profiles were
//! the same history entry and Back restored whichever id the app happened to hold
//! last. A route carries the page and its argument together.

use crate::Destination;

/// The argument that makes one visit to a page distinct from another.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RouteParam {
    #[default]
    None,
    /// A work, for the detail page.
    Illust(u64),
    /// A user, for the profile page.
    User(u64),
}

impl RouteParam {
    #[must_use]
    pub const fn illust(id: u64) -> Self {
        Self::Illust(id)
    }

    #[must_use]
    pub const fn user(id: u64) -> Self {
        Self::User(id)
    }
}

/// One entry in the navigation history: a page plus the thing it shows.
// Slint's generated Destination enum is PartialEq but not Eq, so the route can
// only promise PartialEq. That is all the history needs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Route {
    pub page: Destination,
    pub param: RouteParam,
}

impl Route {
    #[must_use]
    pub const fn new(page: Destination, param: RouteParam) -> Self {
        Self { page, param }
    }

    /// A page with no argument.
    #[must_use]
    pub const fn plain(page: Destination) -> Self {
        Self::new(page, RouteParam::None)
    }

    #[must_use]
    pub const fn illust(page: Destination, id: u64) -> Self {
        Self::new(page, RouteParam::illust(id))
    }

    #[must_use]
    pub const fn user(id: u64) -> Self {
        Self::new(Destination::UserProfile, RouteParam::user(id))
    }

    /// The illustration this entry shows, if any.
    #[must_use]
    pub const fn illust_id(self) -> Option<u64> {
        match self.param {
            RouteParam::Illust(id) => Some(id),
            _ => None,
        }
    }

    /// The user this entry shows, if any.
    #[must_use]
    pub const fn user_id(self) -> Option<u64> {
        match self.param {
            RouteParam::User(id) => Some(id),
            _ => None,
        }
    }

    /// True when the two entries would render the same thing, which is what
    /// makes a repeat navigation a no-op instead of a new history step.
    #[must_use]
    pub fn same_view(self, other: Self) -> bool {
        self.page == other.page && self.param == other.param
    }
}

/// Back/forward history.
///
/// The destination type only needs to be comparable and copyable, which both
/// Route and the plain test enum are.
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

    /// Record a navigation from current to next.
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

/// The history the app actually uses.
pub type RouteHistory = History<Route>;

#[cfg(test)]
mod tests {
    use super::{History, Route, RouteHistory};
    use crate::Destination;

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

    #[test]
    fn two_user_profiles_are_different_history_entries() {
        let mut history: RouteHistory = History::new(16);
        let home = Route::plain(Destination::Home);
        let first = Route::user(11);
        let second = Route::user(22);

        history.visit(home, first);
        history.visit(first, second);
        // Back must land on the FIRST author, not on whichever id the app last
        // wrote into a single shared field.
        assert_eq!(history.back(second), Some(first));
        assert_eq!(history.back(first), Some(home));
        assert_eq!(history.forward(home), Some(first));
        assert_eq!(history.forward(first), Some(second));
    }

    #[test]
    fn reopening_the_same_profile_is_not_a_new_step() {
        let mut history: RouteHistory = History::new(16);
        let profile = Route::user(7);
        history.visit(profile, profile);
        assert!(!history.can_go_back());
    }

    #[test]
    fn illust_ids_survive_a_round_trip() {
        let mut history: RouteHistory = History::new(16);
        let work_a = Route::illust(Destination::Detail, 100);
        let work_b = Route::illust(Destination::Detail, 200);
        history.visit(work_a, work_b);
        let back = history.back(work_b).expect("one step back");
        assert_eq!(back.illust_id(), Some(100));
        assert_eq!(back.page, Destination::Detail);
        assert_eq!(back.user_id(), None);
    }

    #[test]
    fn route_accessors_report_what_the_page_shows() {
        assert_eq!(Route::user(5).user_id(), Some(5));
        assert_eq!(Route::user(5).illust_id(), None);
        assert_eq!(Route::illust(Destination::Detail, 9).illust_id(), Some(9));
        assert_eq!(Route::plain(Destination::Home).param, Default::default());
        assert!(Route::plain(Destination::Home).same_view(Route::plain(Destination::Home)));
        assert!(!Route::user(1).same_view(Route::user(2)));
        assert_eq!(
            Route::new(Destination::Search, Default::default()).page,
            Destination::Search
        );
    }
}
