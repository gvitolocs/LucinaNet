//! swgl picks a blend mode by hashing the GL enums (gl.cc, `hash_blend_key`), and its fields overlap. The Rust
//! rasterizer compares tuples instead (`blend_key`, proved by Verus). This walks every blend state GL allows and lists
//! where the two give different keys, so a difference from swgl is known, not discovered on a page.
// The pairs of the table are walked by index, as the pairs of keys are.
#![allow(clippy::needless_range_loop)]

use lucinanet_raster_rules::{blend_key, BLEND_KEYS_FROM_STATE, BLEND_TABLE, GL_FUNC_ADD};

/// Every blend factor `glBlendFunc` accepts (gl_defs.h).
const FACTORS: [u32; 19] = [
    0, 1, 0x0300, 0x0301, 0x0302, 0x0303, 0x0304, 0x0305, 0x0306, 0x0307, 0x0308, 0x8001, 0x8002, 0x8003, 0x8004,
    0x8589, 0x88F9, 0x88FA, 0x88FB,
];

/// Every equation `glBlendEquation` accepts in swgl (gl.cc asserts the rest away): `GL_FUNC_ADD`, `GL_MIN`, `GL_MAX` and
/// `GL_MULTIPLY_KHR..=GL_HSL_LUMINOSITY_KHR`.
fn equations() -> Vec<u32> {
    let mut e = vec![GL_FUNC_ADD, 0x8007, 0x8008];
    e.extend(0x9294..=0x92B0);
    e
}

/// `remap_blendfunc` (gl.cc): an alpha factor that is the alpha form of the rgb factor becomes the rgb factor.
fn remap(rgb: u32, a: u32) -> u32 {
    let pairs = [
        (0x0300, 0x0302), (0x0301, 0x0303), (0x0306, 0x0304), (0x0307, 0x0305), (0x8001, 0x8003), (0x8002, 0x8004),
        (0x8589, 0x88F9), (0x88FB, 0x88FA), (0x88F9, 0x8589), (0x88FA, 0x88FB),
    ];
    for (r, alpha) in pairs {
        if rgb == r && a == alpha {
            return r;
        }
    }
    a
}

fn hash(x: u32, y: u32, z: u32, w: u32) -> u32 {
    (x << 4) | y | (z << 24) | (w << 20)
}

/// swgl's key, `None` for its `default:` branch.
fn swgl_key(srgb: u32, drgb: u32, sa: u32, da: u32, equation: u32) -> Option<usize> {
    let mut h = hash(srgb, drgb, 0, 0);
    if srgb != sa || drgb != da {
        h |= hash(0, 0, sa, da);
    }
    if equation != GL_FUNC_ADD {
        h = hash(equation, 0, 0, 0);
    }
    (0..BLEND_KEYS_FROM_STATE).find(|&i| {
        let e = BLEND_TABLE[i];
        hash(e.0, e.1, e.2, e.3) == h
    })
}

#[test]
fn table_hashes_are_distinct_as_swgl_needs() {
    for i in 0..BLEND_KEYS_FROM_STATE {
        for j in i + 1..BLEND_KEYS_FROM_STATE {
            let (a, b) = (BLEND_TABLE[i], BLEND_TABLE[j]);
            assert_ne!(hash(a.0, a.1, a.2, a.3), hash(b.0, b.1, b.2, b.3), "entries {i} and {j}");
        }
    }
}

#[test]
fn hash_agrees_on_gl_enums() {
    let mut differ = Vec::new();
    let mut states = 0u64;
    for &equation in &equations() {
        // Factors do not matter once the equation is not GL_FUNC_ADD; one state stands for all.
        let factors: &[u32] = if equation == GL_FUNC_ADD { &FACTORS } else { &FACTORS[1..2] };
        for &srgb in factors {
            for &drgb in factors {
                for &sa0 in factors {
                    for &da0 in factors {
                        states += 1;
                        let (sa, da) = (remap(srgb, sa0), remap(drgb, da0));
                        let ours = blend_key(srgb, drgb, sa, da, equation);
                        let theirs = swgl_key(srgb, drgb, sa, da, equation);
                        if ours != theirs {
                            differ.push((srgb, drgb, sa, da, equation, ours, theirs));
                        }
                    }
                }
            }
        }
    }
    for d in differ.iter().take(40) {
        println!(
            "differ: srgb {:#x} drgb {:#x} sa {:#x} da {:#x} eq {:#x}: exact {:?}, swgl hash {:?}",
            d.0, d.1, d.2, d.3, d.4, d.5, d.6
        );
    }
    println!("{states} states, {} differ", differ.len());
    // Every difference must be swgl matching an entry the state is not (aliasing), never the other way round.
    for d in &differ {
        assert!(d.5.is_none() && d.6.is_some(), "exact matched where swgl did not: {d:x?}");
    }
    assert_eq!(differ.len(), KNOWN_ALIASES, "the number of aliased states changed; look at the list above");
}

/// States swgl's hash maps to a table entry they are not (counted by this test on 2026-11-04). The cause: `z << 24` and
/// `w << 20` overflow 32 bits for factors above 0xff (undefined behaviour in C++; it wraps on clang), and `x << 4 | y`
/// shares bits 4..15 between the two rgb factors. E.g. alpha factors `(GL_ONE_MINUS_SRC_COLOR, GL_ONE_MINUS_SRC_ALPHA)`
/// over rgb `(GL_ONE, GL_ONE)` read as entry 8, `(GL_ONE, GL_ONE, GL_ONE, GL_ONE_MINUS_SRC_ALPHA)`. The Rust rasterizer
/// blends these as unknown states (source-over, key 2). WebRender is not known to set any of them; a trace would show it.
const KNOWN_ALIASES: usize = 22;
