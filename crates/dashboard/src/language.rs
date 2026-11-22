//! Display language.
//!
//! Implements `spec/15-dashboard.md` §6.
//!
//! # English only, for now
//!
//! §6.2. The route and the preference exist; the catalogue does not. §6.3
//! records why that matches what the Python **delivers** rather than what
//! it appears to offer — its Welsh catalogue is 352 empty translations, so
//! selecting Welsh renders English there too (§9.12).
//!
//! Keeping the route and storing the choice means completing the catalogue
//! is the only remaining work, rather than the only remaining work plus
//! re-adding everything around it.

/// A language the dashboard accepts (§6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// English.
    #[default]
    English,
    /// Welsh — accepted and stored; see §6.3 for what it currently renders.
    Welsh,
}

impl Language {
    /// The code used in the form, the cookie and the `lang` attribute.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Welsh => "cy",
        }
    }

    /// The `lang` attribute for the page.
    ///
    /// Region-qualified, because a screen reader picks a voice from it and
    /// `en-GB` is not `en-US`.
    #[must_use]
    pub const fn html_lang(self) -> &'static str {
        match self {
            Self::English => "en-GB",
            Self::Welsh => "cy-GB",
        }
    }

    /// Read a code, falling back to English.
    ///
    /// An unrecognised value is **not** an error: it arrives from a form or
    /// a cookie, and §1.2 makes a page out of anything.
    ///
    /// # Examples
    ///
    /// ```
    /// use dashboard::language::Language;
    ///
    /// assert_eq!(Language::parse("cy"), Language::Welsh);
    /// assert_eq!(Language::parse("EN"), Language::English);
    /// assert_eq!(Language::parse("fr"), Language::English, "unknown falls back");
    /// ```
    #[must_use]
    pub fn parse(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "cy" => Self::Welsh,
            _ => Self::English,
        }
    }
}

/// Where to send the browser after setting a language (§6.4).
///
/// The `Referer` header is client-supplied, so the target is rebuilt from
/// its **path and query only** — scheme and host are discarded, which means
/// the redirect cannot leave this site whatever was sent.
///
/// Backslashes are normalised to `/` first: some browsers resolve
/// `/\evil.example` as a network-path reference, so a target that merely
/// starts with `/` is not enough on its own.
///
/// # Examples
///
/// An ordinary referer comes back as its path:
///
/// ```
/// use dashboard::language::safe_redirect;
///
/// assert_eq!(safe_redirect(Some("https://dash.example/flows?x=1")), "/flows?x=1");
/// assert_eq!(safe_redirect(None), "/");
/// ```
///
/// And an off-site one does not:
///
/// ```
/// use dashboard::language::safe_redirect;
///
/// assert_eq!(safe_redirect(Some("https://evil.example/steal")), "/steal");
/// assert_eq!(safe_redirect(Some("//evil.example/steal")), "/steal");
///
/// // `/\evil.example` normalises to `//evil.example`, which is an
/// // authority with no path — so there is nothing to return to and the
/// // answer is the overview. The Python lands on `/` here too.
/// assert_eq!(safe_redirect(Some("/\\evil.example")), "/");
/// ```
#[must_use]
pub fn safe_redirect(referer: Option<&str>) -> String {
    let Some(referer) = referer.map(|value| value.replace('\\', "/")) else {
        return "/".to_string();
    };

    // Drop any scheme, then any authority. What is left is a path.
    let without_scheme = referer
        .split_once("://")
        .map_or(referer.as_str(), |(_, rest)| rest);

    // A protocol-relative reference (`//host/path`) has no scheme but does
    // have an authority, and it is the form that most often gets through a
    // "starts with /" check.
    let after_authority = if without_scheme.len() == referer.len() {
        // No scheme was present.
        without_scheme.strip_prefix("//").map_or_else(
            || without_scheme.to_string(),
            |rest| {
                rest.split_once('/')
                    .map_or_else(|| "/".to_string(), |(_, path)| format!("/{path}"))
            },
        )
    } else {
        // A scheme was present, so everything up to the next `/` is the
        // host.
        without_scheme
            .split_once('/')
            .map_or_else(|| "/".to_string(), |(_, path)| format!("/{path}"))
    };

    // Strip a fragment, which never reaches a server anyway, and refuse an
    // empty result.
    let path = after_authority.split('#').next().unwrap_or("/").to_string();

    if path.is_empty() || !path.starts_with('/') {
        return "/".to_string();
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_codes_are_accepted() {
        // spec/15-dashboard.md §6.2 — an existing form posts one of these.
        assert_eq!(Language::parse("en"), Language::English);
        assert_eq!(Language::parse("cy"), Language::Welsh);
        assert_eq!(Language::parse(" CY "), Language::Welsh);
    }

    #[test]
    fn anything_else_is_english_rather_than_an_error() {
        // §1.2 — this arrives from a form or a cookie, and the answer is a
        // page either way.
        for code in ["fr", "", "  ", "en-US", "🏥", "../../etc/passwd"] {
            assert_eq!(Language::parse(code), Language::English, "{code:?}");
        }
    }

    #[test]
    fn the_html_lang_is_region_qualified() {
        // A screen reader picks a voice from this, and `en-GB` is not
        // `en-US`.
        assert_eq!(Language::English.html_lang(), "en-GB");
        assert_eq!(Language::Welsh.html_lang(), "cy-GB");
        assert_eq!(Language::English.code(), "en");
        assert_eq!(Language::Welsh.code(), "cy");
    }

    #[test]
    fn a_same_site_referer_comes_back_as_its_path() {
        // §6.4 — the ordinary case.
        assert_eq!(safe_redirect(Some("https://dash.example/flows")), "/flows");
        assert_eq!(
            safe_redirect(Some("https://dash.example/flows?hours=6")),
            "/flows?hours=6"
        );
        assert_eq!(safe_redirect(Some("/alarms")), "/alarms");
    }

    #[test]
    fn a_redirect_can_never_leave_the_site() {
        // §6.4 — THE test of this file. The `Referer` is client-supplied,
        // so every one of these is something an attacker can send.
        let hostile = [
            "https://evil.example/steal",
            "http://evil.example/steal",
            "//evil.example/steal",
            // The one a "starts with /" check lets through: some browsers
            // resolve this as a network-path reference.
            "/\\evil.example",
            "\\\\evil.example/steal",
            "https://evil.example",
            "//evil.example",
        ];

        for referer in hostile {
            let target = safe_redirect(Some(referer));

            assert!(
                target.starts_with('/'),
                "{referer:?} produced {target:?}, which is not a path"
            );
            assert!(
                !target.starts_with("//"),
                "{referer:?} produced {target:?}, a network-path reference"
            );
            assert!(
                !target.contains("evil.example"),
                "{referer:?} produced {target:?}, which still names the host"
            );
        }
    }

    #[test]
    fn a_missing_or_useless_referer_goes_to_the_overview() {
        assert_eq!(safe_redirect(None), "/");
        assert_eq!(safe_redirect(Some("")), "/");
        assert_eq!(safe_redirect(Some("https://evil.example")), "/");
        assert_eq!(safe_redirect(Some("not a url at all")), "/");
    }

    #[test]
    fn a_fragment_is_dropped() {
        // It never reaches a server, and carrying it into a `Location`
        // header is noise at best.
        assert_eq!(
            safe_redirect(Some("https://dash.example/flows#panel")),
            "/flows"
        );
    }

    #[test]
    fn the_query_survives_so_a_filtered_page_stays_filtered() {
        // Switching language on a filtered chart should not reset the
        // filter — that is the whole reason the referer is used at all.
        assert_eq!(
            safe_redirect(Some("https://dash.example/messages?workflow_id=phw-to-mpi")),
            "/messages?workflow_id=phw-to-mpi"
        );
    }
}
