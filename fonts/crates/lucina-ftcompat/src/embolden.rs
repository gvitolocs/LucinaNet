// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from FreeType 2.13.2 (FreeType License, see LICENSES/FTL.TXT).

//! FreeType 2.13.2's outline emboldening (`ftoutln.c`, `ftcalc.c`), without FreeType. Each function names the
//! FreeType function it reproduces; the source of truth is the FreeType tree bundled in `freetype-sys` 0.23.0.
//!
//! Points are 26.6 (`FT_Pos`), `contour_ends[i]` is the index of the last point of contour `i`, like
//! `FT_Outline.contours`. All arithmetic wraps, as the C's `ADD_LONG`/`MUL_LONG` and unsigned temporaries do.

use crate::{mul_div, mul_fix};

/// `FT_Orientation`: `FT_ORIENTATION_TRUETYPE` (0), `FT_ORIENTATION_POSTSCRIPT` (1), `FT_ORIENTATION_NONE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    TrueType,
    PostScript,
    None,
}

/// `FT_MSB` (`ftcalc.c`): the index of the highest set bit of a 32-bit value (0 for 0).
fn msb(z: u32) -> i32 {
    if z == 0 {
        0
    } else {
        31 - z.leading_zeros() as i32
    }
}

/// `FT_Vector_NormLen` (`ftcalc.c`): sets `v` to the 16.16 unit vector in its direction and returns the length.
/// The intermediate `FT_UInt32`/`FT_Int32` arithmetic wraps and truncates as in C.
pub fn vector_norm_len(v: &mut (i64, i64)) -> i64 {
    let x_ = v.0 as i32;
    let y_ = v.1 as i32;
    let (mut x, sx) = if x_ < 0 {
        (x_.wrapping_neg() as u32, -1i64)
    } else {
        (x_ as u32, 1i64)
    };
    let (mut y, sy) = if y_ < 0 {
        (y_.wrapping_neg() as u32, -1i64)
    } else {
        (y_ as u32, 1i64)
    };

    /* trivial cases */
    if x == 0 {
        if y > 0 {
            v.1 = sy * 0x10000;
        }
        return y as i64;
    } else if y == 0 {
        if x > 0 {
            v.0 = sx * 0x10000;
        }
        return x as i64;
    }

    /* estimate length and prenormalize so the new approximate length is between 2/3 and 4/3 */
    let mut l = if x > y {
        x.wrapping_add(y >> 1)
    } else {
        y.wrapping_add(x >> 1)
    };

    let mut shift = 31 - msb(l);
    shift -= 15 + i32::from(l >= (0xAAAA_AAAAu32 >> shift));

    if shift > 0 {
        x = x.wrapping_shl(shift as u32);
        y = y.wrapping_shl(shift as u32);
        l = if x > y {
            x.wrapping_add(y >> 1)
        } else {
            y.wrapping_add(x >> 1)
        };
    } else {
        x >>= -shift;
        y >>= -shift;
        l >>= -shift;
    }

    /* lower linear approximation for reciprocal length minus one */
    let mut b = 0x10000i32.wrapping_sub(l as i32);

    let x_ = x as i32;
    let y_ = y as i32;

    /* Newton's iterations */
    let (uu, vv) = loop {
        let u = x_.wrapping_add(x_.wrapping_mul(b) >> 16) as u32;
        let v = y_.wrapping_add(y_.wrapping_mul(b) >> 16) as u32;

        /* normalized squared length in the parentheses approaches 2^32 */
        let mut z =
            (u.wrapping_mul(u).wrapping_add(v.wrapping_mul(v)) as i32).wrapping_neg() / 0x200;
        z = z.wrapping_mul((0x10000 + b) >> 8) / 0x10000;

        b = b.wrapping_add(z);
        if z <= 0 {
            break (u, v);
        }
    };

    v.0 = if sx < 0 { -(uu as i64) } else { uu as i64 };
    v.1 = if sy < 0 { -(vv as i64) } else { vv as i64 };

    /* conversion to signed recovers from likely wrap around in the prenormalized length */
    let mut l = 0x10000i32
        .wrapping_add((uu.wrapping_mul(x).wrapping_add(vv.wrapping_mul(y)) as i32) / 0x10000)
        as u32;
    if shift > 0 {
        l = (l.wrapping_add(1u32 << (shift - 1))) >> shift;
    } else {
        l = l.wrapping_shl((-shift) as u32);
    }

    l as i64
}

/// `FT_Outline_Get_Orientation` (`ftoutln.c`): the winding orientation of the outline, `None` for collapsed,
/// zero-area or oversized outlines. An empty outline is `TrueType`, as in C.
pub fn outline_orientation(points: &[(i64, i64)], contour_ends: &[usize]) -> Orientation {
    if points.is_empty() {
        return Orientation::TrueType;
    }

    /* FT_Outline_Get_CBox */
    let mut x_min = points[0].0;
    let mut y_min = points[0].1;
    let mut x_max = x_min;
    let mut y_max = y_min;
    for p in &points[1..] {
        x_min = x_min.min(p.0);
        x_max = x_max.max(p.0);
        y_min = y_min.min(p.1);
        y_max = y_max.max(p.1);
    }

    /* handle collapsed outlines to avoid undefined FT_MSB */
    if x_min == x_max || y_min == y_max {
        return Orientation::None;
    }

    /* reject values large outlines */
    if x_min < -0x100_0000 || y_min < -0x100_0000 || x_max > 0x100_0000 || y_max > 0x100_0000 {
        return Orientation::None;
    }

    let xshift = (msb((x_max.abs() | x_min.abs()) as u32) - 14).max(0);
    let yshift = (msb((y_max - y_min) as u32) - 14).max(0);

    let mut area: i64 = 0;
    let mut last: i64 = -1;
    for &ce in contour_ends {
        let first = last + 1;
        last = ce as i64;

        let mut prev = (
            points[last as usize].0 >> xshift,
            points[last as usize].1 >> yshift,
        );
        let mut n = first;
        while n <= last {
            let cur = (
                points[n as usize].0 >> xshift,
                points[n as usize].1 >> yshift,
            );
            area = area.wrapping_add(
                (cur.1.wrapping_sub(prev.1)).wrapping_mul(cur.0.wrapping_add(prev.0)),
            );
            prev = cur;
            n += 1;
        }
    }

    if area > 0 {
        Orientation::PostScript
    } else if area < 0 {
        Orientation::TrueType
    } else {
        Orientation::None
    }
}

/// `FT_Outline_EmboldenXY` (`ftoutln.c`): emboldens the outline by `xstrength`/`ystrength` (each halved first, as
/// in C). Returns `false` exactly where C returns an error (`Invalid_Argument` for an `FT_ORIENTATION_NONE`
/// outline with contours); `true` otherwise, including for empty outlines and zero strength.
pub fn embolden_xy(
    points: &mut [(i64, i64)],
    contour_ends: &[usize],
    xstrength: i64,
    ystrength: i64,
) -> bool {
    let xstrength = xstrength / 2;
    let ystrength = ystrength / 2;
    if xstrength == 0 && ystrength == 0 {
        return true;
    }

    let orientation = outline_orientation(points, contour_ends);
    if orientation == Orientation::None {
        return contour_ends.is_empty();
    }

    let mut last: i64 = -1;
    for &ce in contour_ends {
        let first = last + 1;
        last = ce as i64;
        let mut l_in: i64 = 0;

        /* pacify compiler */
        let mut in_ = (0i64, 0i64);
        let mut anchor = (0i64, 0i64);
        let mut l_anchor: i64 = 0;

        /* counter j cycles through the points; counter i advances only when points are moved; anchor k marks the
         * first moved point */
        let mut i = last;
        let mut j = first;
        let mut k: i64 = -1;
        while j != i && i != k {
            let (out, l_out);
            if j != k {
                let mut o = (
                    points[j as usize].0.wrapping_sub(points[i as usize].0),
                    points[j as usize].1.wrapping_sub(points[i as usize].1),
                );
                l_out = vector_norm_len(&mut o);
                out = o;

                if l_out == 0 {
                    j = if j < last { j + 1 } else { first };
                    continue;
                }
            } else {
                out = anchor;
                l_out = l_anchor;
            }

            if l_in != 0 {
                if k < 0 {
                    k = i;
                    anchor = in_;
                    l_anchor = l_in;
                }

                let mut d = mul_fix(in_.0, out.0).wrapping_add(mul_fix(in_.1, out.1));

                /* shift only if turn is less than ~160 degrees */
                let mut shift = (0i64, 0i64);
                if d > -0xF000 {
                    d = d.wrapping_add(0x10000);

                    /* shift components along lateral bisector in proper orientation */
                    shift.0 = in_.1.wrapping_add(out.1);
                    shift.1 = in_.0.wrapping_add(out.0);

                    if orientation == Orientation::TrueType {
                        shift.0 = shift.0.wrapping_neg();
                    } else {
                        shift.1 = shift.1.wrapping_neg();
                    }

                    /* restrict shift magnitude to better handle collapsing segments */
                    let mut q = mul_fix(out.0, in_.1).wrapping_sub(mul_fix(out.1, in_.0));
                    if orientation == Orientation::TrueType {
                        q = q.wrapping_neg();
                    }

                    let l = l_in.min(l_out);

                    /* non-strict inequalities avoid divide-by-zero when q == l == 0 */
                    if mul_fix(xstrength, q) <= mul_fix(l, d) {
                        shift.0 = mul_div(shift.0, xstrength, d);
                    } else {
                        shift.0 = mul_div(shift.0, l, q);
                    }

                    if mul_fix(ystrength, q) <= mul_fix(l, d) {
                        shift.1 = mul_div(shift.1, ystrength, d);
                    } else {
                        shift.1 = mul_div(shift.1, l, q);
                    }
                }

                while i != j {
                    let p = &mut points[i as usize];
                    p.0 = p.0.wrapping_add(xstrength.wrapping_add(shift.0));
                    p.1 = p.1.wrapping_add(ystrength.wrapping_add(shift.1));
                    i = if i < last { i + 1 } else { first };
                }
            } else {
                i = j;
            }

            in_ = out;
            l_in = l_out;
            j = if j < last { j + 1 } else { first };
        }
    }

    true
}

/// `FT_Outline_Embolden` (`ftoutln.c`): `embolden_xy` with the same strength in both directions.
pub fn embolden(points: &mut [(i64, i64)], contour_ends: &[usize], strength: i64) -> bool {
    embolden_xy(points, contour_ends, strength, strength)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_of_squares() {
        // counter-clockwise (y-up) square: area > 0 -> PostScript
        let ccw = [(0, 0), (640, 0), (640, 640), (0, 640)];
        assert_eq!(outline_orientation(&ccw, &[3]), Orientation::PostScript);
        // clockwise square: area < 0 -> TrueType
        let cw = [(0, 0), (0, 640), (640, 640), (640, 0)];
        assert_eq!(outline_orientation(&cw, &[3]), Orientation::TrueType);
    }

    #[test]
    fn orientation_of_degenerate_outlines() {
        // empty outline: C returns FT_ORIENTATION_TRUETYPE for n_points <= 0
        assert_eq!(outline_orientation(&[], &[]), Orientation::TrueType);
        // zero-area outline (collinear points): area == 0 -> None
        let flat = [(0, 0), (640, 0), (1280, 0)];
        assert_eq!(outline_orientation(&flat, &[2]), Orientation::None);
        // collapsed bbox (single point contour) -> None
        let one = [(100, 100)];
        assert_eq!(outline_orientation(&one, &[0]), Orientation::None);
    }

    #[test]
    fn norm_len_of_3_4_5() {
        let mut v = (3 * 64, 4 * 64);
        let l = vector_norm_len(&mut v);
        assert_eq!(l, 320);
        // unit vector in 16.16: (0.6, 0.8) * 65536 = (39321.6, 52428.8)
        assert_eq!(v, (39321, 52429));
    }

    #[test]
    fn norm_len_trivial_and_large() {
        let mut v = (0, 0);
        assert_eq!(vector_norm_len(&mut v), 0);
        assert_eq!(v, (0, 0));

        let mut v = (-3 * 64, 0);
        assert_eq!(vector_norm_len(&mut v), 192);
        assert_eq!(v, (-65536, 0));

        let mut v = (0, -7 * 64);
        assert_eq!(vector_norm_len(&mut v), 448);
        assert_eq!(v, (0, -65536));

        let mut v = (100000, 50000);
        let l = vector_norm_len(&mut v);
        assert_eq!(l, 111804); // ~sqrt(100000^2 + 50000^2) = 111803....
        assert_eq!(v, (58617, 29308));
    }

    #[test]
    fn embolden_square_both_orientations() {
        // PostScript square: C moves the +x/+y sides out by the full strength
        let mut ccw = [(0, 0), (640, 0), (640, 640), (0, 640)];
        assert!(embolden(&mut ccw, &[3], 64));
        assert_eq!(ccw, [(0, 0), (704, 0), (704, 704), (0, 704)]);

        let mut cw = [(0, 0), (0, 640), (640, 640), (640, 0)];
        assert!(embolden(&mut cw, &[3], 64));
        assert_eq!(cw, [(0, 0), (0, 704), (704, 704), (704, 0)]);
    }

    #[test]
    fn embolden_zero_strength_is_noop() {
        let mut sq = [(0, 0), (640, 0), (640, 640), (0, 640)];
        assert!(embolden(&mut sq, &[3], 0));
        assert_eq!(sq, [(0, 0), (640, 0), (640, 640), (0, 640)]);
        // odd strength halves toward zero: 1/2 == 0
        assert!(embolden(&mut sq, &[3], 1));
        assert_eq!(sq, [(0, 0), (640, 0), (640, 640), (0, 640)]);
    }

    #[test]
    fn embolden_triangle_stays_symmetric() {
        let mut tri = [(0, 0), (640, 0), (320, 640)];
        assert!(embolden(&mut tri, &[2], 64));
        // apex stays on the axis of symmetry x = 352 after emboldening
        assert_eq!(tri, [(-20, 0), (724, 0), (352, 744)]);
        assert_eq!(tri[2].0 - tri[0].0, tri[1].0 - tri[2].0);
    }

    #[test]
    fn embolden_single_point_contour_errors() {
        // one-point contour: orientation None with n_contours > 0 -> Invalid_Argument
        let mut p = [(100, 100)];
        assert!(!embolden(&mut p, &[0], 64));
        assert_eq!(p, [(100, 100)]);
    }

    #[test]
    fn embolden_two_contours() {
        let mut pts = [
            (0, 0),
            (640, 0),
            (640, 640),
            (0, 640),
            (1000, 1000),
            (1200, 1000),
            (1200, 1200),
        ];
        assert!(embolden(&mut pts, &[3, 6], 64));
        assert_eq!(
            pts,
            [
                (0, 0),
                (704, 0),
                (704, 704),
                (0, 704),
                (955, 1000),
                (1264, 1000),
                (1264, 1309),
            ]
        );
    }

    #[test]
    fn embolden_empty_outline_ok() {
        assert!(embolden(&mut [], &[], 64));
    }

    #[test]
    fn embolden_xy_directions() {
        let mut sq = [(0, 0), (640, 0), (640, 640), (0, 640)];
        assert!(embolden_xy(&mut sq, &[3], 64, 0));
        assert_eq!(sq, [(0, 0), (704, 0), (704, 640), (0, 640)]);
    }
}
