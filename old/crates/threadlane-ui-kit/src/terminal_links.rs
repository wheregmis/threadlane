//! Frozen terminal link menu presentation. Hosts validate and route navigation requests.
use gpui::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use std::{collections::HashSet, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalLinkDestination {
    Threadlane,
    DefaultBrowser,
}

pub fn terminal_link_overlay(menu: &Entity<PopupMenu>) -> AnyElement {
    deferred(anchored().child(menu.clone()))
        .with_priority(gpui_kit::base::POPUP_PRIORITY)
        .into_any_element()
}

pub fn terminal_link_commands(
    menu: PopupMenu,
    url: String,
    in_app_browser: bool,
    on_open: impl Fn(String, TerminalLinkDestination, &mut Window, &mut App) + 'static,
) -> PopupMenu {
    let callback = Rc::new(on_open);
    let mut menu = menu;
    if in_app_browser {
        let callback = callback.clone();
        let url = url.clone();
        menu = menu.item(
            PopupMenuItem::new(format!("Open in Threadlane browser — {url}")).on_click(
                move |_, window, cx| {
                    callback(url.clone(), TerminalLinkDestination::Threadlane, window, cx)
                },
            ),
        );
    }
    menu.item(
        PopupMenuItem::new(format!("Open in default browser — {url}")).on_click(
            move |_, window, cx| {
                callback(
                    url.clone(),
                    TerminalLinkDestination::DefaultBrowser,
                    window,
                    cx,
                )
            },
        ),
    )
}

pub struct TerminalLinkPicker {
    urls: Vec<String>,
    retry_url: Option<String>,
    in_app_browser: bool,
    full_screen: bool,
}

impl TerminalLinkPicker {
    pub fn new(urls: impl IntoIterator<Item = String>) -> Self {
        let mut seen = HashSet::new();
        Self {
            urls: urls
                .into_iter()
                .filter(|url| seen.insert(url.clone()))
                .collect(),
            retry_url: None,
            in_app_browser: false,
            full_screen: false,
        }
    }
    pub fn retry_url(mut self, url: Option<String>) -> Self {
        self.retry_url = url;
        self
    }
    pub fn in_app_browser(mut self, available: bool) -> Self {
        self.in_app_browser = available;
        self
    }
    pub fn full_screen(mut self, full_screen: bool) -> Self {
        self.full_screen = full_screen;
        self
    }

    pub fn render(
        self,
        menu: PopupMenu,
        on_open: impl Fn(String, TerminalLinkDestination, &mut Window, &mut App) + 'static,
    ) -> PopupMenu {
        let callback = Rc::new(on_open);
        let mut menu = menu.label("Links in visible output").scrollable(true);
        if self.full_screen {
            return menu.label("Links unavailable in full-screen terminal applications");
        }
        if !self.in_app_browser {
            menu = menu.label("Threadlane browser is not available on this platform");
        }
        if let Some(url) = &self.retry_url {
            let callback = callback.clone();
            menu = menu.label("Navigation could not start — retry or open externally");
            menu = terminal_link_commands(
                menu,
                url.clone(),
                self.in_app_browser,
                move |url, destination, window, cx| callback(url, destination, window, cx),
            )
            .separator();
        }
        if self.urls.is_empty() && self.retry_url.is_none() {
            return menu
                .label("No web links in visible output")
                .label("Scroll older output into view to find links");
        }
        for url in self.urls {
            if self.retry_url.as_ref() == Some(&url) {
                continue;
            }
            let callback = callback.clone();
            menu = terminal_link_commands(
                menu,
                url,
                self.in_app_browser,
                move |url, destination, window, cx| callback(url, destination, window, cx),
            );
        }
        menu
    }
}
