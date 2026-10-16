//! Desktop-only localized labels for the portable model enums. The gettext
//! catalogs live here, so the label methods stay out of `spotifast_core`.

use std::borrow::Cow;

use spotifast_core::model::{DiscographyFilter, SearchFilter};

use crate::i18n::{Locale, gettext, pgettext};

pub trait SearchFilterExt: Sized {
    fn label(self, locale: Locale) -> Cow<'static, str>;
}

impl SearchFilterExt for SearchFilter {
    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            SearchFilter::All => pgettext(locale, "filter", "All"),
            SearchFilter::Songs => gettext(locale, "Songs"),
            SearchFilter::Artists => gettext(locale, "Artists"),
            SearchFilter::Albums => gettext(locale, "Albums"),
            SearchFilter::Playlists => gettext(locale, "Playlists"),
            SearchFilter::Podcasts => gettext(locale, "Podcasts"),
            SearchFilter::Episodes => gettext(locale, "Episodes"),
        }
    }
}

pub trait DiscographyFilterExt: Sized {
    fn label(self, locale: Locale) -> Cow<'static, str>;
}

impl DiscographyFilterExt for DiscographyFilter {
    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            DiscographyFilter::All => pgettext(locale, "filter", "All"),
            DiscographyFilter::Albums => gettext(locale, "Albums"),
            DiscographyFilter::Singles => gettext(locale, "Singles & EPs"),
            DiscographyFilter::AppearsOn => gettext(locale, "Appears On"),
        }
    }
}
