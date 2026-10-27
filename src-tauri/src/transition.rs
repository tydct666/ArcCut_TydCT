//! Conservative, pause-anchored selection-transition verification.
//!
//! This module never discovers pauses or labels selection from camera movement.
//! Its caller supplies an independently confirmed pause and tri-state semantic
//! selection evidence. Unknown evidence is deliberately not a negative label.
//! Coordinates, duration gates and feature grids are resolution independent.

use serde::{Deserialize, Serialize};

const REDUCED_WIDTH: usize = 192;
const CONTEXT_US: i64 = 100_000;
const MAX_TRANSITION_US: i64 = 80_000;
const PATCH_RADIUS: usize = 4;
const SEARCH_RADIUS: isize = 16;

#[derive(Clone, Debug)]
pub struct GrayFrame {
    pub time_us: i64,
    pub pixels: Vec<u8>,
    /// Some(true): affirmative selection UI; Some(false): affirmative normal UI.
    pub selection: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    pub proposed_start_us: Option<i64>,
    pub reason: String,
    pub removed_frames: usize,
    pub candidate_frames: usize,
    pub candidate_duration_us: i64,
    /// Geometry distances use image width, not pixels at one source resolution.
    pub minimum_motion_width_fraction: f64,
    pub maximum_baseline_width_fraction: f64,
    pub minimum_inlier_fraction: f64,
    #[serde(default)]
    pub adjacent_fallback_frames: usize,
    #[serde(default)]
    pub maximum_roundtrip_width_fraction: f64,
}

impl Assessment {
    fn retained(reason: &'static str) -> Self {
        Self {
            proposed_start_us: None,
            reason: reason.into(),
            removed_frames: 0,
            candidate_frames: 0,
            candidate_duration_us: 0,
            minimum_motion_width_fraction: 0.,
            maximum_baseline_width_fraction: 0.,
            minimum_inlier_fraction: 0.,
            adjacent_fallback_frames: 0,
            maximum_roundtrip_width_fraction: 0.,
        }
    }
}

/// An area average, bounded to 192 columns, for source and streaming output audit.
/// It preserves aspect ratio and never stretches a mobile image to a PC layout.
pub fn downsample_gray(
    pixels: &[u8],
    width: usize,
    height: usize,
) -> Option<(Vec<u8>, usize, usize)> {
    if width < 64 || height < 48 || width.checked_mul(height)? != pixels.len() {
        return None;
    }
    let w = width.min(REDUCED_WIDTH);
    let h = (height * w / width).max(1);
    if width == w {
        return Some((pixels.to_vec(), w, height));
    }
    let mut small = vec![0; w * h];
    for y in 0..h {
        let ya = y * height / h;
        let yb = ((y + 1) * height / h).max(ya + 1);
        for x in 0..w {
            let xa = x * width / w;
            let xb = ((x + 1) * width / w).max(xa + 1);
            let mut sum = 0usize;
            for sy in ya..yb {
                for sx in xa..xb {
                    sum += pixels[sy * width + sx] as usize;
                }
            }
            small[y * w + x] = (sum / ((yb - ya) * (xb - xa))) as u8;
        }
    }
    Some((small, w, h))
}

struct Plane {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}

impl Plane {
    fn new(frame: &GrayFrame, width: usize, height: usize) -> Option<Self> {
        let (pixels, width, height) = downsample_gray(&frame.pixels, width, height)?;
        Some(Self {
            pixels,
            width,
            height,
        })
    }
    fn at(&self, x: usize, y: usize) -> f64 {
        self.pixels[y * self.width + x] as f64
    }
}

#[derive(Clone)]
struct Feature {
    x: usize,
    y: usize,
    centered: Vec<f64>,
    norm: f64,
}

fn patch_feature(image: &Plane, x: usize, y: usize) -> Option<Feature> {
    let mut centered = Vec::with_capacity(81);
    for yy in y - PATCH_RADIUS..=y + PATCH_RADIUS {
        for xx in x - PATCH_RADIUS..=x + PATCH_RADIUS {
            centered.push(image.at(xx, yy));
        }
    }
    let mean = centered.iter().sum::<f64>() / centered.len() as f64;
    for v in &mut centered {
        *v -= mean;
    }
    let norm = centered.iter().map(|v| v * v).sum::<f64>().sqrt();
    (norm >= 60.).then_some(Feature {
        x,
        y,
        centered,
        norm,
    })
}

fn corner_strength(image: &Plane, x: usize, y: usize) -> f64 {
    let (mut xx, mut xy, mut yy) = (0., 0., 0.);
    for py in y - 2..=y + 2 {
        for px in x - 2..=x + 2 {
            let gx = image.at(px + 1, py) - image.at(px - 1, py);
            let gy = image.at(px, py + 1) - image.at(px, py - 1);
            xx += gx * gx;
            xy += gx * gy;
            yy += gy * gy;
        }
    }
    ((xx + yy) - ((xx - yy).powi(2) + 4. * xy * xy).sqrt()) / 50.
}

fn features(image: &Plane) -> Vec<Feature> {
    let mut result = vec![];
    // Features are distributed over the field, excluding top controls, the
    // deployment strip and the outer border. No per-map landmarks are stored.
    let (cols, rows) = (8, 4);
    let xa = image.width * 8 / 100;
    let xb = image.width * 92 / 100;
    let ya = image.height * 18 / 100;
    let yb = image.height * 79 / 100;
    for row in 0..rows {
        for col in 0..cols {
            let ax = (xa + col * (xb - xa) / cols).max(PATCH_RADIUS + 1);
            let bx = (xa + (col + 1) * (xb - xa) / cols).min(image.width - PATCH_RADIUS - 1);
            let ay = (ya + row * (yb - ya) / rows).max(PATCH_RADIUS + 1);
            let by = (ya + (row + 1) * (yb - ya) / rows).min(image.height - PATCH_RADIUS - 1);
            let mut best = (0., ax, ay);
            for y in (ay..by).step_by(2) {
                for x in (ax..bx).step_by(2) {
                    let strength = corner_strength(image, x, y);
                    if strength > best.0 {
                        best = (strength, x, y);
                    }
                }
            }
            if best.0 >= 25. {
                if let Some(f) = patch_feature(image, best.1, best.2) {
                    result.push(f);
                }
            }
        }
    }
    result
}

fn correlation(feature: &Feature, image: &Plane, x: usize, y: usize) -> f64 {
    let (mut sum, mut squared, mut product, mut i) = (0., 0., 0., 0);
    for yy in y - PATCH_RADIUS..=y + PATCH_RADIUS {
        for xx in x - PATCH_RADIUS..=x + PATCH_RADIUS {
            let value = image.at(xx, yy);
            sum += value;
            squared += value * value;
            product += value * feature.centered[i];
            i += 1;
        }
    }
    let variance = squared - sum * sum / feature.centered.len() as f64;
    if variance <= 1. {
        return -1.;
    }
    product / (feature.norm * variance.sqrt())
}

#[derive(Clone, Copy, Debug)]
struct Match {
    x: f64,
    y: f64,
    u: f64,
    v: f64,
}

fn track(feature: &Feature, image: &Plane) -> Option<Match> {
    let radius = SEARCH_RADIUS;
    let mut scores = Vec::with_capacity(((radius * 2 + 1).pow(2)) as usize);
    let mut best = (-1., 0usize, 0usize);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let (x, y) = (feature.x as isize + dx, feature.y as isize + dy);
            if x < PATCH_RADIUS as isize
                || y < PATCH_RADIUS as isize
                || x + PATCH_RADIUS as isize >= image.width as isize
                || y + PATCH_RADIUS as isize >= image.height as isize
            {
                continue;
            }
            let score = correlation(feature, image, x as usize, y as usize);
            scores.push((score, x as usize, y as usize));
            if score > best.0 {
                best = (score, x as usize, y as usize);
            }
        }
    }
    if best.0 < 0.78 {
        return None;
    }
    let other = scores
        .iter()
        .filter(|(_, x, y)| x.abs_diff(best.1) > 2 || y.abs_diff(best.2) > 2)
        .map(|s| s.0)
        .fold(-1., f64::max);
    if best.0 - other < 0.035 {
        return None;
    }
    Some(Match {
        x: feature.x as f64,
        y: feature.y as f64,
        u: best.1 as f64,
        v: best.2 as f64,
    })
}

fn solve(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let pivot = (col..3).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-8 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let divisor = a[col][col];
        for j in col..3 {
            a[col][j] /= divisor;
        }
        b[col] /= divisor;
        for row in 0..3 {
            if row == col {
                continue;
            }
            let factor = a[row][col];
            for j in col..3 {
                a[row][j] -= factor * a[col][j];
            }
            b[row] -= factor * b[col];
        }
    }
    Some(b)
}

#[derive(Clone, Copy, Debug)]
struct Affine {
    x: [f64; 3],
    y: [f64; 3],
}

impl Affine {
    /// self(before(point)): accumulate only already-verified adjacent motion.
    fn compose(self, before: Self) -> Self {
        Self {
            x: [
                self.x[0] * before.x[0] + self.x[1] * before.y[0],
                self.x[0] * before.x[1] + self.x[1] * before.y[1],
                self.x[0] * before.x[2] + self.x[1] * before.y[2] + self.x[2],
            ],
            y: [
                self.y[0] * before.x[0] + self.y[1] * before.y[0],
                self.y[0] * before.x[1] + self.y[1] * before.y[1],
                self.y[0] * before.x[2] + self.y[1] * before.y[2] + self.y[2],
            ],
        }
    }
    fn motion(&self, width: usize, height: usize) -> f64 {
        let mut distances = vec![];
        for yy in [0.25, 0.45, 0.65] {
            for xx in [0.2, 0.4, 0.6, 0.8] {
                let (x, y) = (xx * width as f64, yy * height as f64);
                let (u, v) = self.point(x, y);
                distances.push((x - u).hypot(y - v));
            }
        }
        distances.sort_by(f64::total_cmp);
        distances[distances.len() / 2] / width as f64
    }
    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.x[0] * x + self.x[1] * y + self.x[2],
            self.y[0] * x + self.y[1] * y + self.y[2],
        )
    }
    fn residual(&self, m: &Match) -> f64 {
        let (x, y) = self.point(m.x, m.y);
        (x - m.u).hypot(y - m.v)
    }
    fn plausible(&self) -> bool {
        // Short UI-camera transitions cannot legitimately reflect or heavily
        // distort the field. Degenerate patch matches are not evidence.
        let determinant = self.x[0] * self.y[1] - self.x[1] * self.y[0];
        (0.70..1.40).contains(&determinant)
            && self.x[1].abs() < 0.25
            && self.y[0].abs() < 0.25
            && (0.75..1.25).contains(&self.x[0])
            && (0.75..1.25).contains(&self.y[1])
    }
    /// A sequence can accumulate more zoom than one short step. Bound the
    /// resulting geometry by its principal scales and conditioning, rather
    /// than reusing the per-step area-change limit. The independent reverse
    /// consistency budget and return-to-baseline tests still apply.
    fn cumulative_plausible(&self, width: usize, height: usize) -> bool {
        if !self.x.iter().chain(&self.y).all(|v| v.is_finite()) {
            return false;
        }
        let (a, b, c, d) = (self.x[0], self.x[1], self.y[0], self.y[1]);
        let determinant = a * d - b * c;
        if determinant <= 0. {
            return false;
        }
        // Eigenvalues of A^T A are the squared principal scales. This catches
        // a collapsed axis and strong shear even when the determinant is near1.
        let trace = a * a + b * b + c * c + d * d;
        let discriminant = (trace * trace - 4. * determinant * determinant)
            .max(0.)
            .sqrt();
        let low = ((trace - discriminant) / 2.).max(0.).sqrt();
        let high = ((trace + discriminant) / 2.).sqrt();
        if low < 0.5 || high > 2. || high / low > 2. {
            return false;
        }
        // Camera focusing may translate, zoom and mildly skew the field; a
        // large rotation or losing most of the field is not a bounded repair.
        if (c - b).atan2(a + d).abs() > 20f64.to_radians() {
            return false;
        }
        for yy in [0.18, 0.8] {
            for xx in [0.12, 0.88] {
                let (x, y) = (xx * width as f64, yy * height as f64);
                let (u, v) = self.point(x, y);
                if (u - x).abs() > width as f64 * 0.35 || (v - y).abs() > height as f64 * 0.35 {
                    return false;
                }
            }
        }
        true
    }
}

fn chain_is_credible(
    forward_motion: f64,
    roundtrip: f64,
    accumulated_roundtrip: f64,
    model: &Affine,
    width: usize,
    height: usize,
) -> bool {
    roundtrip <= 0.0035
        && accumulated_roundtrip <= 0.012
        && forward_motion <= 0.05
        && model.cumulative_plausible(width, height)
}

fn fit(matches: &[Match]) -> Option<Affine> {
    let (mut ata, mut bx, mut by) = ([[0.; 3]; 3], [0.; 3], [0.; 3]);
    for m in matches {
        let p = [m.x, m.y, 1.];
        for i in 0..3 {
            bx[i] += p[i] * m.u;
            by[i] += p[i] * m.v;
            for j in 0..3 {
                ata[i][j] += p[i] * p[j];
            }
        }
    }
    Some(Affine {
        x: solve(ata, bx)?,
        y: solve(ata, by)?,
    })
}

#[derive(Clone, Copy, Debug)]
struct Registration {
    motion: f64,
    inlier_fraction: f64,
    model: Affine,
}

fn registration(reference: &Plane, target: &Plane, tracked: &[Feature]) -> Option<Registration> {
    if reference.width != target.width || reference.height != target.height || tracked.len() < 12 {
        return None;
    }
    let matches: Vec<_> = tracked.iter().filter_map(|f| track(f, target)).collect();
    if matches.len() < 10 {
        return None;
    }
    let mut best = Vec::new();
    let mut seed = 0x8e39_d721u32;
    // Fixed work and deterministic order; no unbounded estimator or randomness.
    for _ in 0..192 {
        let mut ids = [0usize; 3];
        for id in &mut ids {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *id = seed as usize % matches.len();
        }
        if ids[0] == ids[1] || ids[0] == ids[2] || ids[1] == ids[2] {
            continue;
        }
        let group = [matches[ids[0]], matches[ids[1]], matches[ids[2]]];
        if let Some(model) = fit(&group).filter(Affine::plausible) {
            let inliers: Vec<_> = matches
                .iter()
                .copied()
                .filter(|m| model.residual(m) <= 1.15)
                .collect();
            if inliers.len() > best.len() {
                best = inliers;
            }
        }
    }
    let fraction = best.len() as f64 / tracked.len() as f64;
    // Selection menus can occlude a substantial part of the battlefield. Keep
    // both an absolute spatial-support gate and a majority-of-valid-matches
    // gate, rather than treating every occluded feature as a geometric outlier.
    if best.len() < 12 || fraction < 0.4 || best.len() as f64 / (matches.len() as f64) < 0.6 {
        return None;
    }
    // Require support spread over both axes, not just a character or skill icon.
    let (minx, maxx, miny, maxy) = best.iter().fold(
        (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ),
        |b, m| (b.0.min(m.x), b.1.max(m.x), b.2.min(m.y), b.3.max(m.y)),
    );
    if maxx - minx < reference.width as f64 * 0.45 || maxy - miny < reference.height as f64 * 0.30 {
        return None;
    }
    let model = fit(&best).filter(Affine::plausible)?;
    Some(Registration {
        motion: model.motion(reference.width, reference.height),
        inlier_fraction: fraction,
        model,
    })
}

fn probe_indices(first: usize, last: usize) -> Vec<usize> {
    let mut result = vec![first, (first + last) / 2, last];
    result.sort_unstable();
    result.dedup();
    result
}

/// `pre` excludes paused frames; `post` starts at the first resumed frame.
/// A zero-length pause is permitted for auditing the already-cut output seam.
/// The caller must never invoke this on an unconfirmed/manual pause for editing.
pub fn assess(
    pre: &[GrayFrame],
    post: &[GrayFrame],
    width: usize,
    height: usize,
    pause_start_us: i64,
    pause_end_us: i64,
) -> Assessment {
    let mut result = Assessment::retained("no_adjacent_selection");
    if pause_end_us < pause_start_us
        || pre.is_empty()
        || post.is_empty()
        || pre.len() > 384
        || post.len() > 384
        || pre.last().is_some_and(|f| f.time_us >= pause_start_us)
        || post.first().is_some_and(|f| f.time_us < pause_end_us)
        || pre
            .windows(2)
            .chain(post.windows(2))
            .any(|w| w[0].time_us >= w[1].time_us)
        || width.checked_mul(height).is_none()
        || pre
            .iter()
            .chain(post)
            .any(|f| f.pixels.len() != width * height)
    {
        result.reason = "invalid_frames".into();
        return result;
    }
    match pre.last().unwrap().selection {
        Some(false) => return result,
        None => {
            result.reason = "unknown_selection".into();
            return result;
        }
        Some(true) => {}
    }
    let onset = pre
        .iter()
        .rposition(|f| f.selection != Some(true))
        .map_or(0, |i| i + 1);
    let start = pre[onset].time_us;
    result.candidate_frames = pre.len() - onset;
    result.candidate_duration_us = pause_start_us - start;
    if onset == 0 || result.candidate_duration_us > MAX_TRANSITION_US {
        result.reason = "protected_sustained_selection".into();
        return result;
    }
    if post.iter().any(|f| f.selection == Some(true)) {
        result.reason = "protected_continued_selection".into();
        return result;
    }
    let first = pre.partition_point(|f| f.time_us < start - CONTEXT_US);
    let first = first.saturating_sub(1);
    if start - pre[first].time_us < CONTEXT_US
        || post.last().unwrap().time_us - pause_end_us < CONTEXT_US
        || post[0].time_us - pause_end_us > 40_000
        || pause_start_us - pre.last().unwrap().time_us > 40_000
        || pre[first..]
            .windows(2)
            .chain(post.windows(2))
            .any(|w| w[1].time_us - w[0].time_us > 40_000)
    {
        result.reason = "insufficient_context".into();
        return result;
    }
    if pre[first..onset]
        .iter()
        .chain(post)
        .any(|f| f.selection.is_none())
    {
        result.reason = "unknown_selection".into();
        return result;
    }
    if pre[first..onset].iter().any(|f| f.selection != Some(false)) {
        result.reason = "protected_recent_selection".into();
        return result;
    }
    let Some(reference) = Plane::new(&pre[onset - 1], width, height) else {
        result.reason = "invalid_frames".into();
        return result;
    };
    let tracked = features(&reference);
    let stable_limit = 0.0035; // < 0.7 px at the reduced analysis width.
    let motion_limit = 0.0065; // > 1.2 px, supported across the battlefield.
    result.minimum_inlier_fraction = 1.;
    for i in probe_indices(first, onset - 1) {
        let Some(target) = Plane::new(&pre[i], width, height) else {
            result.reason = "invalid_frames".into();
            return result;
        };
        let Some(r) = registration(&reference, &target, &tracked) else {
            result.reason = "geometry_ambiguous".into();
            return result;
        };
        result.maximum_baseline_width_fraction =
            result.maximum_baseline_width_fraction.max(r.motion);
        result.minimum_inlier_fraction = result.minimum_inlier_fraction.min(r.inlier_fraction);
        if r.motion > stable_limit {
            result.reason = "unstable_pre_context".into();
            return result;
        }
    }
    for i in probe_indices(0, post.len() - 1) {
        let Some(target) = Plane::new(&post[i], width, height) else {
            result.reason = "invalid_frames".into();
            return result;
        };
        let Some(r) = registration(&reference, &target, &tracked) else {
            result.reason = "geometry_ambiguous".into();
            return result;
        };
        result.maximum_baseline_width_fraction =
            result.maximum_baseline_width_fraction.max(r.motion);
        result.minimum_inlier_fraction = result.minimum_inlier_fraction.min(r.inlier_fraction);
        if r.motion > stable_limit {
            result.reason = "geometry_not_returned".into();
            return result;
        }
    }
    result.minimum_motion_width_fraction = 1.;
    let mut previous: Option<(Plane, Affine)> = None;
    let mut accumulated_roundtrip = 0.;
    for frame in &pre[onset..] {
        let Some(target) = Plane::new(frame, width, height) else {
            result.reason = "invalid_frames".into();
            return result;
        };
        let r = if let Some(direct) = registration(&reference, &target, &tracked) {
            // A fresh anchor match cannot accumulate tracking drift.
            accumulated_roundtrip = 0.;
            direct
        } else if let Some((last, accumulated)) = &previous {
            // Growing UI camera motion can exceed the bounded anchor search.
            // Track one actual frame at a time with exactly the same support
            // gates, and require independently estimated reverse agreement.
            let forward = registration(last, &target, &features(last));
            let reverse = registration(&target, last, &features(&target));
            let (Some(forward), Some(reverse)) = (forward, reverse) else {
                result.reason = "geometry_ambiguous".into();
                return result;
            };
            let roundtrip = reverse
                .model
                .compose(forward.model)
                .motion(reference.width, reference.height);
            accumulated_roundtrip += roundtrip;
            result.maximum_roundtrip_width_fraction =
                result.maximum_roundtrip_width_fraction.max(roundtrip);
            let model = forward.model.compose(*accumulated);
            if !chain_is_credible(
                forward.motion,
                roundtrip,
                accumulated_roundtrip,
                &model,
                reference.width,
                reference.height,
            ) {
                result.reason = "geometry_chain_ambiguous".into();
                return result;
            }
            result.adjacent_fallback_frames += 1;
            Registration {
                motion: model.motion(reference.width, reference.height),
                inlier_fraction: forward.inlier_fraction.min(reverse.inlier_fraction),
                model,
            }
        } else {
            result.reason = "geometry_ambiguous".into();
            return result;
        };
        result.minimum_motion_width_fraction = result.minimum_motion_width_fraction.min(r.motion);
        result.minimum_inlier_fraction = result.minimum_inlier_fraction.min(r.inlier_fraction);
        if r.motion < motion_limit {
            result.reason = "local_effect_or_no_motion".into();
            return result;
        }
        previous = Some((target, r.model));
    }
    if result.minimum_motion_width_fraction < result.maximum_baseline_width_fraction * 3. + 0.003 {
        result.reason = "insufficient_seam_improvement".into();
        return result;
    }
    result.reason = "accepted_transition".into();
    result.proposed_start_us = Some(start);
    result.removed_frames = result.candidate_frames;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    const W: usize = 192;
    const H: usize = 108;
    fn texture() -> Vec<u8> {
        let mut seed = 17u32;
        (0..W * H)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect()
    }
    fn warp(src: &[u8], dx: f64, dy: f64, scale: f64) -> Vec<u8> {
        let mut out = vec![0; W * H];
        for y in 0..H {
            for x in 0..W {
                let sx = ((x as f64 - dx - W as f64 / 2.) / scale + W as f64 / 2.).round() as isize;
                let sy = ((y as f64 - dy - H as f64 / 2.) / scale + H as f64 / 2.).round() as isize;
                if sx >= 0 && sy >= 0 && sx < W as isize && sy < H as isize {
                    out[y * W + x] = src[sy as usize * W + sx as usize];
                }
            }
        }
        out
    }
    fn fixture() -> (Vec<GrayFrame>, Vec<GrayFrame>) {
        let src = texture();
        let mut pre: Vec<_> = (0..16)
            .map(|i| GrayFrame {
                time_us: i * 10_000,
                pixels: src.clone(),
                selection: Some(false),
            })
            .collect();
        for i in 0..4 {
            pre.push(GrayFrame {
                time_us: 160_000 + i * 10_000,
                pixels: warp(&src, 2. + i as f64, 2., 1.),
                selection: Some(true),
            });
        }
        let post = (0..13)
            .map(|i| GrayFrame {
                time_us: 1_000_000 + i * 10_000,
                pixels: src.clone(),
                selection: Some(false),
            })
            .collect();
        (pre, post)
    }
    #[test]
    fn short_global_selected_transition_returns_to_normal() {
        let (pre, post) = fixture();
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert_eq!(a.reason, "accepted_transition", "{a:?}");
        assert_eq!(a.proposed_start_us, Some(160_000));
        assert_eq!(a.removed_frames, 4);
    }
    #[test]
    fn zero_duration_output_seam_has_identical_decision() {
        let (pre, mut post) = fixture();
        for f in &mut post {
            f.time_us -= 800_000;
        }
        let a = assess(&pre, &post, W, H, 200_000, 200_000);
        assert_eq!(a.reason, "accepted_transition", "{a:?}");
    }
    #[test]
    fn sustained_and_continued_selection_are_protected() {
        let (mut pre, mut post) = fixture();
        for f in &mut pre[8..] {
            f.selection = Some(true)
        }
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "protected_sustained_selection"
        );
        let (normal_pre, _) = fixture();
        post[0].selection = Some(true);
        assert_eq!(
            assess(&normal_pre, &post, W, H, 200_000, 1_000_000).reason,
            "protected_continued_selection"
        );
    }
    #[test]
    fn no_semantic_selection_never_deletes_camera_motion() {
        let (mut pre, post) = fixture();
        for f in &mut pre {
            f.selection = None;
        }
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "unknown_selection"
        );
        for f in &mut pre {
            f.selection = Some(false);
        }
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "no_adjacent_selection"
        );
    }
    #[test]
    fn local_skill_effect_is_not_global_camera_motion() {
        let (mut pre, post) = fixture();
        let original = texture();
        for f in &mut pre[16..] {
            f.pixels = original.clone();
            for y in 40..65 {
                for x in 70..110 {
                    f.pixels[y * W + x] = 255;
                }
            }
        }
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert_eq!(a.reason, "local_effect_or_no_motion", "{a:?}");
    }
    #[test]
    fn camera_that_does_not_return_is_protected() {
        let (pre, mut post) = fixture();
        for f in &mut post {
            f.pixels = warp(&f.pixels, 5., 0., 1.);
        }
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert_eq!(a.reason, "geometry_not_returned", "{a:?}");
    }
    #[test]
    fn unreadable_low_texture_never_creates_geometry_evidence() {
        let (mut pre, mut post) = fixture();
        for f in pre.iter_mut().chain(&mut post) {
            f.pixels.fill(100)
        }
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "geometry_ambiguous"
        );
    }
    #[test]
    fn scale_and_translation_have_spatially_distributed_support() {
        let (mut pre, post) = fixture();
        let src = texture();
        for (i, f) in pre[16..].iter_mut().enumerate() {
            f.pixels = warp(&src, 2. + i as f64, 2., 1.02);
        }
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert_eq!(a.reason, "accepted_transition", "{a:?}");
    }
    #[test]
    fn adjacent_tracking_handles_motion_beyond_anchor_search() {
        let (mut pre, post) = fixture();
        let src = texture();
        for (i, f) in pre[16..].iter_mut().enumerate() {
            f.pixels = warp(&src, 2. + i as f64 * 7., 0., 1.);
        }
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert_eq!(a.reason, "accepted_transition", "{a:?}");
        assert!(a.adjacent_fallback_frames > 0);
        assert!(a.maximum_roundtrip_width_fraction < 0.0035);
        assert_eq!(a.proposed_start_us, Some(160_000));
    }
    #[test]
    fn adjacent_tracking_cannot_bridge_an_unmatchable_scene_change() {
        let (mut pre, post) = fixture();
        for f in &mut pre[17..] {
            f.pixels = texture().into_iter().rev().collect();
        }
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert!(a.proposed_start_us.is_none(), "{a:?}");
        assert_eq!(a.reason, "geometry_ambiguous");
    }
    #[test]
    fn moving_local_effect_does_not_accumulate_into_camera_motion() {
        let (mut pre, post) = fixture();
        let src = texture();
        for (i, f) in pre[16..].iter_mut().enumerate() {
            let moved = warp(&src, 2. + i as f64 * 7., 0., 1.);
            f.pixels = src.clone();
            for y in 35..70 {
                for x in 65..125 {
                    f.pixels[y * W + x] = moved[y * W + x];
                }
            }
        }
        let a = assess(&pre, &post, W, H, 200_000, 1_000_000);
        assert!(a.proposed_start_us.is_none(), "{a:?}");
        assert_eq!(a.reason, "local_effect_or_no_motion");
        assert_eq!(a.adjacent_fallback_frames, 0);
    }
    #[test]
    fn accumulated_zoom_has_independent_bounds_from_each_step() {
        let step = Affine {
            x: [0.96, 0., W as f64 * 0.02],
            y: [0., 0.96, H as f64 * 0.02],
        };
        assert!(step.plausible());
        let mut accumulated = Affine {
            x: [1., 0., 0.],
            y: [0., 1., 0.],
        };
        for _ in 0..8 {
            accumulated = step.compose(accumulated);
            assert!(accumulated.cumulative_plausible(W, H));
        }
        // Eight modest steps produce about0.72 linear/0.52 area scale. It is
        // invalid as one abrupt step, but valid as a verified smooth sequence.
        assert!(!accumulated.plausible());
        assert!(chain_is_credible(0.01, 0.0005, 0.004, &accumulated, W, H));
    }
    #[test]
    fn cumulative_geometry_rejects_reflection_shear_collapse_and_excessive_motion() {
        for model in [
            Affine {
                x: [-1., 0., W as f64],
                y: [0., 1., 0.],
            },
            Affine {
                x: [1., 1.1, 0.],
                y: [0., 1., 0.],
            },
            Affine {
                x: [0.01, 0., 0.],
                y: [0., 1., 0.],
            },
            Affine {
                x: [1., 0., W as f64],
                y: [0., 1., 0.],
            },
            Affine {
                x: [0., -1., 0.],
                y: [1., 0., 0.],
            },
        ] {
            assert!(!model.cumulative_plausible(W, H), "{model:?}");
        }
    }
    #[test]
    fn reverse_mismatch_and_cumulative_drift_cannot_be_hidden_by_plausible_geometry() {
        let model = Affine {
            x: [0.9, 0., 8.],
            y: [0., 0.9, 4.],
        };
        assert!(model.cumulative_plausible(W, H));
        assert!(!chain_is_credible(0.01, 0.004, 0.004, &model, W, H));
        assert!(!chain_is_credible(0.01, 0.002, 0.014, &model, W, H));
        assert!(!chain_is_credible(0.06, 0.0001, 0.0001, &model, W, H));
    }
    #[test]
    fn missing_context_and_unknown_post_state_are_retained() {
        let (pre, mut post) = fixture();
        assert_eq!(
            assess(&pre[12..], &post, W, H, 200_000, 1_000_000).reason,
            "insufficient_context"
        );
        post[3].selection = None;
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "unknown_selection"
        );
    }
    #[test]
    fn malformed_or_out_of_order_frames_are_rejected() {
        let (mut pre, post) = fixture();
        pre[0].pixels.pop();
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "invalid_frames"
        );
        let (mut pre, post) = fixture();
        pre[1].time_us = pre[0].time_us;
        assert_eq!(
            assess(&pre, &post, W, H, 200_000, 1_000_000).reason,
            "invalid_frames"
        );
    }
    #[test]
    fn downsampling_preserves_mobile_aspect() {
        let (_, w, h) = downsample_gray(&vec![128; 640 * 288], 640, 288).unwrap();
        assert_eq!((w, h), (192, 86));
        assert!(downsample_gray(&[1, 2], 640, 360).is_none());
    }
}
