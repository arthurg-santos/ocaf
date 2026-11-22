//! Linear prediction: fixed polynomial predictors and LPC analysis.
//!
//! The encoder evaluates several predictors per channel block; the decoder
//! only needs the coefficients stored in the subframe. Predictor arithmetic is
//! explicitly wrapping so that decoding is defined even for extreme 64-bit
//! float keys, while integer PCM never reaches the wrapping range.

/// Coefficients of the fixed predictor of the given order (0..=4).
pub fn fixed_coefficients(order: u8) -> Option<&'static [i64]> {
    match order {
        0 => Some(&[]),
        1 => Some(&[1]),
        2 => Some(&[2, -1]),
        3 => Some(&[3, -3, 1]),
        4 => Some(&[4, -6, 4, -1]),
        _ => None,
    }
}

/// Compute prediction residuals for the samples after the warm-up region.
pub fn residuals(samples: &[i64], coeffs: &[i64], shift: u8, out: &mut Vec<i64>) {
    let order = coeffs.len();
    out.clear();
    if samples.len() <= order {
        return;
    }
    out.reserve(samples.len() - order);
    if shift <= 15 {
        for i in order..samples.len() {
            let mut prediction = 0i64;
            for (j, &coeff) in coeffs.iter().enumerate() {
                prediction = prediction.wrapping_add(coeff.wrapping_mul(samples[i - 1 - j]));
            }
            out.push(samples[i].wrapping_sub(prediction >> shift));
        }
    } else {
        // High-precision LPC coefficients can overflow i64 during accumulation.
        for i in order..samples.len() {
            let mut prediction = 0i128;
            for (j, &coeff) in coeffs.iter().enumerate() {
                prediction += coeff as i128 * samples[i - 1 - j] as i128;
            }
            out.push(samples[i].wrapping_sub((prediction >> shift) as i64));
        }
    }
}

/// Reconstruct samples from warm-up values and residuals.
pub fn restore(
    warmup: &[i64],
    residuals: &[i64],
    coeffs: &[i64],
    shift: u8,
    total_samples: usize,
    out: &mut Vec<i64>,
) -> crate::error::Result<()> {
    let order = coeffs.len();
    if warmup.len() != order || total_samples < order || residuals.len() != total_samples - order {
        return Err(crate::error::Error::invalid_ocaf(
            "inconsistent predictor subframe lengths",
        ));
    }
    out.clear();
    out.reserve(total_samples);
    out.extend_from_slice(warmup);
    if shift <= 15 {
        for &residual in residuals {
            let i = out.len();
            let mut prediction = 0i64;
            for (j, &coeff) in coeffs.iter().enumerate() {
                prediction = prediction.wrapping_add(coeff.wrapping_mul(out[i - 1 - j]));
            }
            out.push((prediction >> shift).wrapping_add(residual));
        }
    } else {
        for &residual in residuals {
            let i = out.len();
            let mut prediction = 0i128;
            for (j, &coeff) in coeffs.iter().enumerate() {
                prediction += coeff as i128 * out[i - 1 - j] as i128;
            }
            out.push(((prediction >> shift) as i64).wrapping_add(residual));
        }
    }
    Ok(())
}

/// Windowed autocorrelation of `samples`, up to `max_order` lags.
///
/// A Tukey(0.5) window is applied to reduce the influence of block boundaries
/// on the LPC estimate. The window only affects coefficient estimation; the
/// residual is always computed from the unmodified samples.
pub fn autocorrelation(samples: &[i64], max_order: usize) -> Option<Vec<f64>> {
    let n = samples.len();
    if n <= max_order || max_order == 0 || n < 8 {
        return None;
    }
    let mut windowed = Vec::with_capacity(n);
    let alpha = 0.5f64;
    let edge = alpha * (n - 1) as f64 / 2.0;
    for (i, &sample) in samples.iter().enumerate() {
        let x = i as f64;
        let w = if x < edge {
            0.5 * (1.0 + (std::f64::consts::PI * (2.0 * x / (alpha * (n - 1) as f64) - 1.0)).cos())
        } else if x > (n - 1) as f64 - edge {
            let y = (n - 1) as f64 - x;
            0.5 * (1.0 + (std::f64::consts::PI * (2.0 * y / (alpha * (n - 1) as f64) - 1.0)).cos())
        } else {
            1.0
        };
        windowed.push(sample as f64 * w);
    }
    let mut r = vec![0f64; max_order + 1];
    for (lag, slot) in r.iter_mut().enumerate() {
        let mut sum = 0f64;
        for i in lag..n {
            sum += windowed[i] * windowed[i - lag];
        }
        if !sum.is_finite() {
            return None;
        }
        *slot = sum;
    }
    if r[0] <= 0.0 || !r[0].is_finite() {
        return None;
    }
    Some(r)
}

/// Solve the Yule-Walker equations with Levinson-Durbin recursion.
///
/// Returns predictor coefficients `a[0..order]` for
/// `x[n] ~= sum(a[j] * x[n-1-j])`.
pub fn levinson_durbin(r: &[f64], order: usize) -> Option<Vec<f64>> {
    if order == 0 {
        return Some(Vec::new());
    }
    if r.len() < order + 1 || r[0] <= 0.0 || !r[0].is_finite() {
        return None;
    }
    let mut a = vec![0f64; order];
    let mut previous = vec![0f64; order];
    let mut error = r[0];
    for i in 0..order {
        let mut acc = r[i + 1];
        for j in 0..i {
            acc -= a[j] * r[i - j];
        }
        if error.abs() < 1e-30 || !error.is_finite() {
            return None;
        }
        let k = acc / error;
        if !k.is_finite() {
            return None;
        }
        previous[..i].copy_from_slice(&a[..i]);
        for j in 0..i {
            a[j] = previous[j] - k * previous[i - 1 - j];
            if !a[j].is_finite() {
                return None;
            }
        }
        a[i] = k;
        error *= 1.0 - k * k;
        if !error.is_finite() {
            return None;
        }
    }
    Some(a)
}

/// Quantize LPC coefficients to `precision` fractional bits.
///
/// Coefficients are stored as 32-bit integers, so values with magnitude above
/// one (common for high-order predictors of periodic signals) are represented
/// without clipping.
pub fn quantize_coefficients(coeffs: &[f64], precision: u8) -> Vec<i32> {
    debug_assert!((1..=30).contains(&precision));
    let scale = (1i64 << precision) as f64;
    let mut error = 0.0f64;
    let mut quantized: Vec<i32> = Vec::with_capacity(coeffs.len());
    for &coeff in coeffs {
        let desired = coeff * scale + error;
        let rounded = desired.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
        error = desired - rounded as f64;
        quantized.push(rounded);
    }
    // Keep the predictor from having a large DC gain, which would amplify
    // quantized residual noise. This mirrors the stability constraint used by
    // predictive lossless codecs. The correction is applied in one step: a
    // per-unit loop can take billions of iterations when the raw coefficient
    // sum is large.
    let limit = 1i64 << precision;
    let mut sum: i64 = quantized.iter().map(|&c| c as i64).sum();
    let mut excess = (sum - limit).max(0);
    while excess > 0 {
        let Some(index) = quantized
            .iter()
            .enumerate()
            .max_by_key(|(_, &c)| c)
            .map(|(i, _)| i)
        else {
            break;
        };
        let available = quantized[index] as i64 - i32::MIN as i64;
        if available <= 0 {
            break;
        }
        let take = excess.min(available);
        quantized[index] = (quantized[index] as i64 - take) as i32;
        excess -= take;
        sum -= take;
    }
    debug_assert!(sum <= limit || excess > 0);
    quantized
}

/// Convert quantized coefficients back to i64 for the predictor loops.
pub fn widen(coeffs: &[i32]) -> Vec<i64> {
    coeffs.iter().map(|&c| c as i64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_predictor_orders() {
        let samples: Vec<i64> = (0..100).map(|i| i * i - 3 * i + 7).collect();
        for order in 0..=3u8 {
            let coeffs = fixed_coefficients(order).unwrap();
            let mut res = Vec::new();
            residuals(&samples, coeffs, 0, &mut res);
            let mut restored = Vec::new();
            restore(
                &samples[..order as usize],
                &res,
                coeffs,
                0,
                samples.len(),
                &mut restored,
            )
            .unwrap();
            assert_eq!(restored, samples);
        }
    }

    #[test]
    fn fixed_order_three_annihilates_quadratic() {
        let samples: Vec<i64> = (0..100).map(|i| i * i).collect();
        let coeffs = fixed_coefficients(3).unwrap();
        let mut res = Vec::new();
        residuals(&samples, coeffs, 0, &mut res);
        // The third difference of a quadratic is zero after warm-up.
        assert!(res.iter().all(|&r| r == 0));
    }

    #[test]
    fn lpc_reconstructs_exactly() {
        let samples: Vec<i64> = (0..500)
            .map(|i| {
                let t = i as f64;
                (10000.0 * (t * 0.05).sin() + 3000.0 * (t * 0.31).cos()) as i64
            })
            .collect();
        // Two sinusoids are exactly modelled by an order-4 AR process.
        let r = autocorrelation(&samples, 4).unwrap();
        let coeffs = levinson_durbin(&r, 4).unwrap();
        let quantized = quantize_coefficients(&coeffs, 15);
        let wide = widen(&quantized);
        let mut residual = Vec::new();
        residuals(&samples, &wide, 15, &mut residual);
        let mut restored = Vec::new();
        restore(
            &samples[..4],
            &residual,
            &wide,
            15,
            samples.len(),
            &mut restored,
        )
        .unwrap();
        assert_eq!(restored, samples);
        let energy: i64 = samples.iter().map(|s| s.abs()).sum();
        let residual_energy: i64 = residual.iter().map(|s| s.abs()).sum();
        assert!(residual_energy < energy / 4);
    }

    #[test]
    fn levinson_matches_direct_solution_for_order_two() {
        let samples: Vec<i64> = (0..200)
            .map(|i| {
                let t = i as f64;
                (5000.0 * (t * 0.11).sin() + 2000.0 * (t * 0.07).cos()) as i64
            })
            .collect();
        let r = autocorrelation(&samples, 2).unwrap();
        let coeffs = levinson_durbin(&r, 2).unwrap();
        // Solve the 2x2 Yule-Walker system directly.
        let det = r[0] * r[0] - r[1] * r[1];
        let a1 = (r[1] * r[0] - r[1] * r[2]) / det;
        let a2 = (r[0] * r[2] - r[1] * r[1]) / det;
        assert!((coeffs[0] - a1).abs() < 1e-9);
        assert!((coeffs[1] - a2).abs() < 1e-9);
    }

    #[test]
    fn quantized_coefficients_are_bounded() {
        let coeffs = [0.5, 0.4, -0.1, 0.3];
        let q = quantize_coefficients(&coeffs, 15);
        let sum: i64 = q.iter().map(|&v| v as i64).sum();
        assert!(sum <= 1i64 << 15);
    }
}
