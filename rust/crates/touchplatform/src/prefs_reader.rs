//! The trackpad's gesture settings, read from their defaults domain.
//!
//! The domain is synchronized first, because cfprefsd caches values per process and only writes the
//! plist to disk lazily.

use std::collections::HashMap;

use objc2_core_foundation::{CFBoolean, CFNumber, CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFString};
use touchcore::prefs::{PrefValue, TrackpadPreferences};

/// Reads the trackpad's gesture settings.
pub struct TrackpadPreferencesReader;

impl TrackpadPreferencesReader {
    /// The defaults domain System Settings stores the built-in trackpad's settings in.
    const DOMAIN: &'static str = "com.apple.AppleMultitouchTrackpad";

    /// The current settings, read from the domain. A setting that is not stored keeps whatever
    /// [`TrackpadPreferences::from_values`] defaults it to.
    pub fn read() -> TrackpadPreferences {
        let domain = CFString::from_str(Self::DOMAIN);
        CFPreferencesAppSynchronize(&domain);
        let mut values = HashMap::new();
        for key in TrackpadPreferences::KEYS {
            let Some(value) = CFPreferencesCopyAppValue(&CFString::from_str(key), &domain) else { continue };
            // A boolean is its own CF type; every other number reads as an int, truncated the way
            // Swift's `number.intValue` truncates.
            if let Some(flag) = value.downcast_ref::<CFBoolean>().map(|flag| flag.as_bool()) {
                values.insert(key.to_string(), PrefValue::Bool(flag));
            } else if let Some(number) = value.downcast_ref::<CFNumber>().and_then(|number| number.as_f64()) {
                values.insert(key.to_string(), PrefValue::Int(number as i64));
            }
        }
        TrackpadPreferences::from_values(&values)
    }
}
