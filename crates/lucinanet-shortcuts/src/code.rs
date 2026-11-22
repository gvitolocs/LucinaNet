/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// The physical keys the shortcut tables name, as Linux evdev codes (`u16`), the same numbers `lucina_app::keys`,
// `lucinanet-ui::tabs` and `lucinanet-ui::browser` already use, so the wiring can pass a key press straight in.
// The values are the `KEY_*` of `linux/input-event-codes.h` (include/uapi/linux/input-event-codes.h, torvalds/linux
// master 2026-11-11); only the keys a browser shortcut can name are listed.

pub const ESC: u16 = 1;
pub const N1: u16 = 2;
pub const N2: u16 = 3;
pub const N3: u16 = 4;
pub const N4: u16 = 5;
pub const N5: u16 = 6;
pub const N6: u16 = 7;
pub const N7: u16 = 8;
pub const N8: u16 = 9;
pub const N9: u16 = 10;
pub const N0: u16 = 11;
pub const MINUS: u16 = 12;
pub const EQUAL: u16 = 13;
pub const BACKSPACE: u16 = 14;
pub const TAB: u16 = 15;
pub const Q: u16 = 16;
pub const W: u16 = 17;
pub const E: u16 = 18;
pub const R: u16 = 19;
pub const T: u16 = 20;
pub const Y: u16 = 21;
pub const U: u16 = 22;
pub const I: u16 = 23;
pub const O: u16 = 24;
pub const P: u16 = 25;
/// The `[` key, Firefox's `Ctrl+[` for Back.
pub const BRACKETLEFT: u16 = 26;
/// The `]` key, Firefox's `Ctrl+]` for Forward.
pub const BRACKETRIGHT: u16 = 27;
pub const ENTER: u16 = 28;
pub const A: u16 = 30;
pub const S: u16 = 31;
pub const D: u16 = 32;
pub const F: u16 = 33;
pub const G: u16 = 34;
pub const H: u16 = 35;
pub const J: u16 = 36;
pub const K: u16 = 37;
pub const L: u16 = 38;
pub const Z: u16 = 44;
pub const X: u16 = 45;
pub const C: u16 = 46;
pub const V: u16 = 47;
pub const B: u16 = 48;
pub const N: u16 = 49;
pub const M: u16 = 50;
pub const SPACE: u16 = 57;
pub const F1: u16 = 59;
pub const F2: u16 = 60;
pub const F3: u16 = 61;
pub const F4: u16 = 62;
pub const F5: u16 = 63;
pub const F6: u16 = 64;
pub const F7: u16 = 65;
pub const F8: u16 = 66;
pub const F9: u16 = 67;
pub const F10: u16 = 68;
pub const F11: u16 = 87;
pub const F12: u16 = 88;
pub const KP_7: u16 = 71;
pub const KP_8: u16 = 72;
pub const KP_9: u16 = 73;
pub const KP_MINUS: u16 = 74;
pub const KP_4: u16 = 75;
pub const KP_5: u16 = 76;
pub const KP_6: u16 = 77;
pub const KP_PLUS: u16 = 78;
pub const KP_1: u16 = 79;
pub const KP_2: u16 = 80;
pub const KP_3: u16 = 81;
pub const KP_0: u16 = 82;
pub const HOME: u16 = 102;
pub const UP: u16 = 103;
pub const PAGE_UP: u16 = 104;
pub const LEFT: u16 = 105;
pub const RIGHT: u16 = 106;
pub const END: u16 = 107;
pub const DOWN: u16 = 108;
pub const PAGE_DOWN: u16 = 109;
pub const DELETE: u16 = 111;

/// The name a menu shows for a physical key, the shape Firefox's and Chromium's own menus use
/// (`Ctrl+Shift+T`, `Alt+Left`, `F5`). `"<unknown>"` for a code no shortcut names, so a label never panics.
pub(crate) fn name(key: u16) -> &'static str {
    match key {
        ESC => "Esc",
        N1 => "1",
        N2 => "2",
        N3 => "3",
        N4 => "4",
        N5 => "5",
        N6 => "6",
        N7 => "7",
        N8 => "8",
        N9 => "9",
        N0 => "0",
        MINUS => "-",
        EQUAL => "=",
        BACKSPACE => "Backspace",
        TAB => "Tab",
        Q => "Q",
        W => "W",
        E => "E",
        R => "R",
        T => "T",
        Y => "Y",
        U => "U",
        I => "I",
        O => "O",
        P => "P",
        BRACKETLEFT => "[",
        BRACKETRIGHT => "]",
        ENTER => "Enter",
        A => "A",
        S => "S",
        D => "D",
        F => "F",
        G => "G",
        H => "H",
        J => "J",
        K => "K",
        L => "L",
        Z => "Z",
        X => "X",
        C => "C",
        V => "V",
        B => "B",
        N => "N",
        M => "M",
        SPACE => "Space",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        KP_0 => "KP 0",
        KP_1 => "KP 1",
        KP_2 => "KP 2",
        KP_3 => "KP 3",
        KP_4 => "KP 4",
        KP_5 => "KP 5",
        KP_6 => "KP 6",
        KP_7 => "KP 7",
        KP_8 => "KP 8",
        KP_9 => "KP 9",
        KP_MINUS => "KP -",
        KP_PLUS => "KP +",
        HOME => "Home",
        UP => "Up",
        PAGE_UP => "PageUp",
        LEFT => "Left",
        RIGHT => "Right",
        END => "End",
        DOWN => "Down",
        PAGE_DOWN => "PageDown",
        DELETE => "Delete",
        _ => "<unknown>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_known_and_distinct() {
        // Codes the tables use, so a binding never labels itself "<unknown>".
        let used = [
            T, W, E, R, K, J, L, D, F, G, H, O, P, S, U, Y, B, N, I, A, C, M, Q, V, X, Z, N1, N2, N3, N4, N5, N6, N7,
            N8, N9, N0, MINUS, EQUAL, TAB, ESC, DELETE, F3, F4, F5, F11, F12, HOME, PAGE_UP, PAGE_DOWN, LEFT, RIGHT,
            BRACKETLEFT, BRACKETRIGHT, KP_PLUS, KP_MINUS, KP_0, KP_1, KP_2, KP_3, KP_4, KP_5, KP_6, KP_7, KP_8, KP_9,
        ];
        for key in used {
            assert_ne!(name(key), "<unknown>", "code {key}");
        }
        // Two keys sharing a name would make two shortcuts look the same in a menu.
        for a in used {
            for b in used {
                if a != b {
                    assert_ne!(name(a), name(b), "{a} vs {b}");
                }
            }
        }
        assert_eq!(name(4095), "<unknown>");
    }

    /// Every constant against the number `linux/input-event-codes.h` gives its `KEY_*` (checked against the header
    /// on a Linux system, /usr/include/linux/input-event-codes.h). The values are written out here on purpose:
    /// reading the header at test time would make the test depend on the machine.
    #[test]
    fn every_code_is_the_kernels() {
        let kernel: &[(&str, u16, u16)] = &[
            ("KEY_ESC", ESC, 1),
            ("KEY_1", N1, 2),
            ("KEY_2", N2, 3),
            ("KEY_3", N3, 4),
            ("KEY_4", N4, 5),
            ("KEY_5", N5, 6),
            ("KEY_6", N6, 7),
            ("KEY_7", N7, 8),
            ("KEY_8", N8, 9),
            ("KEY_9", N9, 10),
            ("KEY_0", N0, 11),
            ("KEY_MINUS", MINUS, 12),
            ("KEY_EQUAL", EQUAL, 13),
            ("KEY_BACKSPACE", BACKSPACE, 14),
            ("KEY_TAB", TAB, 15),
            ("KEY_Q", Q, 16),
            ("KEY_W", W, 17),
            ("KEY_E", E, 18),
            ("KEY_R", R, 19),
            ("KEY_T", T, 20),
            ("KEY_Y", Y, 21),
            ("KEY_U", U, 22),
            ("KEY_I", I, 23),
            ("KEY_O", O, 24),
            ("KEY_P", P, 25),
            ("KEY_LEFTBRACE", BRACKETLEFT, 26),
            ("KEY_RIGHTBRACE", BRACKETRIGHT, 27),
            ("KEY_ENTER", ENTER, 28),
            ("KEY_A", A, 30),
            ("KEY_S", S, 31),
            ("KEY_D", D, 32),
            ("KEY_F", F, 33),
            ("KEY_G", G, 34),
            ("KEY_H", H, 35),
            ("KEY_J", J, 36),
            ("KEY_K", K, 37),
            ("KEY_L", L, 38),
            ("KEY_Z", Z, 44),
            ("KEY_X", X, 45),
            ("KEY_C", C, 46),
            ("KEY_V", V, 47),
            ("KEY_B", B, 48),
            ("KEY_N", N, 49),
            ("KEY_M", M, 50),
            ("KEY_SPACE", SPACE, 57),
            ("KEY_F1", F1, 59),
            ("KEY_F2", F2, 60),
            ("KEY_F3", F3, 61),
            ("KEY_F4", F4, 62),
            ("KEY_F5", F5, 63),
            ("KEY_F6", F6, 64),
            ("KEY_F7", F7, 65),
            ("KEY_F8", F8, 66),
            ("KEY_F9", F9, 67),
            ("KEY_F10", F10, 68),
            ("KEY_F11", F11, 87),
            ("KEY_F12", F12, 88),
            ("KEY_KP7", KP_7, 71),
            ("KEY_KP8", KP_8, 72),
            ("KEY_KP9", KP_9, 73),
            ("KEY_KPMINUS", KP_MINUS, 74),
            ("KEY_KP4", KP_4, 75),
            ("KEY_KP5", KP_5, 76),
            ("KEY_KP6", KP_6, 77),
            ("KEY_KPPLUS", KP_PLUS, 78),
            ("KEY_KP1", KP_1, 79),
            ("KEY_KP2", KP_2, 80),
            ("KEY_KP3", KP_3, 81),
            ("KEY_KP0", KP_0, 82),
            ("KEY_HOME", HOME, 102),
            ("KEY_UP", UP, 103),
            ("KEY_PAGEUP", PAGE_UP, 104),
            ("KEY_LEFT", LEFT, 105),
            ("KEY_RIGHT", RIGHT, 106),
            ("KEY_END", END, 107),
            ("KEY_DOWN", DOWN, 108),
            ("KEY_PAGEDOWN", PAGE_DOWN, 109),
            ("KEY_DELETE", DELETE, 111),
        ];
        for &(kernel_name, ours, theirs) in kernel {
            assert_eq!(ours, theirs, "{kernel_name}");
        }
        // No two keys share a number.
        for (i, a) in kernel.iter().enumerate() {
            for b in &kernel[i + 1..] {
                assert_ne!(a.2, b.2, "{} and {}", a.0, b.0);
            }
        }
    }
}
