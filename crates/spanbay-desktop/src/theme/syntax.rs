//! Curated light/dark-paired code syntax highlighting palettes for API workbenches.
//!
//! Pure UI-framework-agnostic RGB token tables consumed by Layer 3's `HighlightTheme` builder and
//! Settings preview cards. Every scheme harmonizes its editor background, gutter, active line,
//! and selection with the active SpanBay eye-care [`ResolvedPalette`] so code blocks blend
//! naturally without harsh white/black cutouts.

use serde::{Deserialize, Serialize};

use super::{ResolvedPalette, RgbaColor};

/// Built-in code syntax highlighting color schemes (each paired for Light & Dark modes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxThemeId {
    /// Catppuccin Latte (light) / Mocha (dark) — soft low-glare default for API payloads.
    #[default]
    Catppuccin,
    /// Rosé Pine Dawn (light) / Moon (dark) — warm editorial paper tones.
    RosePine,
    /// Tokyo Night Day (light) / Storm (dark) — crisp modern IDE contrast.
    TokyoNight,
    /// GitHub Light / Dark Dimmed — clean, restrained engineering palette.
    GitHub,
    /// One Light / One Dark Pro — iconic balanced multi-hue contrast.
    OnePro,
    /// Dynamically derived from the active SpanBay OKLCH eye-care palette & accent.
    FollowPalette,
}

impl SyntaxThemeId {
    pub const ALL: [Self; 6] = [
        Self::Catppuccin,
        Self::RosePine,
        Self::TokyoNight,
        Self::GitHub,
        Self::OnePro,
        Self::FollowPalette,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Catppuccin => "catppuccin",
            Self::RosePine => "rose_pine",
            Self::TokyoNight => "tokyo_night",
            Self::GitHub => "github",
            Self::OnePro => "one_pro",
            Self::FollowPalette => "follow_palette",
        }
    }

    /// Canonical English/display name shown in theme headers and tooltips.
    #[must_use]
    pub const fn variant_pair(self, is_dark: bool) -> &'static str {
        match (self, is_dark) {
            (Self::Catppuccin, false) => "Latte",
            (Self::Catppuccin, true) => "Mocha",
            (Self::RosePine, false) => "Dawn",
            (Self::RosePine, true) => "Moon",
            (Self::TokyoNight, false) => "Day",
            (Self::TokyoNight, true) => "Storm",
            (Self::GitHub, false) => "Light",
            (Self::GitHub, true) => "Dark Dimmed",
            (Self::OnePro, false) => "One Light",
            (Self::OnePro, true) => "One Dark Pro",
            (Self::FollowPalette, false) => "OKLCH Light",
            (Self::FollowPalette, true) => "OKLCH Dark",
        }
    }
}

/// Framework-agnostic resolved code syntax color tokens for one `(SyntaxThemeId, ResolvedPalette)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedSyntaxPalette {
    pub id: SyntaxThemeId,
    pub is_dark: bool,
    /// JSON keys, object properties, YAML keys.
    pub key: RgbaColor,
    /// Quoted string literals.
    pub string: RgbaColor,
    /// String escapes & regex tokens.
    pub escape: RgbaColor,
    /// Numeric literals.
    pub number: RgbaColor,
    /// Booleans, null, constants.
    pub boolean: RgbaColor,
    /// Language keywords & preprocessor directives.
    pub keyword: RgbaColor,
    /// Functions, methods, constructors.
    pub function: RgbaColor,
    /// Types, enums, classes.
    pub type_name: RgbaColor,
    /// XML/HTML tags.
    pub tag: RgbaColor,
    /// XML/HTML attributes & labels.
    pub attribute: RgbaColor,
    /// Operators (`=`, `->`, `&&`).
    pub operator: RgbaColor,
    /// Identifiers & variables.
    pub variable: RgbaColor,
    /// Comments & doc comments (rendered italic).
    pub comment: RgbaColor,
    /// Brackets, delimiters, commas, colons (quiet structural framing).
    pub punctuation: RgbaColor,
}

impl ResolvedSyntaxPalette {
    /// Five representative swatch dots `[key, string, number, keyword, tag]` for compact UI cards.
    #[must_use]
    pub const fn swatch_dots(&self) -> [RgbaColor; 5] {
        [self.key, self.string, self.number, self.keyword, self.tag]
    }
}

const fn hex(v: u32) -> RgbaColor {
    RgbaColor::rgb(
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    )
}

/// Resolves concrete syntax token colors for `id` harmonized with `base`'s light/dark mode and
/// eye-care tokens.
#[must_use]
pub fn resolve_syntax_palette(id: SyntaxThemeId, base: &ResolvedPalette) -> ResolvedSyntaxPalette {
    let is_dark = base.is_dark;
    match (id, is_dark) {
        (SyntaxThemeId::Catppuccin, false) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x1E66F5),
            string: hex(0x2B7A2C),
            escape: hex(0xC61D42),
            number: hex(0xBD4D0C),
            boolean: hex(0x7C3AED),
            keyword: hex(0x7C3AED),
            function: hex(0x1E66F5),
            type_name: hex(0x9A6108),
            tag: hex(0x0F766E),
            attribute: hex(0x7C3AED),
            operator: hex(0x0F766E),
            variable: base.ink,
            comment: hex(0x6C6F85),
            punctuation: base.muted,
        },
        (SyntaxThemeId::Catppuccin, true) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x89B4FA),
            string: hex(0xA6E3A1),
            escape: hex(0xF2CDCD),
            number: hex(0xFAB387),
            boolean: hex(0xCBA6F7),
            keyword: hex(0xCBA6F7),
            function: hex(0x89DCEB),
            type_name: hex(0xF9E2AF),
            tag: hex(0xF38BA8),
            attribute: hex(0xF9E2AF),
            operator: hex(0x94E2D5),
            variable: base.ink,
            comment: hex(0x9399B2),
            punctuation: base.muted,
        },
        (SyntaxThemeId::RosePine, false) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x286983),
            string: hex(0xA35D12),
            escape: hex(0xB44C6B),
            number: hex(0xB44C6B),
            boolean: hex(0x6D5494),
            keyword: hex(0x286983),
            function: hex(0x9E5049),
            type_name: hex(0x3B7380),
            tag: hex(0x286983),
            attribute: hex(0x6D5494),
            operator: hex(0x6E6A86),
            variable: base.ink,
            comment: hex(0x797593),
            punctuation: base.muted,
        },
        (SyntaxThemeId::RosePine, true) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x9CCFD8),
            string: hex(0xF6C177),
            escape: hex(0xEA9A97),
            number: hex(0xEB6F92),
            boolean: hex(0xC4A7E7),
            keyword: hex(0x76BEDB),
            function: hex(0xEA9A97),
            type_name: hex(0x9CCFD8),
            tag: hex(0xEB6F92),
            attribute: hex(0xC4A7E7),
            operator: hex(0x908CAA),
            variable: base.ink,
            comment: hex(0x908CAA),
            punctuation: base.muted,
        },
        (SyntaxThemeId::TokyoNight, false) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x2E5EC9),
            string: hex(0x38701D),
            escape: hex(0x006A83),
            number: hex(0xA85700),
            boolean: hex(0x7847BD),
            keyword: hex(0x7847BD),
            function: hex(0x2E5EC9),
            type_name: hex(0x006A83),
            tag: hex(0xB83B3B),
            attribute: hex(0x7847BD),
            operator: hex(0x006A83),
            variable: base.ink,
            comment: hex(0x6E7494),
            punctuation: base.muted,
        },
        (SyntaxThemeId::TokyoNight, true) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x7AA2F7),
            string: hex(0x9ECE6A),
            escape: hex(0x89DDFF),
            number: hex(0xFF9E64),
            boolean: hex(0xBB9AF7),
            keyword: hex(0xBB9AF7),
            function: hex(0x7DCFFF),
            type_name: hex(0x2AC3DE),
            tag: hex(0xF7768E),
            attribute: hex(0xE0AF68),
            operator: hex(0x89DDFF),
            variable: base.ink,
            comment: hex(0x828FB8),
            punctuation: base.muted,
        },
        (SyntaxThemeId::GitHub, false) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x0550AE),
            string: hex(0x0A3069),
            escape: hex(0x116329),
            number: hex(0x0550AE),
            boolean: hex(0xCF222E),
            keyword: hex(0xCF222E),
            function: hex(0x7A42D8),
            type_name: hex(0x953800),
            tag: hex(0x116329),
            attribute: hex(0x0550AE),
            operator: hex(0xCF222E),
            variable: base.ink,
            comment: hex(0x57606A),
            punctuation: base.muted,
        },
        (SyntaxThemeId::GitHub, true) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0x6CB6FF),
            string: hex(0x96D0FF),
            escape: hex(0x8DDB8C),
            number: hex(0x79C0FF),
            boolean: hex(0xF47067),
            keyword: hex(0xF47067),
            function: hex(0xDCBDFB),
            type_name: hex(0xF69D50),
            tag: hex(0x8DDB8C),
            attribute: hex(0x6CB6FF),
            operator: hex(0xF47067),
            variable: base.ink,
            comment: hex(0x8B949E),
            punctuation: base.muted,
        },
        (SyntaxThemeId::OnePro, false) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0xBC382E),
            string: hex(0x367A37),
            escape: hex(0x017485),
            number: hex(0x8F6100),
            boolean: hex(0x97279B),
            keyword: hex(0x97279B),
            function: hex(0x3264D6),
            type_name: hex(0x8F6100),
            tag: hex(0xBC382E),
            attribute: hex(0x8F6100),
            operator: hex(0x017485),
            variable: base.ink,
            comment: hex(0x696C77),
            punctuation: base.muted,
        },
        (SyntaxThemeId::OnePro, true) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: hex(0xE06C75),
            string: hex(0x98C379),
            escape: hex(0x56B6C2),
            number: hex(0xD19A66),
            boolean: hex(0xC678DD),
            keyword: hex(0xC678DD),
            function: hex(0x61AFEF),
            type_name: hex(0xE5C07B),
            tag: hex(0xE06C75),
            attribute: hex(0xD19A66),
            operator: hex(0x56B6C2),
            variable: base.ink,
            comment: hex(0x848B98),
            punctuation: base.muted,
        },
        (SyntaxThemeId::FollowPalette, _) => ResolvedSyntaxPalette {
            id,
            is_dark,
            key: base.tk_key,
            string: base.tk_str,
            escape: base.accent_ink,
            number: base.tk_num,
            boolean: base.tk_kw,
            keyword: base.tk_kw,
            function: base.tk_fn,
            type_name: base.ph_tls,
            tag: base.m_delete,
            attribute: base.tk_fn,
            operator: base.accent_ink,
            variable: base.ink,
            comment: base.tk_com,
            punctuation: base.muted,
        },
    }
}
