//! RIFF/WAVE writer (16-bit PCM) with an optional `smpl` loop chunk.

use crate::hca::Pcm;

/// Serialize interleaved 16-bit PCM as a WAV file. Loop points go into a `smpl` chunk (players that
/// understand it loop; browsers ignore it).
pub fn write(pcm: &Pcm) -> Vec<u8> {
    let channels = pcm.channels.max(1);
    let block_align = channels * 2;
    let data_len = pcm.samples.len() * 2;
    let smpl_len = if pcm.loop_range.is_some() { 36 + 24 } else { 0 };
    let riff_len = 4 + (8 + 16) + (8 + data_len) + if smpl_len > 0 { 8 + smpl_len } else { 0 };
    let mut out = Vec::with_capacity(riff_len + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&pcm.sample_rate.to_le_bytes());
    out.extend_from_slice(&(pcm.sample_rate * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    if let Some((start, end)) = pcm.loop_range {
        out.extend_from_slice(b"smpl");
        out.extend_from_slice(&(smpl_len as u32).to_le_bytes());
        for v in [0u32, 0, 1_000_000_000 / pcm.sample_rate.max(1), 60, 0, 0, 0, 1, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0u32, 0, start as u32, end.saturating_sub(1) as u32, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in &pcm.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Read back the samples of a 16-bit PCM WAV (only what the tests need): `(channels, rate, samples)`.
pub fn read(d: &[u8]) -> Option<(u16, u32, Vec<i16>)> {
    if d.len() < 12 || &d[..4] != b"RIFF" || &d[8..12] != b"WAVE" {
        return None;
    }
    let (mut channels, mut rate, mut samples) = (0u16, 0u32, Vec::new());
    let mut p = 12;
    while p + 8 <= d.len() {
        let id = &d[p..p + 4];
        let len = u32::from_le_bytes(d[p + 4..p + 8].try_into().ok()?) as usize;
        let body = d.get(p + 8..(p + 8 + len).min(d.len()))?;
        match id {
            b"fmt " if body.len() >= 16 => {
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
            }
            b"data" => samples = body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
            _ => {}
        }
        p += 8 + len + (len & 1);
    }
    Some((channels, rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let pcm = Pcm { channels: 2, sample_rate: 48000, samples: vec![1, -1, 32767, -32768], loop_range: Some((0, 2)) };
        let bytes = write(&pcm);
        assert_eq!(&bytes[..4], b"RIFF");
        let (c, r, s) = read(&bytes).unwrap();
        assert_eq!((c, r), (2, 48000));
        assert_eq!(s, pcm.samples);
    }
}
