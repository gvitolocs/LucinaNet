# Harness doors

A few ways to start a request or run script from outside the browser exist only for tests and measurements: the environment
names a user script or a start script, or the command line asks for an unattended page load. They are the **doors**. A door a
normal launch can open is a hole (a shell rc, `launchctl setenv` or a wrapper could hand the engine a script that fetches
something nobody asked for), so they are **compiled in only with the cargo feature `harness`**. The feature is off
by default and off in every build that can reach a user; a normal build does not contain the code, and the names below are not
strings in its binary (`tests/harness-gate/check-release-binary.sh` proves that from the built file).

| Door | Where | What it did |
|---|---|---|
| `LUCINANET_USER_JS=<file>` | `crates/lucinanet-servo/src/doors.rs`, read in `engine.rs` | a user script on every page, before the page's own |
| `LUCINANET_USER_CSS=<file>` | same | a user style sheet on every page |
| `LUCINANET_PREFS=name=value,...` | same | any engine preference by name |
| `LUCINANET_AX=<socket path>` | same | where the accessibility socket opens off the device |
| `LUCINANET_SCRIPT=<file>` | `crates/lucinanet-ui/src/lib.rs` (`script_door`) | replays typed input, loads and clicks from a file (non-desktop `run()` only) |
| `--render <url> <out.png> [WxH] [dump.json]` | `crates/lucinanet-ui/src/lib.rs` (`parse_mode`, `render`) | loads a page without a window and writes a picture |
| `LUCINANET_LOADS=<file>`, `LUCINANET_RENDER_HOLD=<s>` | inside `render()` | a scripted session, and N more seconds of engine; they go with `--render` |
| `--netprobe`, `--dnsprobe`, `--dnsparallel`, `--asyncprobe`, `--udpdns`, `--udpconnect`, `--libcdns`, `--pollgap` | `crates/lucinanet-servo/src/probes.rs` | raw DNS and connect diagnostics, no browser |

What stays in every build, on purpose: the embedded compat shims (`COMPAT_JS`, switched off only by `LUCINANET_COMPAT_SHIMS=0`), the
allow-listed `LUCINANET_CSS_PREFS`, the debug and trace variables that start no request (`LUCINANET_CONSOLE`, `_TIMING`,
`_MEDIA_SINK`, `_PROFILE_DIR`, `_MEDIA_TRACE` and the like), and the accessibility socket itself: on the device the socket opens
because `/run/lucina` exists (`engine.rs` `ax_wanted`), which is a product feature the assistant uses. Only the
environment-chosen path went.

## What a normal build does with them

* The variables are not read; `script_door`, `extra_prefs`, `user_files` and `ax_socket_override` are constant `None`/empty.
* `--render` and every other `--option` except `--desktop` and `--version` is refused: `lucinanet: unknown option --render`,
  exit status 2, nothing started. (Before, such an argument was taken for an address and became a search request.)
* `lucinanet --version` prints `lucinanet <version>`.

## What a harness build says about itself

`lucinanet --version` prints `lucinanet <version> harness build (LUCINANET-HARNESS-BUILD: ...)` and the window title is
`LucinaNet (harness)`. The marker `LUCINANET-HARNESS-BUILD` is in no other build, so a script can tell from the file.
`tools/release/package-macos.sh` and the release job (`.github/workflows/release.yml`, through `tools/release/harness-guard.sh`) refuse a
binary that has it and a cargo feature list that names `harness`; the packager says so differently when the destination is an installed
app path. A measurement build is never installed over the maintainer's app.

## Building and using one

```sh
tools/build-harness.sh            # prints the command and where the binary lands
tools/build-harness.sh --run      # builds it (cargo build --profile probe -p lucinanet-servo --features desktop,media,harness) and checks it
cargo build-harness               # the same command as a cargo alias (.cargo/config.toml)
```

The scripts that need the doors and how each finds its binary are in `tests/acceptance/README.md`. The shared helper
`require_harness_binary` (`tests/acceptance/lib.sh`) stops a run on a binary that is not a harness build instead of letting it
measure nothing.

## The gates that keep it so

* `cargo test -p lucinanet-ui --lib door_tests` and `cargo test -p lucinanet-servo doors` (once without and once with
  `--features harness`): the door does nothing without the feature, and works as before with it.
* `tests/harness-gate/check-release-binary.sh <binary>` on the built release binary, and `--harness <binary>` as the positive control
  on a harness build; `tests/harness-gate/test-check-release-binary.sh` tests the checker on stubs.
* `tests/zero-network/run.sh planted`: the shipped binary is started with the doors' variables planted at files that would reach a
  local listener, and `--render` as an argument; nothing may connect.
* The launcher (`packaging/macos/launcher.sh`) removes every `LUCINA*` and `LUCINANET_*`
  variable of the caller before it sets its own; `packaging/macos/tests/test-package.sh` plants them and checks the engine sees only
  `LUCINANET_CONSOLE`.

Every door above is closed by one of these: the `LUCINANET_USER_JS`, `LUCINANET_USER_CSS`, `LUCINANET_PREFS`,
`LUCINANET_AX` and `LUCINANET_SCRIPT` doors, `--render` with `LUCINANET_LOADS` and `LUCINANET_RENDER_HOLD`, the probe subcommands,
the zero-network gate measuring the real launcher, and the dead `LUCINANET_JS_MISSING_LOG` export (removed).
