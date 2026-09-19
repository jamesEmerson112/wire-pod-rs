//! Translation of `pkg/wirepod/ttr/convert.go`.

pub fn bytes_to_int16s(data: &[u8]) -> Vec<i16> {
    data.chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

pub fn int16s_to_bytes(data: &[i16]) -> Vec<u8> {
    data.iter().flat_map(|val| val.to_le_bytes()).collect()
}

pub fn downsample_24k_to_16k(input: &[u8]) -> Vec<Vec<u8>> {
    let out_bytes = downsample_24k_to_16k_linear(input);
    let mut audio_chunks = Vec::new();
    let filtered_bytes = low_pass_filter(&out_bytes, 4000.0, 16000);
    let i_vol_bytes = increase_volume(&filtered_bytes, 5.0);
    let mut i_vol_bytes = i_vol_bytes.as_slice();
    while !i_vol_bytes.is_empty() {
        if i_vol_bytes.len() < 1024 {
            let mut chunk = vec![0u8; 1024];
            chunk[..i_vol_bytes.len()].copy_from_slice(i_vol_bytes);
            audio_chunks.push(chunk);
            break;
        }
        audio_chunks.push(i_vol_bytes[..1024].to_vec());
        i_vol_bytes = &i_vol_bytes[1024..];
    }

    audio_chunks
}

pub fn increase_volume(data: &[u8], factor: f64) -> Vec<u8> {
    let mut int16s = bytes_to_int16s(data);

    for v in int16s.iter_mut() {
        let scaled = f64::from(*v) * factor;
        if scaled > f64::from(i16::MAX) {
            *v = i16::MAX;
        } else if scaled < f64::from(i16::MIN) {
            *v = i16::MIN;
        } else {
            *v = scaled as i16;
        }
    }

    int16s_to_bytes(&int16s)
}

// this is copied
// Go writes 3.1416 rather than pi, and the filter's output depends on it.
#[allow(clippy::approx_constant)]
pub fn low_pass_filter(data: &[u8], cutoff_freq: f64, sample_rate: i32) -> Vec<u8> {
    let int16s = bytes_to_int16s(data);
    // Go indexes element 0 of an empty slice here and panics.
    if int16s.is_empty() {
        return Vec::new();
    }
    let mut filtered = vec![0i16; int16s.len()];
    let rc = 1.0 / (2.0 * 3.1416 * cutoff_freq);
    let dt = 1.0 / f64::from(sample_rate);
    let alpha = dt / (rc + dt);
    filtered[0] = int16s[0];
    for i in 1..int16s.len() {
        let current = alpha * f64::from(int16s[i]) + (1.0 - alpha) * f64::from(filtered[i - 1]);
        filtered[i] = current as i16;
    }

    int16s_to_bytes(&filtered)
}

// copied too
pub fn downsample_24k_to_16k_linear(input: &[u8]) -> Vec<u8> {
    let int16s = bytes_to_int16s(input);
    let output_length = (int16s.len() * 2) / 3;
    let mut output = vec![0i16; output_length];

    let mut j = 0;
    let mut i = 0;
    while i + 2 < int16s.len() {
        let first = (2 * i32::from(int16s[i]) + i32::from(int16s[i + 1])) / 3;
        let second = (i32::from(int16s[i + 1]) + 2 * i32::from(int16s[i + 2])) / 3;
        output[j] = first as i16;
        output[j + 1] = second as i16;
        j += 2;
        i += 3;
    }

    int16s_to_bytes(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_samples_in_become_two_out() {
        let input = int16s_to_bytes(&[300, 600, 900, -300, -600, -900]);
        let out = bytes_to_int16s(&downsample_24k_to_16k_linear(&input));
        assert_eq!(out, vec![400, 800, -400, -800]);
    }

    #[test]
    fn chunks_are_1024_bytes_and_the_last_one_is_zero_padded() {
        let input = int16s_to_bytes(&vec![1000i16; 1000]);
        let chunks = downsample_24k_to_16k(&input);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.len() == 1024));
        assert!(chunks[1].ends_with(&[0, 0]));
        assert!(downsample_24k_to_16k(&[]).is_empty());
    }

    #[test]
    fn volume_is_clamped_to_the_int16_range() {
        let out = bytes_to_int16s(&increase_volume(
            &int16s_to_bytes(&[10000, -10000, 100]),
            5.0,
        ));
        assert_eq!(out, vec![i16::MAX, i16::MIN, 500]);
    }
}
