//! Semantic icon registry isolating `gpui_kit::assets::IconName` inside `src/ui/`.

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, Sizable as _, Size};
use gpui_kit::{
    App, Div, ObjectFit, ParentElement as _, Styled as _, StyledImage as _, div, img, px, svg,
};

use super::assets::{APP_ICON_PATH, PINGMARK_BODY_SVG_PATH, PINGMARK_DOT_SVG_PATH};
use super::theme::{palette, to_hsla};

/// Semantic icons used across PingDeck views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiIcon {
    Send,
    Server,
    Workflow,
    History,
    Layers,
    Chat,
    Cpu,
    Usage,
    Settings,
    Search,
    Folder,
    FolderOpen,
    FileCode,
    FileDiff,
    FilePen,
    FileText,
    Plus,
    Trash,
    Play,
    Stop,
    Refresh,
    Copy,
    Check,
    Close,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    PanelLeft,
    PanelRight,
    PanelRightClose,
    PanelRightOpen,
    Globe,
    Key,
    Shield,
    ShieldAlert,
    ShieldCheck,
    Lock,
    Terminal,
    SquareTerminal,
    Sun,
    Moon,
    Sparkles,
    Bot,
    Brain,
    Download,
    Upload,
    Warning,
    Info,
    Cookie,
    Certificate,
    Activity,
    Sliders,
    Zap,
    Wrench,
    Branch,
    Eye,
    EyeOff,
    ArrowLeft,
    ArrowUp,
    CornerDownRight,
    CornerUpRight,
    Database,
    Timer,
    Coins,
    Archive,
    Pin,
    PinOff,
    Paperclip,
    ListChecks,
    ListTodo,
    Pencil,
    RotateCcw,
    Ellipsis,
    ExternalLink,
    MessageSquarePlus,
    CircleCheck,
    CircleAlert,
    CircleDot,
    CircleX,
    Diamond,
    Triangle,
}

impl UiIcon {
    #[must_use]
    const fn to_asset(self) -> IconName {
        match self {
            Self::Send => IconName::Send,
            Self::Server => IconName::Server,
            Self::Workflow => IconName::Workflow,
            Self::History => IconName::Clock,
            Self::Layers => IconName::Layers,
            Self::Chat => IconName::MessageSquare,
            Self::Cpu => IconName::Cpu,
            Self::Usage => IconName::ChartBar,
            Self::Settings => IconName::Settings,
            Self::Search => IconName::Search,
            Self::Folder => IconName::Folder,
            Self::FolderOpen => IconName::FolderOpen,
            Self::FileCode => IconName::FileCode,
            Self::FileDiff => IconName::FileDiff,
            Self::FilePen => IconName::FilePen,
            Self::FileText => IconName::FileText,
            Self::Plus => IconName::Plus,
            Self::Trash => IconName::Trash,
            Self::Play => IconName::Play,
            Self::Stop => IconName::Square,
            Self::Refresh => IconName::RefreshCw,
            Self::Copy => IconName::Copy,
            Self::Check => IconName::Check,
            Self::Close => IconName::X,
            Self::ChevronRight => IconName::ChevronRight,
            Self::ChevronDown => IconName::ChevronDown,
            Self::ChevronUp => IconName::ChevronUp,
            Self::PanelLeft => IconName::PanelLeft,
            Self::PanelRight => IconName::PanelRight,
            Self::PanelRightClose => IconName::PanelRightClose,
            Self::PanelRightOpen => IconName::PanelRightOpen,
            Self::Globe => IconName::Globe,
            Self::Key => IconName::KeyRound,
            Self::Shield => IconName::Shield,
            Self::ShieldAlert => IconName::ShieldAlert,
            Self::ShieldCheck => IconName::ShieldCheck,
            Self::Lock => IconName::Lock,
            Self::Terminal => IconName::Terminal,
            Self::SquareTerminal => IconName::SquareTerminal,
            Self::Sun => IconName::Sun,
            Self::Moon => IconName::Moon,
            Self::Sparkles => IconName::Sparkles,
            Self::Bot => IconName::Bot,
            Self::Brain => IconName::Brain,
            Self::Download => IconName::Download,
            Self::Upload => IconName::Upload,
            Self::Warning => IconName::TriangleAlert,
            Self::Info => IconName::Info,
            Self::Cookie => IconName::Cookie,
            Self::Certificate => IconName::FileCheck,
            Self::Activity => IconName::Activity,
            Self::Sliders => IconName::SlidersHorizontal,
            Self::Zap => IconName::Zap,
            Self::Wrench => IconName::Wrench,
            Self::Branch => IconName::GitBranch,
            Self::Eye => IconName::Eye,
            Self::EyeOff => IconName::EyeOff,
            Self::ArrowLeft => IconName::ArrowLeft,
            Self::ArrowUp => IconName::ArrowUp,
            Self::CornerDownRight => IconName::CornerDownRight,
            Self::CornerUpRight => IconName::CornerUpRight,
            Self::Database => IconName::Database,
            Self::Timer => IconName::Timer,
            Self::Coins => IconName::Coins,
            Self::Archive => IconName::Archive,
            Self::Pin => IconName::Pin,
            Self::PinOff => IconName::PinOff,
            Self::Paperclip => IconName::Paperclip,
            Self::ListChecks => IconName::ListChecks,
            Self::ListTodo => IconName::ListTodo,
            Self::Pencil => IconName::Pencil,
            Self::RotateCcw => IconName::RotateCcw,
            Self::Ellipsis => IconName::Ellipsis,
            Self::ExternalLink => IconName::ExternalLink,
            Self::MessageSquarePlus => IconName::MessageSquarePlus,
            Self::CircleCheck => IconName::CircleCheck,
            Self::CircleAlert => IconName::CircleAlert,
            Self::CircleDot => IconName::CircleDot,
            Self::CircleX => IconName::CircleX,
            Self::Diamond => IconName::Diamond,
            Self::Triangle => IconName::Triangle,
        }
    }
}

/// Renders a standard 14px semantic icon.
#[must_use]
pub fn icon(glyph: UiIcon) -> Icon {
    Icon::from(glyph.to_asset()).small()
}

/// Renders a compact 12px semantic icon (toolbars, badges, inline metadata).
#[must_use]
pub fn icon_xs(glyph: UiIcon) -> Icon {
    Icon::from(glyph.to_asset()).xsmall()
}

/// Renders a semantic icon at an explicit component size.
#[must_use]
pub fn icon_sized(glyph: UiIcon, size: Size) -> Icon {
    Icon::from(glyph.to_asset()).with_size(size)
}

/// Renders the official full-color PingDeck application icon (`assets/icons/128x128@2x.png`).
#[must_use]
pub fn app_icon(size_px: f32) -> Div {
    div()
        .size(px(size_px))
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .child(
            img(APP_ICON_PATH)
                .size(px(size_px))
                .object_fit(ObjectFit::Contain),
        )
}

/// Renders PingDeck's crisp two-color vector PingMark (`p` bowl + cobalt ping dot).
#[must_use]
pub fn pingmark(size_px: f32, cx: &App) -> Div {
    let p = palette(cx);
    let w = size_px * (458.0 / 576.0);
    div()
        .relative()
        .w(px(w))
        .h(px(size_px))
        .flex_shrink_0()
        .child(
            svg()
                .path(PINGMARK_BODY_SVG_PATH)
                .w(px(w))
                .h(px(size_px))
                .text_color(to_hsla(p.ink)),
        )
        .child(
            svg()
                .path(PINGMARK_DOT_SVG_PATH)
                .absolute()
                .inset_0()
                .w(px(w))
                .h(px(size_px))
                .text_color(to_hsla(p.accent)),
        )
}
