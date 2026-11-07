//! Plane-to-plane perspective transforms (homographies) from four point pairs.

use bevy::math::{DMat3, DVec3, Mat3, Vec2};

/// The homography mapping each `from[i]` to `to[i]`, or `None` for a degenerate quad.
pub fn from_quads(from: [Vec2; 4], to: [Vec2; 4]) -> Option<Mat3> {
    // h33 = 1; each pair gives two linear equations in the 8 other coefficients.
    let mut a = [[0.0f64; 9]; 8];
    for (i, (p, q)) in from.iter().zip(&to).enumerate() {
        let (x, y) = (f64::from(p.x), f64::from(p.y));
        let (u, v) = (f64::from(q.x), f64::from(q.y));
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    let h = solve(a)?;
    let m = DMat3::from_cols(
        DVec3::new(h[0], h[3], h[6]),
        DVec3::new(h[1], h[4], h[7]),
        DVec3::new(h[2], h[5], 1.0),
    );
    // A homography is defined up to a factor, sign included: make w positive inside the quad,
    // so "w > 0" means "in front of the camera" for whoever applies it.
    let center = from.iter().fold(Vec2::ZERO, |acc, p| acc + *p) / 4.0;
    let w = (m * center.as_dvec2().extend(1.0)).z;
    let m = if w < 0.0 { -m } else { m };
    // A singular matrix flattens the plane onto a line: not invertible, useless here.
    (m.is_finite() && m.determinant().abs() > 1e-12).then(|| m.as_mat3())
}

/// Applies `h` to `p`; `None` when the point maps behind the projection (w <= 0).
pub fn apply(h: &Mat3, p: Vec2) -> Option<Vec2> {
    let r = *h * p.extend(1.0);
    (r.z > 1e-6).then(|| r.truncate() / r.z)
}

/// Maps the segment `a`-`b`, keeping only its part in front of the projection (w > 0), so lines
/// crossing the horizon are cut there instead of wrapping around.
pub fn clip_segment(h: &Mat3, a: Vec2, b: Vec2) -> Option<(Vec2, Vec2)> {
    let w = |p: Vec2| (*h * p.extend(1.0)).z;
    let (wa, wb) = (w(a), w(b));
    // Keep away from w = 0, where points fly off to infinity.
    let min = wa.max(wb) * 0.02;
    if wa.max(wb) <= 1e-6 {
        return None;
    }
    // w is linear along the segment.
    let cut =
        |from: Vec2, to: Vec2, wf: f32, wt: f32| from + (to - from) * ((min - wf) / (wt - wf));
    let a2 = if wa < min { cut(a, b, wa, wb) } else { a };
    let b2 = if wb < min { cut(b, a, wb, wa) } else { b };
    Some((apply(h, a2)?, apply(h, b2)?))
}

/// Gaussian elimination with partial pivoting on an 8x8 augmented system.
fn solve(mut a: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let pivot_row = a[col];
        for (i, row) in a.iter_mut().enumerate() {
            if i != col {
                let k = row[col] / pivot_row[col];
                for (x, p) in row.iter_mut().zip(pivot_row).skip(col) {
                    *x -= k * p;
                }
            }
        }
    }
    Some(std::array::from_fn(|i| a[i][8] / a[i][i]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;

    const UNIT: [Vec2; 4] = [
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(0.0, 1.0),
    ];

    #[test]
    fn maps_the_corners_and_inverts() {
        let quad = [
            Vec2::new(0.12, 0.55),
            Vec2::new(0.91, 0.58),
            Vec2::new(0.97, 0.83),
            Vec2::new(0.04, 0.79),
        ];
        let forward = from_quads(UNIT, quad).unwrap();
        let back = from_quads(quad, UNIT).unwrap();
        for (p, q) in UNIT.iter().zip(&quad) {
            assert!(apply(&forward, *p).unwrap().distance(*q) < 1e-5);
            assert!(apply(&back, *q).unwrap().distance(*p) < 1e-5);
        }
        // Straight lines stay straight: the middle of an edge lands on the image of that edge.
        let mid = apply(&forward, Vec2::new(0.5, 0.0)).unwrap();
        let (a, b) = (quad[0], quad[1]);
        assert!((b - a).perp_dot(mid - a).abs() < 1e-5);
    }

    #[test]
    fn segments_are_cut_at_the_horizon() {
        // w = 1 - 2y: everything below y = 0.5 is behind the projection.
        let h = Mat3::from_cols(Vec3::X, Vec3::new(0.0, 1.0, -2.0), Vec3::Z);
        let (a, b) = clip_segment(&h, Vec2::new(0.3, 0.0), Vec2::new(0.3, 1.0)).unwrap();
        assert_eq!(a, Vec2::new(0.3, 0.0));
        assert!(b.y.is_finite() && b.y > 0.0);
        assert!(clip_segment(&h, Vec2::new(0.0, 0.8), Vec2::new(1.0, 0.9)).is_none());
    }

    #[test]
    fn rejects_degenerate_quads() {
        let line = [
            Vec2::ZERO,
            Vec2::new(1.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(3.0, 0.0),
        ];
        assert!(from_quads(UNIT, line).is_none());
    }
}
