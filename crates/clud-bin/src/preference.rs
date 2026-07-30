//! Shared pure state machine for typed launch/settings choices.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChoiceOption<T> {
    pub value: T,
    pub label: &'static str,
    pub note: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceSelector<T> {
    options: Vec<ChoiceOption<T>>,
    selected: usize,
    cancel_value: T,
}

impl<T: Copy + PartialEq> ChoiceSelector<T> {
    pub fn new(options: &[ChoiceOption<T>], selected: T, cancel_value: T) -> Self {
        assert!(!options.is_empty(), "choice selector requires an option");
        let selected = options
            .iter()
            .position(|option| option.value == selected)
            .unwrap_or(0);
        Self {
            options: options.to_vec(),
            selected,
            cancel_value,
        }
    }

    pub fn selected(&self) -> T {
        self.options[self.selected].value
    }

    pub fn selected_option(&self) -> ChoiceOption<T> {
        self.options[self.selected]
    }

    pub fn options(&self) -> &[ChoiceOption<T>] {
        &self.options
    }

    pub fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn next(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    pub fn cycle(&mut self) {
        self.selected = (self.selected + 1) % self.options.len();
    }

    pub fn confirm(&self) -> T {
        self.selected()
    }

    pub fn cancel(&self) -> T {
        self.cancel_value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: [ChoiceOption<u8>; 3] = [
        ChoiceOption {
            value: 1,
            label: "one",
            note: "",
        },
        ChoiceOption {
            value: 2,
            label: "two",
            note: "",
        },
        ChoiceOption {
            value: 3,
            label: "three",
            note: "",
        },
    ];

    #[test]
    fn selection_navigation_clamps_and_cycles() {
        let mut selector = ChoiceSelector::new(&OPTIONS, 2, 1);
        assert_eq!(selector.selected(), 2);
        selector.next();
        selector.next();
        assert_eq!(selector.selected(), 3);
        selector.previous();
        assert_eq!(selector.selected(), 2);
        selector.cycle();
        selector.cycle();
        assert_eq!(selector.selected(), 1);
    }

    #[test]
    fn confirm_and_cancel_are_distinct() {
        let selector = ChoiceSelector::new(&OPTIONS, 3, 1);
        assert_eq!(selector.confirm(), 3);
        assert_eq!(selector.cancel(), 1);
    }
}
