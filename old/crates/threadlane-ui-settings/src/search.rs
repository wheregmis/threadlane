pub use threadlane_ui_kit::{SettingsSearchItem, SETTINGS_SEARCH_ITEMS};

#[cfg(test)]
mod tests {
    use super::SETTINGS_SEARCH_ITEMS;
    use std::collections::HashSet;

    #[test]
    fn settings_search_destinations_have_unique_ids_and_requested_aliases() {
        let ids = SETTINGS_SEARCH_ITEMS
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), SETTINGS_SEARCH_ITEMS.len());
        for term in [
            "theme",
            "upgrade",
            "auto review",
            "github token",
            "fusion",
            "shortcuts",
        ] {
            assert!(
                SETTINGS_SEARCH_ITEMS
                    .iter()
                    .any(|item| item.keywords.contains(&term)),
                "missing alias {term}"
            );
        }
    }
}
