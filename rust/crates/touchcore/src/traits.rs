//! Facts about an app that change how touches must be delivered to it.

/// The set of browser and app shells that are Chromium underneath.
const CHROMIUM_MARKERS: [&str; 7] = ["Electron", "Chromium", "Chrome", "Edge", "Brave", "Vivaldi", "Opera"];

/// App traits, looked up by the platform crate from bundle and process information.
pub struct AppTraits;

impl AppTraits {
    /// Chromium and Electron apps ship a "<Name> Framework.framework" in Contents/Frameworks;
    /// their windows ignore synthesized events posted straight to them, so the caller warps instead.
    pub fn is_chromium_framework(name: &str) -> bool {
        name.ends_with(" Framework.framework") && CHROMIUM_MARKERS.iter().any(|marker| name.contains(marker))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_chromium_and_electron_framework_bundles() {
        for name in [
            "Electron Framework.framework",
            "Chromium Embedded Framework.framework",
            "Google Chrome Framework.framework",
            "Microsoft Edge Framework.framework",
            "Brave Browser Framework.framework",
        ] {
            assert!(AppTraits::is_chromium_framework(name), "{name}");
        }
    }

    #[test]
    fn ignores_ordinary_frameworks() {
        for name in ["Sparkle.framework", "Squirrel.framework", "Mantle.framework", "Chrome.txt"] {
            assert!(!AppTraits::is_chromium_framework(name), "{name}");
        }
    }
}
