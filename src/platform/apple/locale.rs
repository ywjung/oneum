//! System language.
use objc2_foundation::NSLocale;

/// True when the user's first preferred language is Korean.
pub fn prefers_korean() -> bool {
    NSLocale::preferredLanguages()
        .iter()
        .next()
        .is_some_and(|lang| lang.to_string().starts_with("ko"))
}
