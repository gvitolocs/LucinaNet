//! The tabs as plain data: the engine keeps its web views in a [`Tabs`], the window's shortcuts are in `keymap`.
//! Nothing here needs an engine, so the rules are tested on their own.

/// At most this many tabs: each holds a whole page's memory on a 4 GB laptop without swap.
pub const MAX_TABS: usize = 8;

/// The browser's tabs in strip order, each with an id that is never reused, and the one shown.
pub struct Tabs<T> {
    list: Vec<(u32, T)>,
    active: u32,
    next: u32,
}

impl<T> Tabs<T> {
    /// No tabs; ids start at 1. `active()` is None until a tab is opened.
    pub fn new() -> Tabs<T> {
        Tabs {
            list: Vec::new(),
            active: 0,
            next: 1,
        }
    }

    /// Adds `item` right after the active tab (at the end if there is none) and returns its new id.
    /// When `activate`, it becomes the active tab. When there are already MAX_TABS tabs, nothing changes
    /// and the item is handed back: `Err(item)`.
    pub fn open(&mut self, item: T, activate: bool) -> Result<u32, T> {
        if self.list.len() >= MAX_TABS {
            return Err(item);
        }

        let id = self.next;
        self.next += 1;

        if let Some(active_index) = self.list.iter().position(|&(tab_id, _)| tab_id == self.active) {
            self.list.insert(active_index + 1, (id, item));
        } else {
            self.list.push((id, item));
        }

        if activate {
            self.active = id;
        }

        Ok(id)
    }

    /// Removes tab `id` and returns it (None if unknown). If it was the active one, the tab that took its
    /// place in the strip (the one to its right) becomes active, else the one to its left.
    /// If that leaves no tabs, `fresh()` is opened and made active, so there is always a tab after a close.
    /// `fresh` is only called in that case.
    pub fn close(&mut self, id: u32, fresh: impl FnOnce() -> T) -> Option<T> {
        if let Some(index) = self.list.iter().position(|&(tab_id, _)| tab_id == id) {
            let (_, item) = self.list.remove(index);

            if self.active == id {
                if index < self.list.len() {
                    self.active = self.list[index].0;
                } else if !self.list.is_empty() {
                    self.active = self.list[index - 1].0;
                } else {
                    let new_item = fresh();
                    let new_id = self.next;
                    self.next += 1;
                    self.list.push((new_id, new_item));
                    self.active = new_id;
                }
            }

            Some(item)
        } else {
            None
        }
    }

    /// Makes tab `id` active; false (and no change) if there is no such tab.
    pub fn select(&mut self, id: u32) -> bool {
        if self.list.iter().any(|&(tab_id, _)| tab_id == id) {
            self.active = id;
            true
        } else {
            false
        }
    }

    /// The id of the tab `step` places from the active one, wrapping around both ends (step may be negative,
    /// e.g. -1 for the previous tab). None if there are no tabs.
    pub fn neighbour(&self, step: i32) -> Option<u32> {
        if self.list.is_empty() {
            return None;
        }

        let active_index = self.list.iter().position(|&(tab_id, _)| tab_id == self.active)?;
        let len = self.list.len() as i32;
        let new_index = (active_index as i32 + step).rem_euclid(len) as usize;

        Some(self.list[new_index].0)
    }

    /// The active tab's id, if any tab is open.
    pub fn active_id(&self) -> Option<u32> {
        if self.list.is_empty() {
            None
        } else {
            Some(self.active)
        }
    }

    pub fn active(&self) -> Option<&T> {
        self.list.iter().find(|&&(tab_id, _)| tab_id == self.active).map(|&(_, ref item)| item)
    }

    pub fn get(&self, id: u32) -> Option<&T> {
        self.list.iter().find(|&&(tab_id, _)| tab_id == id).map(|&(_, ref item)| item)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut T> {
        self.list.iter_mut().find(|&&mut (tab_id, _)| tab_id == id).map(|&mut (_, ref mut item)| item)
    }

    /// The id of the first tab whose item matches.
    pub fn find(&self, matches: impl Fn(&T) -> bool) -> Option<u32> {
        self.list.iter().find(|&&(_, ref item)| matches(item)).map(|&(tab_id, _)| tab_id)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.list.len() >= MAX_TABS
    }

    /// Ids and items in strip order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.list.iter().map(|&(tab_id, ref item)| (tab_id, item))
    }
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Linux key codes (input-event-codes.h) the window's tests press. The shortcut table itself is
/// `lucinanet-shortcuts`; `keymap` turns a press into a command.
#[cfg(test)]
pub const KEY_T: u16 = 20;
#[cfg(test)]
pub const KEY_W: u16 = 17;
#[cfg(test)]
pub const KEY_TAB: u16 = 15;

#[cfg(test)]
mod tests {
    use super::*;

    fn order(tabs: &Tabs<&str>) -> Vec<u32> {
        tabs.iter().map(|(id, _)| id).collect()
    }

    #[test]
    fn tabs_open_after_the_shown_one() {
        let mut tabs = Tabs::new();
        assert!(tabs.active().is_none());
        assert_eq!(tabs.open("a", true), Ok(1));
        assert_eq!(tabs.open("b", false), Ok(2));
        assert_eq!(tabs.open("c", false), Ok(3));
        // Not activated: the first stays shown, and each opens next to it.
        assert_eq!(tabs.active_id(), Some(1));
        assert_eq!(order(&tabs), [1, 3, 2]);
        assert_eq!(tabs.open("d", true), Ok(4));
        assert_eq!(order(&tabs), [1, 4, 3, 2]);
        assert_eq!(tabs.active(), Some(&"d"));
        assert_eq!(tabs.find(|t| *t == "c"), Some(3));
    }

    #[test]
    fn a_ninth_tab_is_refused() {
        let mut tabs = Tabs::new();
        for i in 0..MAX_TABS {
            assert_eq!(tabs.open("tab", true), Ok(i as u32 + 1));
        }
        assert!(tabs.is_full());
        assert_eq!(tabs.open("ninth", true), Err("ninth"));
        assert_eq!(tabs.len(), MAX_TABS);
        assert_eq!(tabs.active_id(), Some(MAX_TABS as u32));
        // Closing one makes room again.
        tabs.close(1, || unreachable!());
        assert!(tabs.open("ninth", true).is_ok());
    }

    #[test]
    fn closing_the_shown_tab_shows_its_neighbour() {
        let mut tabs = Tabs::new();
        for name in ["a", "b", "c", "d"] {
            tabs.open(name, true).unwrap();
        }
        assert_eq!(order(&tabs), [1, 2, 3, 4]);
        // The middle one: the one to its right takes its place.
        tabs.select(2);
        assert_eq!(tabs.close(2, || unreachable!()), Some("b"));
        assert_eq!(tabs.active_id(), Some(3));
        // The last one: the one to its left.
        tabs.select(4);
        assert_eq!(tabs.close(4, || unreachable!()), Some("d"));
        assert_eq!(tabs.active_id(), Some(3));
        // One in the background: the shown one stays.
        assert_eq!(tabs.close(1, || unreachable!()), Some("a"));
        assert_eq!(tabs.active_id(), Some(3));
        assert_eq!(tabs.close(99, || unreachable!()), None);
        assert_eq!(order(&tabs), [3]);
    }

    #[test]
    fn closing_the_last_tab_opens_a_fresh_one() {
        let mut tabs = Tabs::new();
        tabs.open("a", true).unwrap();
        let mut made = 0;
        assert_eq!(
            tabs.close(1, || {
                made += 1;
                "blank"
            }),
            Some("a")
        );
        assert_eq!(made, 1);
        assert_eq!(tabs.len(), 1);
        // A new id: the window must not mistake it for the closed tab.
        assert_eq!(tabs.active_id(), Some(2));
        assert_eq!(tabs.active(), Some(&"blank"));
    }

    #[test]
    fn select_only_knows_open_tabs() {
        let mut tabs = Tabs::new();
        tabs.open("a", true).unwrap();
        tabs.open("b", false).unwrap();
        assert!(tabs.select(2));
        assert_eq!(tabs.active_id(), Some(2));
        assert!(!tabs.select(7));
        assert_eq!(tabs.active_id(), Some(2));
        *tabs.get_mut(1).unwrap() = "z";
        assert_eq!(tabs.get(1), Some(&"z"));
    }

    #[test]
    fn neighbours_wrap_around() {
        let mut tabs = Tabs::new();
        assert_eq!(tabs.neighbour(1), None);
        tabs.open("a", true).unwrap();
        assert_eq!(tabs.neighbour(1), Some(1));
        assert_eq!(tabs.neighbour(-1), Some(1));
        tabs.open("b", true).unwrap();
        tabs.open("c", true).unwrap();
        assert_eq!(tabs.neighbour(1), Some(1));
        assert_eq!(tabs.neighbour(-1), Some(2));
        tabs.select(1);
        assert_eq!(tabs.neighbour(-1), Some(3));
        assert_eq!(tabs.neighbour(1), Some(2));
    }
}
