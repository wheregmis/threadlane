// Embed the same compatibility icons as the native Kit bundle. Its default web
// source fetches /assets/icons asynchronously; Threadlane previews serve these
// directly so disclosure and input controls work without a separate asset host.
gpui_kit_assets::icon_assets!(pub(crate) ComponentAssets, [
    ALargeSmall, ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Asterisk,
    Ban, BatteryCharging, BatteryFull, BatteryLow, BatteryMedium, BatteryWarning,
    Battery, Bell, BookOpen, Bot, Building2, Calendar,
    CaseSensitive, ChartPie, Check, ChevronDown, ChevronLeft, ChevronRight,
    ChevronUp, ChevronsUpDown, CircleAlert, CircleCheck, CircleUser, CircleX,
    Close, Copy, Cpu, Dash, Delete, EllipsisVertical,
    Ellipsis, ExternalLink, EyeOff, Eye, FileText, File,
    FolderClosed, FolderOpen, Folder, Frame, GalleryVerticalEnd, Github,
    Globe, HardDrive, HeartOff, Heart, Inbox, Info,
    Inspector, LayoutDashboard, LoaderCircle, Loader, Map, Maximize,
    MemoryStick, Menu, Minimize, Minus, Moon, Network,
    Palette, PanelBottomOpen, PanelBottom, PanelLeftClose, PanelLeftOpen, PanelLeft,
    PanelRightClose, PanelRightOpen, PanelRight, Pause, Play, Plus,
    Redo2, Redo, RefreshCw, Replace, ResizeCorner, RotateCw,
    Search, Settings2, Settings, SortAscending, SortDescending, SquareTerminal,
    StarFill, StarOff, Star, Sun, ThumbsDown, ThumbsUp,
    TriangleAlert, Undo2, Undo, User, WindowClose, WindowMaximize,
    WindowMinimize, WindowRestore,
]);
