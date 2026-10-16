//! The interface language, portable between the desktop binary and the
//! Android library.
//!
//! This holds only the locale *identity* (the tag, its parsing, and the
//! native name) so core code can name the interface language without pulling
//! in `fastframe-i18n`. The gettext catalogs, operating-system language
//! detection, and localized count/label strings depend on `fastframe-i18n`
//! and stay in the desktop crate's `i18n` module (see `LocaleExt` there).

/// The languages Spotifast's interface is translated into.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Locale {
    #[default]
    English,
    German,
    Spanish,
    Dutch,
    PortugueseBrazil,
    PortuguesePortugal,
    French,
    Swedish,
    Polish,
    Russian,
    Italian,
    Japanese,
    ChineseSimplified,
    ChineseTraditional,
    Turkish,
    Ukrainian,
}

impl Locale {
    /// The tag this locale is named by on the command line and in
    /// `settings.json`. [`Self::from_tag`] reads each one back.
    pub fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::German => "de-DE",
            Self::Spanish => "es",
            Self::Dutch => "nl",
            Self::PortugueseBrazil => "pt-BR",
            Self::PortuguesePortugal => "pt-PT",
            Self::French => "fr",
            Self::Swedish => "sv",
            Self::Polish => "pl",
            Self::Russian => "ru",
            Self::Italian => "it",
            Self::Japanese => "ja",
            Self::ChineseSimplified => "zh-Hans",
            Self::ChineseTraditional => "zh-Hant",
            Self::Turkish => "tr",
            Self::Ukrainian => "uk",
        }
    }

    /// The locale a stored or typed tag names exactly, as [`Self::tag`]
    /// writes it or by one of its command-line aliases (`de`, `en-US`),
    /// matching case-insensitively.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag.to_ascii_lowercase().as_str() {
            "en" | "en-us" => Self::English,
            "de-de" | "de" => Self::German,
            "es" => Self::Spanish,
            "nl" => Self::Dutch,
            "pt-br" => Self::PortugueseBrazil,
            "pt-pt" => Self::PortuguesePortugal,
            "fr" => Self::French,
            "sv" => Self::Swedish,
            "pl" => Self::Polish,
            "ru" => Self::Russian,
            "it" => Self::Italian,
            "ja" => Self::Japanese,
            "zh-hans" => Self::ChineseSimplified,
            "zh-hant" => Self::ChineseTraditional,
            "tr" => Self::Turkish,
            "uk" => Self::Ukrainian,
            _ => return None,
        })
    }

    /// The language's name in that language. A listener who has the app in a
    /// language they cannot read must still find their own in the picker, so
    /// these are never translated.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::German => "Deutsch",
            Self::Spanish => "Español",
            Self::Dutch => "Nederlands",
            Self::PortugueseBrazil => "Português (Brasil)",
            Self::PortuguesePortugal => "Português (Portugal)",
            Self::French => "Français",
            Self::Swedish => "Svenska",
            Self::Polish => "Polski",
            Self::Russian => "Русский",
            Self::Italian => "Italiano",
            Self::Japanese => "日本語",
            Self::ChineseSimplified => "简体中文",
            Self::ChineseTraditional => "繁體中文",
            Self::Turkish => "Türkçe",
            Self::Ukrainian => "Українська",
        }
    }
}

/// Every bundled language, in the order the Settings picker lists them: by
/// their own names, as a reader of each would look for them.
pub const LOCALES: &[Locale] = &[
    Locale::German,
    Locale::English,
    Locale::Spanish,
    Locale::French,
    Locale::Italian,
    Locale::Dutch,
    Locale::Polish,
    Locale::PortugueseBrazil,
    Locale::PortuguesePortugal,
    Locale::Swedish,
    Locale::Turkish,
    Locale::Russian,
    Locale::Ukrainian,
    Locale::Japanese,
    Locale::ChineseSimplified,
    Locale::ChineseTraditional,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_locale_is_english() {
        assert_eq!(Locale::default(), Locale::English);
    }

    #[test]
    fn every_locale_round_trips_through_its_tag_and_is_listed_once() {
        for &locale in LOCALES {
            assert_eq!(Locale::from_tag(locale.tag()), Some(locale));
            assert!(!locale.native_name().is_empty());
            assert_eq!(LOCALES.iter().filter(|other| **other == locale).count(), 1);
        }
        assert_eq!(Locale::from_tag("de"), Some(Locale::German));
        assert_eq!(Locale::from_tag("en-US"), Some(Locale::English));
        assert_eq!(Locale::from_tag("tr"), Some(Locale::Turkish));
        assert_eq!(Locale::from_tag("uk"), Some(Locale::Ukrainian));
        assert_eq!(Locale::from_tag("klingon"), None);
    }
}
