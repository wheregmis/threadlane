//! Back and forward screen navigation history (`Cmd+[` / `Cmd+]`).

use crate::Screen;

const MAX_HISTORY: usize = 64;

#[derive(Default)]
pub(crate) struct NavHistory {
    back: Vec<Screen>,
    forward: Vec<Screen>,
}

impl NavHistory {
    /// Records moving from `current` to `next`, clearing any forward branch.
    pub(crate) fn record(&mut self, current: Screen, next: &Screen) {
        if &current == next {
            return;
        }
        if self.back.last() != Some(&current) {
            if self.back.len() >= MAX_HISTORY {
                self.back.remove(0);
            }
            self.back.push(current);
        }
        self.forward.clear();
    }

    /// Steps backward, pushing `current` onto the forward stack.
    pub(crate) fn go_back(&mut self, current: Screen) -> Option<Screen> {
        let target = self.back.pop()?;
        if self.forward.len() >= MAX_HISTORY {
            self.forward.remove(0);
        }
        self.forward.push(current);
        Some(target)
    }

    /// Steps forward, pushing `current` onto the back stack.
    pub(crate) fn go_forward(&mut self, current: Screen) -> Option<Screen> {
        let target = self.forward.pop()?;
        if self.back.len() >= MAX_HISTORY {
            self.back.remove(0);
        }
        self.back.push(current);
        Some(target)
    }

    pub(crate) fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub(crate) fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Removes entries belonging to an unpaired space.
    pub(crate) fn prune_space(&mut self, space: &str) {
        self.back.retain(|screen| screen.space() != Some(space));
        self.forward.retain(|screen| screen.space() != Some(space));
    }
}

#[cfg(test)]
mod tests {
    use super::NavHistory;
    use crate::{Screen, Tab};

    #[test]
    fn back_and_forward_walk_history_and_clear_forward_on_new_visit() {
        let mut history = NavHistory::default();
        let home = Screen::Home;
        let tasks = Screen::SpaceTasks {
            space: "dev".into(),
        };
        let project = Screen::Project {
            space: "dev".into(),
            project: "api".into(),
            tab: Tab::Conversation,
        };
        let machines = Screen::Machines;

        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());

        history.record(home.clone(), &tasks);
        history.record(tasks.clone(), &project);
        assert!(history.can_go_back());
        assert!(!history.can_go_forward());

        assert_eq!(history.go_back(project.clone()), Some(tasks.clone()));
        assert!(history.can_go_forward());
        assert_eq!(history.go_back(tasks.clone()), Some(home.clone()));
        assert!(!history.can_go_back());

        assert_eq!(history.go_forward(home.clone()), Some(tasks.clone()));
        history.record(tasks.clone(), &machines);
        assert!(!history.can_go_forward());
        assert_eq!(history.go_back(machines), Some(tasks));
    }

    #[test]
    fn prune_space_drops_screens_for_unpaired_space() {
        let mut history = NavHistory::default();
        let home = Screen::Home;
        let dev = Screen::SpaceTasks {
            space: "dev".into(),
        };
        let ops = Screen::SpaceTasks {
            space: "ops".into(),
        };

        history.record(home.clone(), &dev);
        history.record(dev.clone(), &ops);
        history.prune_space("dev");

        assert_eq!(history.go_back(ops), Some(home));
        assert!(!history.can_go_back());
    }
}
