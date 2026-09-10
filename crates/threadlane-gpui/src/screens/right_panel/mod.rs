mod browser;
mod draft_pr;
#[cfg(test)]
mod tests;
mod types;
mod view;

pub(crate) use types::{GitAction, Surface};
pub(crate) use view::RightPanelView;
