use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::Icon;
#[cfg(not(target_family = "wasm"))]
use gpui_kit_assets::Assets as ComponentAssets;
#[cfg(target_family = "wasm")]
use crate::web_assets::ComponentAssets;

/// Vendored Threadlane icon bytes by path — the same set [`Assets::load`]
/// serves before falling back to the kit bundle.
pub fn vendored_icon_bytes(path: &str) -> Option<&'static [u8]> {
    match path {
        "icons/providers/openai.svg" => {
            Some(include_bytes!("../assets/icons/providers/openai.svg"))
        }
        "icons/providers/google.svg" => {
            Some(include_bytes!("../assets/icons/providers/google.svg"))
        }
        "icons/providers/opencode.svg" => {
            Some(include_bytes!("../assets/icons/providers/opencode.svg"))
        }
        "icons/providers/acp.svg" => Some(include_bytes!("../assets/icons/providers/acp.svg")),
        "icons/effort.svg" => Some(include_bytes!("../assets/icons/effort.svg")),
        "icons/download.svg" => Some(include_bytes!("../assets/icons/download.svg")),
        "icons/refresh-cw.svg" => Some(include_bytes!("../assets/icons/refresh-cw.svg")),
        "icons/eraser.svg" => Some(include_bytes!("../assets/icons/eraser.svg")),
        "icons/archive.svg" => Some(include_bytes!("../assets/icons/archive.svg")),
        "icons/folder-plus.svg" => Some(include_bytes!("../assets/icons/folder-plus.svg")),
        "icons/square-pen.svg" => Some(include_bytes!("../assets/icons/square-pen.svg")),
        "icons/quote.svg" => Some(include_bytes!("../assets/icons/quote.svg")),
        "icons/crosshair.svg" => Some(include_bytes!("../assets/icons/crosshair.svg")),
        "icons/lock.svg" => Some(include_bytes!("../assets/icons/lock.svg")),
        "icons/pin.svg" => Some(include_bytes!("../assets/icons/pin.svg")),
        "icons/smartphone.svg" => Some(include_bytes!("../assets/icons/smartphone.svg")),
        "icons/tabs/trajectory.svg" => Some(include_bytes!("../assets/icons/tabs/trajectory.svg")),
        "icons/tabs/chat.svg" => Some(include_bytes!("../assets/icons/tabs/chat.svg")),
        "icons/tabs/editor.svg" => Some(include_bytes!("../assets/icons/tabs/editor.svg")),
        "icons/git/actions.svg" => Some(include_bytes!("../assets/icons/git/actions.svg")),
        "icons/git/commit.svg" => Some(include_bytes!("../assets/icons/git/commit.svg")),
        "icons/git/compare.svg" => Some(include_bytes!("../assets/icons/git/compare.svg")),
        "icons/git/branch.svg" => Some(include_bytes!("../assets/icons/git/branch.svg")),
        "icons/git/comments.svg" => Some(include_bytes!("../assets/icons/git/comments.svg")),
        "icons/git/issue.svg" => Some(include_bytes!("../assets/icons/git/issue.svg")),
        "icons/git/pull-request.svg" => {
            Some(include_bytes!("../assets/icons/git/pull-request.svg"))
        }
        "icons/threadlane.svg" | "icons/threadlane-logo.svg" => {
            Some(include_bytes!("../assets/icons/threadlane.svg"))
        }
        _ => None,
    }
}

/// A vendored Threadlane icon rendered without a process `AssetSource`.
///
/// `Icon::path()` resolves through the app-wide asset registry, which the
/// iOS platform does not install — mobile clients embed the bytes directly
/// via [`Icon::data`]. Returns `None` for paths this crate does not vendor.
pub fn bundled_icon(path: &str) -> Option<Icon> {
    vendored_icon_bytes(path).map(|bytes| Icon::default().data(bytes))
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match vendored_icon_bytes(path) {
            Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            None => ComponentAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        assets.extend(
            [
                "icons/providers/openai.svg",
                "icons/providers/google.svg",
                "icons/providers/opencode.svg",
                "icons/providers/acp.svg",
                "icons/effort.svg",
                "icons/download.svg",
                "icons/refresh-cw.svg",
                "icons/eraser.svg",
                "icons/archive.svg",
                "icons/folder-plus.svg",
                "icons/square-pen.svg",
                "icons/crosshair.svg",
                "icons/lock.svg",
                "icons/pin.svg",
                "icons/smartphone.svg",
                "icons/tabs/trajectory.svg",
                "icons/tabs/chat.svg",
                "icons/tabs/editor.svg",
                "icons/git/actions.svg",
                "icons/git/commit.svg",
                "icons/git/compare.svg",
                "icons/git/branch.svg",
                "icons/git/comments.svg",
                "icons/git/issue.svg",
                "icons/git/pull-request.svg",
                "icons/threadlane.svg",
                "icons/threadlane-logo.svg",
            ]
            .into_iter()
            .filter(|asset| asset.starts_with(path))
            .map(SharedString::from),
        );
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_custom_assets() {
        let assets = Assets;
        assert!(
            assets.load("icons/git/pull-request.svg").unwrap().is_some(),
            "pull-request.svg must load"
        );
        assert!(
            assets.load("icons/git/issue.svg").unwrap().is_some(),
            "issue.svg must load"
        );
        assert!(
            assets.load("icons/threadlane.svg").unwrap().is_some(),
            "threadlane.svg must load"
        );
    }
}
