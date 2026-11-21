//! Icons used by the desktop and the kit source registered to load them.

use gpui_kit::SharedString;
use gpui_kit::assets::{IconName, IconNamed};

pub(super) use gpui_kit::assets::AllAssets as DesktopAssets;

macro_rules! desktop_icons {
    ($($name:ident),+ $(,)?) => {
        /// Desktop icons selected from the kit's full catalog.
        #[derive(Clone, Copy, Debug)]
        pub(super) enum AppIcon {
            $($name),+
        }

        impl IconNamed for AppIcon {
            fn path(self) -> SharedString {
                match self {
                    $(Self::$name => IconName::$name.path()),+
                }
            }
        }

        #[cfg(test)]
        const USED_ICONS: &[AppIcon] = &[$(AppIcon::$name),+];
    };
}

desktop_icons!(
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    BookOpen,
    Bot,
    CalendarClock,
    CalendarPlus,
    ChartColumn,
    Check,
    ChevronDown,
    ChevronRight,
    CircleDashed,
    Clock,
    Columns3,
    Copy,
    ExternalLink,
    FileText,
    Folder,
    FolderOpen,
    FolderPlus,
    GitFork,
    Inbox,
    KeyRound,
    ListPlus,
    MessagesSquare,
    Minus,
    Monitor,
    PanelLeft,
    PanelLeftClose,
    PanelLeftOpen,
    Pause,
    Pencil,
    Play,
    Plus,
    Save,
    Search,
    Send,
    Settings,
    ShieldOff,
    Square,
    Trash,
    Unlink,
    UserPlus,
    X,
);

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;

    use super::{DesktopAssets, IconNamed as _, USED_ICONS};

    #[test]
    fn every_desktop_icon_loads_from_the_registered_assets() {
        for icon in USED_ICONS {
            let path = icon.path();
            let bytes = DesktopAssets.load(&path);
            assert!(
                matches!(bytes, Ok(Some(ref svg)) if !svg.is_empty()),
                "{icon:?} must load SVG bytes from {path}: {bytes:?}"
            );
        }
    }
}
