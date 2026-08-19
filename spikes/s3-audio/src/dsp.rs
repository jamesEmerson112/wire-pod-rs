//! Port of the Go server's per-chunk filter chain
//! (`chipper/pkg/wirepod/speechrequest/speechrequest.go`), bug-for-bug:
//! gain x5 (clipped) -> single-pole recurrence with alpha = dt/(rc+dt)
//! (the Go code uses the low-pass alpha inside a high-pass recurrence; kept
//! as-is for parity) -> truncating i16 cast -> gain x1.5 (clipped).
//! Filter state resets at every chunk boundary, exactly like the Go code.

use std::f64::consts::PI;

pub fn apply_gain(samples: &mut [i16], gain: f64) {
    for s in samples.iter_mut() {
        let amplified = f64::from(*s) * gain;
        *s = if amplified > f64::from(i16::MAX) {
            i16::MAX
        } else if amplified < f64::from(i16::MIN) {
            i16::MIN
        } else {
            amplified as i16
        };
    }
}

/// One chunk through the Go `highPassFilter`. Returns an empty vec for empty
/// input (the Go code would panic on `samples[0]`; empty chunks never occur).
pub fn high_pass_chunk(data: &[u8]) -> Vec<u8> {
    let mut samples: Vec<i16> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    if samples.is_empty() {
        return Vec::new();
    }
    apply_gain(&mut samples, 5.0);

    let rc = 1.0 / (2.0 * PI * 300.0);
    let dt = 1.0 / 16000.0;
    let alpha = dt / (rc + dt); // Go quirk: low-pass alpha in a high-pass recurrence

    let mut filtered = vec![0.0f64; samples.len()];
    let mut previous = f64::from(samples[0]);
    for i in 1..samples.len() {
        let current = f64::from(samples[i]);
        filtered[i] = alpha * (filtered[i - 1] + current - previous);
        previous = current;
    }

    // Go's int16(float64) truncates through the int pipeline rather than
    // saturating; `as i64 as i16` reproduces that for any realistic magnitude.
    let mut out: Vec<i16> = filtered.iter().map(|&f| f as i64 as i16).collect();
    apply_gain(&mut out, 1.5);
    out.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_clips_at_i16_bounds() {
        let mut s = [20000i16, -20000, 100];
        apply_gain(&mut s, 5.0);
        assert_eq!(s, [i16::MAX, i16::MIN, 500]);
    }

    #[test]
    fn first_output_sample_is_zero() {
        // Go never writes filteredSamples[0]; after the 1.5x gain it stays 0.
        let data: Vec<u8> = [1000i16, 1000, 1000]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = high_pass_chunk(&data);
        let first = i16::from_le_bytes([out[0], out[1]]);
        assert_eq!(first, 0);
    }

    #[test]
    fn constant_input_stays_near_zero() {
        // A DC input through the recurrence never grows past the first sample.
        let data: Vec<u8> = std::iter::repeat(500i16)
            .take(160)
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let out = high_pass_chunk(&data);
        let last = i16::from_le_bytes([out[out.len() - 2], out[out.len() - 1]]);
        assert_eq!(last, 0);
    }

    #[test]
    fn deterministic() {
        let data: Vec<u8> = (0..320i16).flat_map(|s| (s * 50).to_le_bytes()).collect();
        assert_eq!(high_pass_chunk(&data), high_pass_chunk(&data));
    }

    #[test]
    fn chunk_boundary_resets_state() {
        // Filtering two 320-byte chunks independently differs from one 640-byte
        // chunk -- the Go behavior we must preserve. Amplitude stays below the
        // x5 gain clip so the filter state at the split is non-trivial.
        let data: Vec<u8> = (0..320)
            .map(|i| {
                ((i as f64 / 16000.0 * 440.0 * 2.0 * std::f64::consts::PI).sin() * 2000.0) as i16
            })
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let (a, b) = data.split_at(320);
        let mut split = high_pass_chunk(a);
        split.extend(high_pass_chunk(b));
        let whole = high_pass_chunk(&data);
        assert_ne!(split, whole);
    }
}
