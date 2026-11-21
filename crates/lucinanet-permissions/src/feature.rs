/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The features a site can ask for, with their stable names and the defaults of the originals.

/// What a feature does when nothing is stored for the origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Setting {
    Allow,
    Block,
    Ask,
}

/// Matches Servo's `PermissionFeature` (components/shared/embedder/lib.rs: Geolocation, Notifications, Push, Midi,
/// Camera, Microphone, Speaker, DeviceInfo, BackgroundSync, Bluetooth, PersistentStorage, ScreenWakeLock, Gamepad)
/// plus three the browsers have and Servo may ask for later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    Geolocation,
    Notifications,
    Push,
    Midi,
    Camera,
    Microphone,
    Speaker,
    DeviceInfo,
    BackgroundSync,
    Bluetooth,
    PersistentStorage,
    ScreenWakeLock,
    Gamepad,
    ClipboardRead,
    Popups,
    Autoplay,
}

impl Feature {
    /// Every variant, in declaration order (which is also the sort order).
    pub const ALL: [Feature; 16] = [
        Feature::Geolocation,
        Feature::Notifications,
        Feature::Push,
        Feature::Midi,
        Feature::Camera,
        Feature::Microphone,
        Feature::Speaker,
        Feature::DeviceInfo,
        Feature::BackgroundSync,
        Feature::Bluetooth,
        Feature::PersistentStorage,
        Feature::ScreenWakeLock,
        Feature::Gamepad,
        Feature::ClipboardRead,
        Feature::Popups,
        Feature::Autoplay,
    ];

    /// The stable name used in the file format: the W3C Permissions name where one exists.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Geolocation => "geolocation",
            Feature::Notifications => "notifications",
            Feature::Push => "push",
            Feature::Midi => "midi",
            Feature::Camera => "camera",
            Feature::Microphone => "microphone",
            Feature::Speaker => "speaker-selection",
            Feature::DeviceInfo => "device-info",
            Feature::BackgroundSync => "background-sync",
            Feature::Bluetooth => "bluetooth",
            Feature::PersistentStorage => "persistent-storage",
            Feature::ScreenWakeLock => "screen-wake-lock",
            Feature::Gamepad => "gamepad",
            Feature::ClipboardRead => "clipboard-read",
            Feature::Popups => "popups",
            Feature::Autoplay => "autoplay",
        }
    }

    /// The feature with this stable name, or `None` (a newer file may carry features this build lacks).
    pub fn from_name(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }

    /// What happens with no stored answer. Firefox's choice wins where the two browsers differ (see PORTING.md).
    pub fn default_setting(self) -> Setting {
        match self {
            // Firefox SitePermissions.sys.mjs @33abb90 getDefault(): no `permissions.default.<id>` pref means UNKNOWN,
            // i.e. a prompt; Chromium content_settings_registry.cc @e9a4673 registers geolocation, notifications,
            // midi-sysex, durable-storage, bluetooth-guard and clipboard as CONTENT_SETTING_ASK.
            Feature::Geolocation
            | Feature::Notifications
            | Feature::Push
            | Feature::Midi
            | Feature::Camera
            | Feature::Microphone
            | Feature::Speaker
            | Feature::Bluetooth
            | Feature::PersistentStorage
            | Feature::ClipboardRead => Setting::Ask,
            // Chromium content_settings_registry.cc @e9a4673: sensors and background-sync default to ALLOW.
            // Firefox StaticPrefList.yaml @33abb90: `permissions.default.screen-wake-lock` = 1 (accept),
            // `dom.gamepad.enabled` = true with no permission gate.
            Feature::DeviceInfo
            | Feature::BackgroundSync
            | Feature::ScreenWakeLock
            | Feature::Gamepad => Setting::Allow,
            // Firefox SitePermissions.sys.mjs @33abb90: `popup.getDefault()` is BLOCK; Chromium registry POPUPS is BLOCK.
            // Firefox StaticPrefList.yaml @33abb90: `media.autoplay.default` = 1 (blocked); Chromium would ALLOW.
            Feature::Popups | Feature::Autoplay => Setting::Block,
        }
    }

    /// Whether the web platform exposes the feature only to secure contexts, so `decide` blocks it on an insecure
    /// origin before looking at anything stored.
    pub fn needs_secure_context(self) -> bool {
        match self {
            Feature::Geolocation
            | Feature::Notifications
            | Feature::Push
            | Feature::Midi
            | Feature::Camera
            | Feature::Microphone
            | Feature::Bluetooth
            | Feature::ClipboardRead
            | Feature::DeviceInfo
            | Feature::ScreenWakeLock
            | Feature::PersistentStorage => true,
            // Firefox Navigator.webidl @33abb90: getGamepads() carries no [SecureContext].
            Feature::Gamepad => false,
            Feature::Speaker | Feature::BackgroundSync | Feature::Popups | Feature::Autoplay => false,
        }
    }

    /// Camera, microphone and geolocation: a prompt is never auto-granted, one-time grants are allowed.
    pub fn is_sensitive(self) -> bool {
        matches!(self, Feature::Camera | Feature::Microphone | Feature::Geolocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn feature_names_roundtrip_and_are_unique() {
        let mut seen = HashSet::new();
        for f in Feature::ALL {
            assert_eq!(Feature::from_name(f.name()), Some(f));
            assert!(seen.insert(f.name()), "duplicate name {}", f.name());
        }
        assert_eq!(Feature::from_name("nope"), None);
        assert_eq!(Feature::from_name(""), None);
        assert_eq!(Feature::from_name("Camera"), None);
    }

    #[test]
    fn feature_all_has_every_variant() {
        // An exhaustive match fails to compile when a variant is added without being listed here.
        let index = |f: Feature| match f {
            Feature::Geolocation => 0,
            Feature::Notifications => 1,
            Feature::Push => 2,
            Feature::Midi => 3,
            Feature::Camera => 4,
            Feature::Microphone => 5,
            Feature::Speaker => 6,
            Feature::DeviceInfo => 7,
            Feature::BackgroundSync => 8,
            Feature::Bluetooth => 9,
            Feature::PersistentStorage => 10,
            Feature::ScreenWakeLock => 11,
            Feature::Gamepad => 12,
            Feature::ClipboardRead => 13,
            Feature::Popups => 14,
            Feature::Autoplay => 15,
        };
        for (i, f) in Feature::ALL.into_iter().enumerate() {
            assert_eq!(index(f), i);
        }
        let mut sorted = Feature::ALL;
        sorted.sort();
        assert_eq!(sorted, Feature::ALL);
    }

    #[test]
    fn servo_features_all_present() {
        // The 13 names of Servo's PermissionFeature enum, as W3C names where one exists.
        let servo = [
            "geolocation",
            "notifications",
            "push",
            "midi",
            "camera",
            "microphone",
            "speaker-selection",
            "device-info",
            "background-sync",
            "bluetooth",
            "persistent-storage",
            "screen-wake-lock",
            "gamepad",
        ];
        for n in servo {
            assert!(Feature::from_name(n).is_some(), "missing {n}");
        }
        assert_eq!(servo.len(), 13);
    }

    #[test]
    fn defaults_table_matches_sources() {
        use Setting::*;
        // Firefox SitePermissions.sys.mjs getDefault(): unset pref -> UNKNOWN -> prompt; Chromium registry ASK lines.
        assert_eq!(Feature::Geolocation.default_setting(), Ask);
        assert_eq!(Feature::Notifications.default_setting(), Ask);
        assert_eq!(Feature::Push.default_setting(), Ask);
        assert_eq!(Feature::Midi.default_setting(), Ask);
        assert_eq!(Feature::Camera.default_setting(), Ask);
        assert_eq!(Feature::Microphone.default_setting(), Ask);
        assert_eq!(Feature::Speaker.default_setting(), Ask);
        assert_eq!(Feature::Bluetooth.default_setting(), Ask);
        assert_eq!(Feature::PersistentStorage.default_setting(), Ask);
        assert_eq!(Feature::ClipboardRead.default_setting(), Ask);
        // Chromium content_settings_registry.cc: sensors ALLOW (line 341), background-sync ALLOW (line 246).
        assert_eq!(Feature::DeviceInfo.default_setting(), Allow);
        assert_eq!(Feature::BackgroundSync.default_setting(), Allow);
        // Firefox StaticPrefList.yaml: permissions.default.screen-wake-lock value 1 (line 17959); dom.gamepad.enabled true.
        assert_eq!(Feature::ScreenWakeLock.default_setting(), Allow);
        assert_eq!(Feature::Gamepad.default_setting(), Allow);
        // Firefox SitePermissions.sys.mjs popup.getDefault() BLOCK; media.autoplay.default = 1 (blocked).
        assert_eq!(Feature::Popups.default_setting(), Block);
        assert_eq!(Feature::Autoplay.default_setting(), Block);
    }

    #[test]
    fn sensitive_features_are_camera_microphone_geolocation() {
        let sensitive: Vec<_> = Feature::ALL.into_iter().filter(|f| f.is_sensitive()).collect();
        assert_eq!(sensitive, vec![Feature::Geolocation, Feature::Camera, Feature::Microphone]);
    }
}
