# Porting record: lucinanet-permissions

The originals are the truth for behaviour; this crate copies behaviour and constants, not structure.
Pinned revisions (files fetched into a scratch directory outside this repo):
Firefox `33abb900fc1646fef2e5ffed2d6b22e806ae10c1` ("@33abb90"), Chromium `e9a46735de970a8e31217f2233efa3a47a1c337d` ("@e9a4673"). Every path below returned 200 at the pinned sha.

## Sources

| File | Licence | Decision | What was taken |
| --- | --- | --- | --- |
| Firefox `netwerk/base/nsIPermissionManager.idl` | MPL-2.0 | adapt | The action values (unknown/allow/deny/prompt) map to `Setting`; `EXPIRE_NEVER/SESSION/TIME` map to `Forever/Session/Until`. `EXPIRE_POLICY` and `EXPIRE_SESSION_TAB` not ported (no enterprise policy; tab scope is Chromium's `OneTime`). |
| Firefox `extensions/permissions/PermissionManager.cpp` | MPL-2.0 | reference | Expiry semantics: a time-expired entry is treated as absent. Storage (SQLite, idle maintenance, unused-permission expiry) not ported. |
| Firefox `browser/modules/SitePermissions.sys.mjs` | MPL-2.0 | adopt (defaults), reference (rest) | `getDefault()`: no `permissions.default.<id>` pref means UNKNOWN (prompt); `popup` default BLOCK; the feature list and which ones have a prompt. Temporary per-tab permissions correspond to `OneTime`. |
| Firefox `modules/libpref/init/StaticPrefList.yaml` | MPL-2.0 | adopt (values) | `permissions.default.screen-wake-lock` = 1 (line 17959), `media.autoplay.default` = 1 blocked (line 12581), `dom.gamepad.enabled` = true (line 3425), `permissions.default.image` is out of scope. |
| Firefox `dom/webidl/Navigator.webidl` | MPL-2.0 | reference | `getGamepads()` has no `[SecureContext]`, so Gamepad does not need a secure context. |
| Chromium `components/permissions/permission_decision_auto_blocker.cc/.h` | BSD-3-Clause | adapt | `kDefaultDismissalsBeforeBlock = 3`, `kDefaultIgnoresBeforeBlock = 4`, `kDefaultEmbargoDays = 7`; counters are per (origin, permission type), persisted in the profile, not reset when an embargo starts, cleared by `RemoveEmbargoAndResetCounts` (an explicit allow or deny); the embargo ends `start + 7 days` (`IsUnderEmbargo`: `now < start + offset`); dismissal and ignore embargoes are separate keys, "embargoed" means either is in force. Quiet-UI variants, federated-identity, sub-app and display embargoes: rejected (out of scope per the spec). |
| Chromium `components/permissions/permission_actions_history.cc` | BSD-3-Clause | reject | Statistics for metrics and quiet-UI triggers; nothing needed. |
| Chromium `components/permissions/permission_util.cc` | BSD-3-Clause | reference | Which content settings are permissions. |
| Chromium `components/content_settings/core/common/content_settings.h`, `content_settings_types.h` | BSD-3-Clause | reference | `ALLOW/BLOCK/ASK` (plus `DEFAULT`, our "no entry"). The types enum lives in a mojom file not read. |
| Chromium `components/content_settings/core/browser/content_settings_registry.cc` | BSD-3-Clause | adopt (defaults) | Default per type: popups BLOCK (line 138), geolocation ASK (148), notifications ASK (158), midi-sysex ASK (209), durable-storage ASK (235), background-sync ALLOW (246), autoplay ALLOW (256), bluetooth-guard ASK (328), sensors ALLOW (341), clipboard ASK (352), media-stream mic/camera ASK. Not in the spec's list but where the defaults live. |
| Chromium `components/content_settings/core/browser/host_content_settings_map.cc` | BSD-3-Clause | reference | One-time grants come from a separate provider and end when the origin's tabs close. |

Chromium-derived numbers and ASK/ALLOW defaults are covered by `NOTICE` (BSD-3-Clause text from Chromium's `LICENSE`).

## Defaults table (`Feature::default_setting`)

| Feature | Default | Source |
| --- | --- | --- |
| Geolocation, Notifications, Push, Midi, Camera, Microphone, Speaker, Bluetooth, ClipboardRead | Ask | Firefox: unset pref is UNKNOWN (prompt); Chromium registry ASK lines above |
| PersistentStorage | Ask | Firefox unset pref; Chromium registry line 235 is also ASK (the spec expected Chromium to auto-grant by engagement: that is a separate heuristic in the permission context, not the registry default; Firefox's value is used) |
| DeviceInfo | Allow | Chromium sensors ALLOW (341); Firefox has no permission for sensors (`device.sensors.enabled` true) |
| BackgroundSync | Allow | Chromium registry line 246; Firefox has no such permission |
| ScreenWakeLock | Allow | Firefox `permissions.default.screen-wake-lock` = 1 |
| Gamepad | Allow | Firefox `dom.gamepad.enabled` = true, no permission; Chromium has no prompt |
| Popups | Block | Firefox `popup.getDefault()`; Chromium line 138 |
| Autoplay | Block | Firefox `media.autoplay.default` = 1 (blocked); Chromium ALLOWs. Firefox followed. |

`needs_secure_context`: true for Geolocation, Notifications, Push, Midi, Camera, Microphone, Bluetooth, ClipboardRead, DeviceInfo, ScreenWakeLock, PersistentStorage (the spec's list). False for Gamepad (Navigator.webidl, above), Speaker, BackgroundSync, Popups, Autoplay.

## Deviations from the spec (the original wins)

1. **Counters are not reset when an embargo starts.** The spec says "reset the counter"; Chromium's `RecordDismissAndEmbargo`/`RecordIgnoreAndEmbargo` only increment and compare `>=`, and counters are cleared only by `RemoveEmbargoAndResetCounts` (an explicit allow or deny). So after an embargo ends, one more dismissal embargoes again. Tested in `embargo_ends_after_seven_days`.
2. **Ignore counters are persistent and survive `session_end`.** The spec says `session_end` drops the in-memory ignore counters; in Chromium both counters live in the profile's `PERMISSION_AUTOBLOCKER_DATA`. `session_end` drops only Session and OneTime entries.
3. **File format adds `ignores`** to each `embargo` record and makes `until` optional there (a record may hold only counts). One `until` per record is the later of the dismissal and ignore embargo ends, equivalent to Chromium's two keys because both last seven days. A missing `ignores` reads as 0.
4. **`reset_site` and `clear` also forget counters and embargoes** (Chromium's clear-browsing-data path removes the autoblocker data with the site's settings). `remove` keeps them.
5. **Time unit.** Expiry is microseconds as the spec says; Firefox's `EXPIRE_TIME` is milliseconds.
6. **Feature names.** Speaker is `speaker-selection` (the W3C name) and DeviceInfo `device-info` (no W3C name). Sort order of features is enum declaration order, not alphabetical by name.
7. **`origin_of` accepts only http and https.** Other tuple origins (ws, ftp) hold no permissions here.
8. **Secure origins**: https, plus http on `localhost`, `*.localhost`, `127.0.0.1`, `[::1]`. `*.localhost` goes beyond the spec's list (both browsers treat it as potentially trustworthy).
9. **Unknown setting in a file** (including `"ask"`, which is never stored) is a `Syntax` error, not silently dropped.
10. The spec's index note says `lucinanet-history` does not exist at the base; it does in this tree. Not used here.

## Not ported

Quiet-UI embargo variants, federated-identity and sub-app special cases, permission-actions history statistics, unused-permission expiry (`permissions.expireUnused*`), per-user-context and private-browsing isolation, enterprise policy, third-party (double-keyed) permissions.
