//! HCA decoder (docs/formats/audio-acb-awb-hca.md §4), ported from the public descriptions of the
//! format (VGAudio / vgmstream `clHCA`): header chunks, scale factors, resolutions, quantized spectra,
//! v3.0 noise substitution, high-frequency reconstruction, intensity stereo and the 128-band IMDCT.
//!
//! Supported: versions 1.3 – 3.0 with `comp` or `dec` chunks, `ciph` type 0 (or no `ciph` chunk).
//! Encrypted payloads (`ciph` 1 / 56) are rejected: the game ships none.
//!
//! v3.0 differences handled here (clHCA): resolution 0 past curve position 65 (noise bands when `min_res`
//! is 0 — VGAudio 2.2.1 clamps them to 1 and mis-parses every such frame), noise substitution, HFR
//! scales coded after the scale factors, delta-coded intensity and the HFR source band that stops
//! walking down after half the groups.

use serde::Serialize;

use crate::{Error, Result};

const SUBFRAMES: usize = 8;
const SAMPLES_PER_SUBFRAME: usize = 128;
pub const SAMPLES_PER_FRAME: usize = SUBFRAMES * SAMPLES_PER_SUBFRAME;
const MAX_CHANNELS: usize = 16;

// ---------------------------------------------------------------- tables

/// Scale factor → dequantizer gain: `2^((i - 63) * 53/128 + 3.5)`.
pub(crate) fn dequantizer_scaling(i: usize) -> f32 {
    2f64.powf((i as f64 - 63.0) * 53.0 / 128.0 + 3.5) as f32
}

/// Resolution → quantization step: 0, 2/3, 2/5 … 2/15, 2/31, 2/63 … 2/4095.
pub(crate) fn dequantizer_range(r: usize) -> f32 {
    match r {
        0 => 0.0,
        1..=7 => 2.0 / (2 * r + 1) as f32,
        _ => 2.0 / ((1u32 << (r - 3)) - 1) as f32,
    }
}

/// Scale factor difference (+64) → gain ratio: `2^((i - 64) * 53/128)`, 0 at the ends.
fn scale_conversion(i: usize) -> f32 {
    if !(2..=126).contains(&i) {
        0.0
    } else {
        2f64.powf((i as f64 - 64.0) * 53.0 / 128.0) as f32
    }
}

/// Intensity index → left ratio `(14 - i) / 7` (right = left - 2).
fn intensity_ratio(i: usize) -> f32 {
    (14 - i.min(14)) as f32 / 7.0
}

/// Curve position → resolution (bits class), VGAudio `ScaleToResolutionCurve`.
pub(crate) const SCALE_TO_RESOLUTION: [u8; 59] = [
    15, 14, 14, 14, 14, 14, 14, 13, 13, 13, 13, 13, 13, 12, 12, 12, 12, 12, 12, 11, 11, 11, 11, 11, 11, 10, 10, 10, 10, 10, 10, 10, 9, 9, 9, 9, 9, 9, 8, 8, 8, 8, 8, 8, 7, 6, 6, 5, 4, 4,
    4, 3, 3, 3, 2, 2, 2, 2, 1,
];

/// Last table position (in `SCALE_TO_RESOLUTION` coordinates) that still gives resolution 1: clHCA's
/// `hcadecoder_invert_table` has 66 entries (curve positions 0..=65, the last 9 being 1) and returns 0 past
/// them. Positions 58..=66 here give 1 (the table's last entry, clamped).
pub(crate) const RESOLUTION_ZERO_FROM: i32 = 66;

/// Bits read for a quantized value of each resolution (resolutions 1–7 use the prefix codes below).
pub(crate) const MAX_BITS: [u8; 16] = [0, 2, 3, 3, 4, 4, 4, 4, 5, 6, 7, 8, 9, 10, 11, 12];

/// Resolutions 1–7: code length (bits actually consumed) and value, indexed by the `MAX_BITS` peeked bits.
pub(crate) const CODE_BITS: [[u8; 16]; 8] = [
    [0; 16],
    [1, 1, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [2, 2, 2, 2, 2, 2, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0],
    [2, 2, 3, 3, 3, 3, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0],
    [3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4],
    [3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4],
    [3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4],
    [3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4],
];
pub(crate) const CODE_VALUE: [[f32; 16]; 8] = [
    [0.0; 16],
    [0.0, 0.0, 1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 1.0, -1.0, -1.0, 2.0, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 1.0, -1.0, -1.0, 2.0, 2.0, -2.0, -2.0, 3.0, 3.0, -3.0, -3.0, 4.0, -4.0],
    [0.0, 0.0, 1.0, 1.0, -1.0, -1.0, 2.0, 2.0, -2.0, -2.0, 3.0, -3.0, 4.0, -4.0, 5.0, -5.0],
    [0.0, 0.0, 1.0, 1.0, -1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0, 5.0, -5.0, 6.0, -6.0],
    [0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0, 5.0, -5.0, 6.0, -6.0, 7.0, -7.0],
];

/// IMDCT window (first half; the second half is its mirror).
pub(crate) const MDCT_WINDOW: [f32; 128] = [
    0.0006905337795615196, 0.0019762348383665085, 0.0036738645285367966, 0.005724240094423294, 0.008096703328192234, 0.010773181915283203, 0.013742517679929733, 0.01699785701930523,
    0.020535264164209366, 0.024352902546525, 0.02845051884651184, 0.03282909467816353, 0.03749062120914459, 0.04243789613246918, 0.047674428671598434, 0.05320430174469948,
    0.05903211236000061, 0.06516288220882416, 0.07160200923681259, 0.07835522294044495, 0.08542849123477936, 0.09282802045345306, 0.10056015104055405, 0.10863135010004044,
    0.11704812198877335, 0.12581698596477509, 0.134944349527359, 0.14443650841712952, 0.1542995125055313, 0.1645391285419464, 0.1751607209444046, 0.18616916239261627,
    0.19756872951984406, 0.2093629688024521, 0.22155462205410004, 0.2341454178094864, 0.24713599681854248, 0.26052576303482056, 0.27431270480155945, 0.28849318623542786,
    0.30306193232536316, 0.31801173090934753, 0.3333333432674408, 0.3490152955055237, 0.3650438189506531, 0.3814027011394501, 0.39807310700416565, 0.4150335192680359,
    0.43225979804992676, 0.44972503185272217, 0.46739956736564636, 0.48525115847587585, 0.503244936466217, 0.5213438272476196, 0.5395085215568542, 0.5576977729797363,
    0.5758689045906067, 0.5939780473709106, 0.6119805574417114, 0.6298314332962036, 0.6474860310554504, 0.6649002432823181, 0.6820311546325684, 0.6988375782966614,
    0.7152804136276245, 0.7313231229782104, 0.7469321489334106, 0.7620773315429688, 0.7767318487167358, 0.7908728122711182, 0.8044812679290771, 0.8175420165061951,
    0.8300440907478333, 0.8419801592826843, 0.8533467054367065, 0.8641437888145447, 0.8743748068809509, 0.884046196937561, 0.8931670784950256, 0.9017491340637207,
    0.9098061323165894, 0.9173536896705627, 0.9244089722633362, 0.9309903383255005, 0.9371170401573181, 0.9428090453147888, 0.9480867981910706, 0.9529708623886108,
    0.9574819207191467, 0.9616405367851257, 0.9654669165611267, 0.9689807891845703, 0.9722015857696533, 0.9751479625701904, 0.9778379797935486, 0.9802890419960022,
    0.9825177192687988, 0.9845398664474487, 0.9863705635070801, 0.988024115562439, 0.9895140528678894, 0.9908531904220581, 0.9920534491539001, 0.9931262731552124,
    0.9940820932388306, 0.9949309825897217, 0.9956821799278259, 0.9963443279266357, 0.9969255328178406, 0.9974333047866821, 0.9978746175765991, 0.9982560873031616,
    0.9985836744308472, 0.9988629221916199, 0.9990991353988647, 0.9992969632148743, 0.9994609951972961, 0.9995952248573303, 0.9997034072875977, 0.9997891187667847,
    0.9998555183410645, 0.9999055862426758, 0.9999419450759888, 0.9999672174453735, 0.9999836087226868, 0.9999932646751404, 0.9999980330467224, 0.9999997615814209,
];

/// Absolute-threshold-of-hearing base curve (`ath` type 1), sampled by frequency.
const ATH_CURVE: [u8; 654] = [
    120, 95, 86, 81, 78, 76, 75, 73, 72, 72, 71, 70, 70, 69, 69, 69, 68, 68, 68, 68, 67, 67, 67, 67, 67, 67, 66, 66, 66, 66, 66, 66, 66, 66, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 64, 64, 64, 64, 64, 64, 64, 64, 64, 63, 63, 63, 63, 63, 63,
    63, 63, 63, 63, 63, 63, 63, 63, 62, 62, 62, 62, 62, 62, 61, 61, 61, 61, 61, 61, 61, 60, 60, 60, 60, 60, 60, 60, 60, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59, 59,
    59, 59, 60, 60, 60, 60, 60, 60, 60, 60, 61, 61, 61, 61, 61, 61, 61, 61, 62, 62, 62, 62, 62, 62, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 65, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66, 66,
    66, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 67, 68, 68, 68, 68, 68, 68, 68, 68, 68, 68, 68, 68, 68, 68, 69, 69, 69, 69, 69, 69, 69, 69, 69, 69, 69, 69, 70, 70, 70, 70, 70, 70, 70, 70, 70, 70, 71, 71, 71, 71, 71,
    71, 71, 71, 71, 71, 72, 72, 72, 72, 72, 72, 72, 72, 73, 73, 73, 73, 73, 73, 73, 73, 74, 74, 74, 74, 74, 74, 74, 74, 75, 75, 75, 75, 75, 75, 75, 76, 76, 76, 76, 76, 76, 77, 77, 77, 77, 77, 77, 78, 78, 78, 78, 78, 78, 79, 79, 79, 79, 79,
    79, 80, 80, 80, 80, 80, 81, 81, 81, 81, 81, 82, 82, 82, 82, 82, 83, 83, 83, 83, 84, 84, 84, 84, 84, 85, 85, 85, 85, 86, 86, 86, 86, 87, 87, 87, 87, 87, 88, 88, 88, 89, 89, 89, 89, 90, 90, 90, 90, 91, 91, 91, 91, 92, 92, 92, 93, 93, 93,
    93, 94, 94, 94, 95, 95, 95, 96, 96, 96, 97, 97, 97, 97, 98, 98, 98, 99, 99, 99, 100, 100, 100, 101, 101, 102, 102, 102, 103, 103, 103, 104, 104, 104, 105, 105, 106, 106, 106, 107, 107, 107, 108, 108, 109, 109, 109, 110, 110, 111, 111,
    112, 112, 112, 113, 113, 114, 114, 115, 115, 115, 116, 116, 117, 117, 118, 118, 119, 119, 120, 120, 120, 121, 121, 122, 122, 123, 123, 124, 124, 125, 125, 126, 126, 127, 127, 128, 128, 129, 129, 130, 131, 131, 132, 132, 133, 133, 134,
    134, 135, 136, 136, 137, 137, 138, 138, 139, 140, 140, 141, 141, 142, 143, 143, 144, 144, 145, 146, 146, 147, 148, 148, 149, 149, 150, 151, 151, 152, 153, 153, 154, 155, 155, 156, 157, 157, 158, 159, 160, 160, 161, 162, 162, 163, 164,
    165, 165, 166, 167, 167, 168, 169, 170, 170, 171, 172, 173, 174, 174, 175, 176, 177, 177, 178, 179, 180, 181, 182, 182, 183, 184, 185, 186, 186, 187, 188, 189, 190, 191, 192, 193, 193, 194, 195, 196, 197, 198, 199, 200, 201, 201, 202,
    203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233, 234, 235, 237, 238, 239, 240, 241, 242, 243, 244, 245, 247, 248, 249, 250, 251,
    252, 253,
];

// ---------------------------------------------------------------- header

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Loop {
    pub start_frame: u32,
    pub end_frame: u32,
    /// Samples to skip at the start of the loop start frame ("pre-loop samples").
    pub start_delay: u16,
    /// Samples to drop at the end of the loop end frame ("post-loop samples").
    pub end_padding: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    /// `0x0300` for 3.0.
    pub version: u16,
    pub header_size: u16,
    pub channels: u8,
    pub sample_rate: u32,
    pub frame_count: u32,
    pub encoder_delay: u16,
    pub end_padding: u16,
    pub frame_size: u16,
    pub min_resolution: u8,
    pub max_resolution: u8,
    pub track_count: u8,
    pub channel_config: u8,
    pub total_band_count: u8,
    pub base_band_count: u8,
    pub stereo_band_count: u8,
    pub bands_per_hfr_group: u8,
    pub ms_stereo: u8,
    pub ath_type: u16,
    pub cipher_type: u16,
    #[serde(rename = "loop")]
    pub loop_info: Option<Loop>,
    pub volume: f32,
    pub comment: String,
    /// Whether the header CRC-16 matched.
    pub crc_ok: bool,
}

impl Header {
    pub fn is_hca(d: &[u8]) -> bool {
        d.len() >= 8 && d[0] & 0x7F == b'H' && d[1] & 0x7F == b'C' && d[2] & 0x7F == b'A' && d[3] & 0x7F == 0
    }

    /// Total samples after trimming delay and padding.
    pub fn sample_count(&self) -> u64 {
        (self.frame_count as u64 * SAMPLES_PER_FRAME as u64).saturating_sub(self.encoder_delay as u64 + self.end_padding as u64)
    }

    /// Loop points in trimmed samples.
    pub fn loop_samples(&self) -> Option<(u64, u64)> {
        let l = self.loop_info?;
        let start = (l.start_frame as u64 * SAMPLES_PER_FRAME as u64 + l.start_delay as u64).saturating_sub(self.encoder_delay as u64);
        let end = ((l.end_frame as u64 + 1) * SAMPLES_PER_FRAME as u64).saturating_sub(l.end_padding as u64 + self.encoder_delay as u64);
        Some((start, end))
    }

    pub fn hfr_group_count(&self) -> usize {
        if self.bands_per_hfr_group == 0 {
            0
        } else {
            (self.total_band_count as usize).saturating_sub(self.base_band_count as usize + self.stereo_band_count as usize).div_ceil(self.bands_per_hfr_group as usize)
        }
    }

    pub fn parse(d: &[u8]) -> Result<Header> {
        if !Self::is_hca(d) {
            return Err(Error::Hca("missing HCA magic".into()));
        }
        let version = u16::from_be_bytes([d[4], d[5]]);
        let header_size = u16::from_be_bytes([d[6], d[7]]);
        let hs = header_size as usize;
        if hs < 8 || d.len() < hs {
            return Err(Error::Hca(format!("header size {hs} exceeds the {} bytes given", d.len())));
        }
        let crc_ok = hs >= 10 && crc16(&d[..hs - 2]) == u16::from_be_bytes([d[hs - 2], d[hs - 1]]);
        let mut h = Header {
            version,
            header_size,
            channels: 0,
            sample_rate: 0,
            frame_count: 0,
            encoder_delay: 0,
            end_padding: 0,
            frame_size: 0,
            min_resolution: 1,
            max_resolution: 15,
            track_count: 1,
            channel_config: 0,
            total_band_count: 0,
            base_band_count: 0,
            stereo_band_count: 0,
            bands_per_hfr_group: 0,
            ms_stereo: 0,
            ath_type: if version < 0x200 { 1 } else { 0 },
            cipher_type: 0,
            loop_info: None,
            volume: 1.0,
            comment: String::new(),
            crc_ok,
        };
        let mut p = 8;
        let mut have_fmt = false;
        let mut have_comp = false;
        let tag = |b: &[u8]| -> [u8; 4] { [b[0] & 0x7F, b[1] & 0x7F, b[2] & 0x7F, b[3] & 0x7F] };
        while p + 4 <= hs - 2 {
            let t = tag(&d[p..p + 4]);
            let need = |n: usize| -> Result<()> { (p + 4 + n <= hs).then_some(()).ok_or_else(|| Error::Hca(format!("chunk {} truncated", String::from_utf8_lossy(&t)))) };
            match &t {
                b"fmt\0" => {
                    need(12)?;
                    h.channels = d[p + 4];
                    h.sample_rate = u32::from_be_bytes([0, d[p + 5], d[p + 6], d[p + 7]]);
                    h.frame_count = u32::from_be_bytes([d[p + 8], d[p + 9], d[p + 10], d[p + 11]]);
                    h.encoder_delay = u16::from_be_bytes([d[p + 12], d[p + 13]]);
                    h.end_padding = u16::from_be_bytes([d[p + 14], d[p + 15]]);
                    have_fmt = true;
                    p += 16;
                }
                b"comp" => {
                    need(12)?;
                    h.frame_size = u16::from_be_bytes([d[p + 4], d[p + 5]]);
                    h.min_resolution = d[p + 6];
                    h.max_resolution = d[p + 7];
                    h.track_count = d[p + 8];
                    h.channel_config = d[p + 9];
                    h.total_band_count = d[p + 10];
                    h.base_band_count = d[p + 11];
                    h.stereo_band_count = d[p + 12];
                    h.bands_per_hfr_group = d[p + 13];
                    h.ms_stereo = d[p + 14];
                    have_comp = true;
                    p += 16;
                }
                b"dec\0" => {
                    need(8)?;
                    h.frame_size = u16::from_be_bytes([d[p + 4], d[p + 5]]);
                    h.min_resolution = d[p + 6];
                    h.max_resolution = d[p + 7];
                    h.total_band_count = d[p + 8].wrapping_add(1);
                    h.base_band_count = d[p + 9].wrapping_add(1);
                    h.track_count = d[p + 10] >> 4;
                    h.channel_config = d[p + 10] & 0xF;
                    let stereo_type = d[p + 11];
                    if stereo_type == 0 {
                        h.base_band_count = h.total_band_count;
                    }
                    h.stereo_band_count = h.total_band_count.saturating_sub(h.base_band_count);
                    h.bands_per_hfr_group = 0;
                    have_comp = true;
                    p += 12;
                }
                b"vbr\0" => {
                    need(4)?;
                    p += 8;
                }
                b"ath\0" => {
                    need(2)?;
                    h.ath_type = u16::from_be_bytes([d[p + 4], d[p + 5]]);
                    p += 6;
                }
                b"loop" => {
                    need(12)?;
                    h.loop_info = Some(Loop {
                        start_frame: u32::from_be_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]),
                        end_frame: u32::from_be_bytes([d[p + 8], d[p + 9], d[p + 10], d[p + 11]]),
                        start_delay: u16::from_be_bytes([d[p + 12], d[p + 13]]),
                        end_padding: u16::from_be_bytes([d[p + 14], d[p + 15]]),
                    });
                    p += 16;
                }
                b"ciph" => {
                    need(2)?;
                    h.cipher_type = u16::from_be_bytes([d[p + 4], d[p + 5]]);
                    p += 6;
                }
                b"rva\0" => {
                    need(4)?;
                    h.volume = f32::from_be_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]);
                    p += 8;
                }
                b"comm" => {
                    need(1)?;
                    let n = d[p + 4] as usize;
                    need(1 + n)?;
                    h.comment = String::from_utf8_lossy(&d[p + 5..p + 5 + n]).trim_end_matches('\0').to_string();
                    p += 5 + n;
                }
                b"pad\0" => break,
                _ => break,
            }
        }
        if !have_fmt || !have_comp {
            return Err(Error::Hca("missing fmt / comp chunk".into()));
        }
        if h.channels == 0 || h.channels as usize > MAX_CHANNELS || h.frame_size == 0 || h.sample_rate == 0 {
            return Err(Error::Hca(format!("unsupported layout: {} ch, frame {} B, {} Hz", h.channels, h.frame_size, h.sample_rate)));
        }
        if h.cipher_type != 0 {
            return Err(Error::Hca(format!("encrypted payload (ciph type {})", h.cipher_type)));
        }
        if h.total_band_count as usize > SAMPLES_PER_SUBFRAME || h.base_band_count as usize + h.stereo_band_count as usize > SAMPLES_PER_SUBFRAME {
            return Err(Error::Hca("band counts out of range".into()));
        }
        if h.track_count == 0 {
            h.track_count = 1;
        }
        Ok(h)
    }
}

/// CRC-16 (poly 0x8005, init 0, no reflection) of HCA headers and frames.
pub fn crc16(d: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in d {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

// ---------------------------------------------------------------- bit reader

struct BitReader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(d: &'a [u8]) -> Self {
        BitReader { d, pos: 0 }
    }

    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let mut v: u64 = 0;
        for i in 0..4 {
            v = (v << 8) | *self.d.get(byte + i).unwrap_or(&0) as u64;
        }
        let shift = 32 - (self.pos % 8) as u32 - n;
        ((v >> shift) & ((1u64 << n) - 1)) as u32
    }

    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }
}

// ---------------------------------------------------------------- decoder

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelType {
    Discrete,
    StereoPrimary,
    StereoSecondary,
}

struct Channel {
    ty: ChannelType,
    coded_count: usize,
    scalefactors: [u8; SAMPLES_PER_SUBFRAME],
    intensity: [u8; SUBFRAMES],
    hfr_scales: [u8; SAMPLES_PER_SUBFRAME],
    resolution: [u8; SAMPLES_PER_SUBFRAME],
    gain: [f32; SAMPLES_PER_SUBFRAME],
    /// Band indices: noise bands (resolution 0) from the front, valid bands from the back.
    noises: [u8; SAMPLES_PER_SUBFRAME],
    noise_count: usize,
    valid_count: usize,
    spectra: [[f32; SAMPLES_PER_SUBFRAME]; SUBFRAMES],
    imdct_prev: [f32; SAMPLES_PER_SUBFRAME],
    wave: [f32; SAMPLES_PER_FRAME],
}

impl Channel {
    fn new(ty: ChannelType, coded_count: usize) -> Self {
        Channel {
            ty,
            coded_count,
            scalefactors: [0; SAMPLES_PER_SUBFRAME],
            intensity: [0; SUBFRAMES],
            hfr_scales: [0; SAMPLES_PER_SUBFRAME],
            resolution: [0; SAMPLES_PER_SUBFRAME],
            gain: [0.0; SAMPLES_PER_SUBFRAME],
            noises: [0; SAMPLES_PER_SUBFRAME],
            noise_count: 0,
            valid_count: 0,
            spectra: [[0.0; SAMPLES_PER_SUBFRAME]; SUBFRAMES],
            imdct_prev: [0.0; SAMPLES_PER_SUBFRAME],
            wave: [0.0; SAMPLES_PER_FRAME],
        }
    }
}

/// 64-point complex FFT twiddles + DCT-IV pre/post twiddles for the 128-band IMDCT.
pub(crate) struct Transform {
    fft_tw: Vec<(f32, f32)>,
    pre: [(f32, f32); 64],
    post: [(f32, f32); 64],
    bitrev: [u8; 64],
}

impl Transform {
    pub(crate) fn new() -> Self {
        let n = 128.0f64;
        let mut pre = [(0.0f32, 0.0f32); 64];
        let mut post = [(0.0f32, 0.0f32); 64];
        for m in 0..64 {
            let a = -std::f64::consts::PI * (4.0 * m as f64 + 1.0) / (4.0 * n);
            pre[m] = (a.cos() as f32, a.sin() as f32);
            let b = -std::f64::consts::PI * m as f64 / n;
            post[m] = (b.cos() as f32, b.sin() as f32);
        }
        let mut fft_tw = Vec::with_capacity(32);
        for k in 0..32 {
            let a = -2.0 * std::f64::consts::PI * k as f64 / 64.0;
            fft_tw.push((a.cos() as f32, a.sin() as f32));
        }
        let mut bitrev = [0u8; 64];
        for (i, b) in bitrev.iter_mut().enumerate() {
            *b = (i as u8).reverse_bits() >> 2;
        }
        Transform { fft_tw, pre, post, bitrev }
    }

    /// DCT-IV of 128 points scaled by sqrt(2/128): `c[k] = 0.125 · Σ x[n] cos(π/128 (n+½)(k+½))`.
    pub(crate) fn dct4(&self, x: &[f32; 128], c: &mut [f32; 128]) {
        let mut re = [0f32; 64];
        let mut im = [0f32; 64];
        for m in 0..64 {
            let (zr, zi) = (x[2 * m], x[127 - 2 * m]);
            let (tr, ti) = self.pre[m];
            let j = self.bitrev[m] as usize;
            re[j] = zr * tr - zi * ti;
            im[j] = zr * ti + zi * tr;
        }
        let mut len = 2;
        while len <= 64 {
            let half = len / 2;
            let step = 64 / len;
            let mut start = 0;
            while start < 64 {
                for k in 0..half {
                    let (wr, wi) = self.fft_tw[k * step];
                    let (a, b) = (start + k, start + k + half);
                    let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
                start += len;
            }
            len *= 2;
        }
        for p in 0..64 {
            let (wr, wi) = self.post[p];
            let qr = (re[p] * wr - im[p] * wi) * 0.125;
            let qi = (re[p] * wi + im[p] * wr) * 0.125;
            c[2 * p] = qr;
            c[127 - 2 * p] = -qi;
        }
    }
}

/// Decoded audio: interleaved 16-bit samples.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub channels: u16,
    pub sample_rate: u32,
    /// Interleaved samples (`frames * channels`).
    pub samples: Vec<i16>,
    /// Loop points in sample frames, from the HCA `loop` chunk.
    pub loop_range: Option<(u64, u64)>,
}

impl Pcm {
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// Stateful decoder: `decode_frame` one frame at a time, in order (the IMDCT overlaps frames).
pub struct Decoder {
    pub header: Header,
    channels: Vec<Channel>,
    ath: [u8; SAMPLES_PER_SUBFRAME],
    hfr_group_count: usize,
    random: i32,
    transform: Transform,
    /// Frames whose CRC-16 did not match (decoded anyway).
    pub bad_crc_frames: usize,
    /// Frames whose bitstream read past the payload (the frame minus its CRC): a desynchronized parse.
    pub overrun_frames: usize,
}

impl Decoder {
    pub fn new(header: Header) -> Result<Decoder> {
        let n = header.channels as usize;
        let per_track = n / header.track_count.max(1) as usize;
        let mut types = vec![ChannelType::Discrete; n];
        if header.stereo_band_count > 0 && per_track > 1 {
            for t in 0..header.track_count as usize {
                let c = &mut types[t * per_track..((t + 1) * per_track).min(n)];
                let pair = |c: &mut [ChannelType], i: usize| {
                    if i + 1 < c.len() {
                        c[i] = ChannelType::StereoPrimary;
                        c[i + 1] = ChannelType::StereoSecondary;
                    }
                };
                match per_track {
                    2 | 3 => pair(c, 0),
                    4 => {
                        pair(c, 0);
                        if header.channel_config == 0 {
                            pair(c, 2);
                        }
                    }
                    5 => {
                        pair(c, 0);
                        if header.channel_config <= 2 {
                            pair(c, 3);
                        }
                    }
                    6 | 7 => {
                        pair(c, 0);
                        pair(c, 4);
                    }
                    8 => {
                        pair(c, 0);
                        pair(c, 4);
                        pair(c, 6);
                    }
                    _ => {}
                }
            }
        }
        let base = header.base_band_count as usize;
        let stereo = header.stereo_band_count as usize;
        let channels = types.iter().map(|&ty| Channel::new(ty, if ty == ChannelType::StereoSecondary { base } else { base + stereo })).collect();
        let mut ath = [0u8; SAMPLES_PER_SUBFRAME];
        match header.ath_type {
            0 => {}
            1 => {
                let mut acc: u32 = 0;
                let mut i = 0;
                while i < SAMPLES_PER_SUBFRAME {
                    acc = acc.wrapping_add(header.sample_rate);
                    let idx = (acc >> 13) as usize;
                    if idx >= ATH_CURVE.len() {
                        break;
                    }
                    ath[i] = ATH_CURVE[idx];
                    i += 1;
                }
                for a in ath.iter_mut().skip(i) {
                    *a = 0xFF;
                }
            }
            other => return Err(Error::Hca(format!("unsupported ath type {other}"))),
        }
        Ok(Decoder { hfr_group_count: header.hfr_group_count(), header, channels, ath, random: 1, transform: Transform::new(), bad_crc_frames: 0, overrun_frames: 0 })
    }

    /// Decode one frame (`header.frame_size` bytes). Returns the per-channel float samples (1024 each).
    pub fn decode_frame(&mut self, frame: &[u8]) -> Result<Vec<&[f32]>> {
        let fs = self.header.frame_size as usize;
        if frame.len() < fs {
            return Err(Error::Hca(format!("frame needs {fs} bytes, got {}", frame.len())));
        }
        let frame = &frame[..fs];
        if frame[0] != 0xFF || frame[1] != 0xFF {
            return Err(Error::Hca("bad frame sync".into()));
        }
        if crc16(&frame[..fs - 2]) != u16::from_be_bytes([frame[fs - 2], frame[fs - 1]]) {
            self.bad_crc_frames += 1;
        }
        let mut br = BitReader::new(frame);
        br.skip(16);
        let noise_level = br.read(9) as i32;
        let evaluation_boundary = br.read(7) as i32;
        let packed_noise_level = (noise_level << 8) - evaluation_boundary;
        let version = self.header.version;
        let (min_res, max_res) = (self.header.min_resolution, self.header.max_resolution);
        let hfr = self.hfr_group_count;

        for ch in &mut self.channels {
            unpack_scalefactors(ch, &mut br, hfr, version)?;
            unpack_intensity(ch, &mut br, hfr, version)?;
            calculate_resolution(ch, packed_noise_level, &self.ath, min_res, max_res);
            for i in 0..ch.coded_count {
                ch.gain[i] = dequantizer_scaling(ch.scalefactors[i] as usize) * dequantizer_range(ch.resolution[i] as usize);
            }
        }

        let h = &self.header;
        let (base, stereo, total) = (h.base_band_count as usize, h.stereo_band_count as usize, h.total_band_count as usize);
        let (bands_per_group, ms_stereo) = (h.bands_per_hfr_group as usize, h.ms_stereo);
        for sf in 0..SUBFRAMES {
            for ch in &mut self.channels {
                dequantize(ch, &mut br, sf);
            }
            for ch in &mut self.channels {
                reconstruct_noise(ch, sf, min_res, ms_stereo, &mut self.random);
            }
            for ch in &mut self.channels {
                reconstruct_high_frequency(ch, sf, hfr, bands_per_group, stereo, base, total, version);
            }
            for i in 0..self.channels.len().saturating_sub(1) {
                if self.channels[i].ty == ChannelType::StereoPrimary && self.channels[i + 1].ty == ChannelType::StereoSecondary {
                    let (a, b) = self.channels.split_at_mut(i + 1);
                    apply_intensity_stereo(&mut a[i], &mut b[0], sf, base, total);
                }
            }
            for ch in &mut self.channels {
                imdct(ch, sf, &self.transform);
            }
        }
        if br.pos > (fs - 2) * 8 {
            self.overrun_frames += 1;
        }
        Ok(self.channels.iter().map(|c| &c.wave[..]).collect())
    }
}

fn unpack_scalefactors(ch: &mut Channel, br: &mut BitReader, hfr_group_count: usize, version: u16) -> Result<()> {
    let delta_bits = br.read(3);
    let extra = if ch.ty == ChannelType::StereoSecondary || hfr_group_count == 0 || version <= 0x200 { 0 } else { hfr_group_count };
    let count = ch.coded_count + extra;
    if count > SAMPLES_PER_SUBFRAME {
        return Err(Error::Hca("too many scale factors".into()));
    }
    if delta_bits >= 6 {
        for i in 0..count {
            ch.scalefactors[i] = br.read(6) as u8;
        }
    } else if delta_bits > 0 {
        let expected = (1u32 << delta_bits) - 1;
        let mut value = br.read(6);
        ch.scalefactors[0] = value as u8;
        for i in 1..count {
            let delta = br.read(delta_bits);
            if delta == expected {
                value = br.read(6);
            } else {
                let test = value as i32 + delta as i32 - (expected >> 1) as i32;
                if !(0..64).contains(&test) {
                    return Err(Error::Hca("scale factor out of range".into()));
                }
                value = test as u32;
            }
            ch.scalefactors[i] = value as u8;
        }
    } else {
        ch.scalefactors = [0; SAMPLES_PER_SUBFRAME];
    }
    // v3.0: the HFR group scales follow the coded ones in the stream.
    for g in 0..extra {
        ch.hfr_scales[g] = ch.scalefactors[ch.coded_count + g];
    }
    Ok(())
}

fn unpack_intensity(ch: &mut Channel, br: &mut BitReader, hfr_group_count: usize, version: u16) -> Result<()> {
    if ch.ty == ChannelType::StereoSecondary {
        if version <= 0x200 {
            let value = br.peek(4) as u8;
            ch.intensity[0] = value;
            if value < 15 {
                br.skip(4);
                for i in 1..SUBFRAMES {
                    ch.intensity[i] = br.read(4) as u8;
                }
            }
        } else {
            let value = br.peek(4);
            if value < 15 {
                br.skip(4);
                let delta_bits = br.read(2);
                ch.intensity[0] = value as u8;
                if delta_bits == 3 {
                    for i in 1..SUBFRAMES {
                        ch.intensity[i] = br.read(4) as u8;
                    }
                } else {
                    let bmax = (2u32 << delta_bits) - 1;
                    let bits = delta_bits + 1;
                    let mut v = value;
                    for i in 1..SUBFRAMES {
                        let delta = br.read(bits);
                        if delta == bmax {
                            v = br.read(4);
                        } else {
                            let t = v as i32 - (bmax >> 1) as i32 + delta as i32;
                            if !(0..16).contains(&t) {
                                return Err(Error::Hca("intensity out of range".into()));
                            }
                            v = t as u32;
                        }
                        ch.intensity[i] = v as u8;
                    }
                }
            } else {
                br.skip(4);
                ch.intensity = [7; SUBFRAMES];
            }
        }
    } else if hfr_group_count > 0 && version <= 0x200 {
        for g in 0..hfr_group_count.min(SAMPLES_PER_SUBFRAME) {
            ch.hfr_scales[g] = br.read(6) as u8;
        }
    }
    Ok(())
}

fn calculate_resolution(ch: &mut Channel, packed_noise_level: i32, ath: &[u8; SAMPLES_PER_SUBFRAME], min_res: u8, max_res: u8) {
    let (mut noise_count, mut valid_count) = (0usize, 0usize);
    for i in 0..ch.coded_count {
        let sf = ch.scalefactors[i] as i32;
        let mut res = 0u8;
        if sf > 0 {
            let noise = ath[i] as i32 + ((packed_noise_level + i as i32) >> 8);
            // clHCA `curve_position` + 1 (index 0 of the table is the "< 0 → 15" case).
            let pos = noise - ((sf * 5) >> 1) + 2;
            res = if pos > RESOLUTION_ZERO_FROM {
                // Far below the noise floor: no coded value. v3.0 files (`min_res` 0) turn these bands into
                // noise; clamping to the table's last entry (1) instead reads a phantom 1–2-bit code per
                // band and desynchronizes the rest of the frame (VGAudio 2.2.1 has the same bug).
                0
            } else {
                SCALE_TO_RESOLUTION[pos.clamp(0, SCALE_TO_RESOLUTION.len() as i32 - 1) as usize]
            };
            res = res.clamp(min_res, max_res);
            if res < 1 {
                ch.noises[noise_count] = i as u8;
                noise_count += 1;
            } else {
                ch.noises[SAMPLES_PER_SUBFRAME - 1 - valid_count] = i as u8;
                valid_count += 1;
            }
        }
        ch.resolution[i] = res;
    }
    for r in ch.resolution[ch.coded_count..].iter_mut() {
        *r = 0;
    }
    ch.noise_count = noise_count;
    ch.valid_count = valid_count;
}

fn dequantize(ch: &mut Channel, br: &mut BitReader, sf: usize) {
    let sp = &mut ch.spectra[sf];
    for i in 0..ch.coded_count {
        let res = ch.resolution[i] as usize;
        let bits = MAX_BITS[res] as u32;
        let value = if res == 0 {
            0.0
        } else if res < 8 {
            let code = br.peek(bits) as usize;
            br.skip(CODE_BITS[res][code] as u32);
            CODE_VALUE[res][code]
        } else {
            let v = br.read(bits);
            let mag = (v >> 1) as i32;
            let signed = if v & 1 != 0 { -mag } else { mag };
            if mag == 0 {
                br.pos -= 1; // the zero code is one bit shorter
            }
            signed as f32
        };
        sp[i] = value * ch.gain[i];
    }
    for v in sp[ch.coded_count..].iter_mut() {
        *v = 0.0;
    }
}

fn reconstruct_noise(ch: &mut Channel, sf: usize, min_res: u8, ms_stereo: u8, random: &mut i32) {
    if min_res > 0 || ch.valid_count == 0 || ch.noise_count == 0 {
        return;
    }
    if ms_stereo != 0 && ch.ty != ChannelType::StereoPrimary {
        return;
    }
    let mut r = *random;
    for i in 0..ch.noise_count {
        r = r.wrapping_mul(0x343FD).wrapping_add(0x269EC3);
        let random_index = SAMPLES_PER_SUBFRAME - ch.valid_count + ((((r & 0x7FFF) as usize) * ch.valid_count) >> 15);
        let noise_index = ch.noises[i] as usize;
        let valid_index = ch.noises[random_index] as usize;
        let sc = (ch.scalefactors[noise_index] as i32 - ch.scalefactors[valid_index] as i32 + 62).max(0) as usize;
        ch.spectra[sf][noise_index] = scale_conversion(sc) * ch.spectra[sf][valid_index];
    }
    *random = r;
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_high_frequency(ch: &mut Channel, sf: usize, hfr_group_count: usize, bands_per_group: usize, stereo: usize, base: usize, total: usize, version: u16) {
    if bands_per_group == 0 || hfr_group_count == 0 || ch.ty == ChannelType::StereoSecondary {
        return;
    }
    let start = base + stereo;
    let total = total.min(SAMPLES_PER_SUBFRAME - 1);
    // v3.0: the source band only walks down during the first half of the groups, then stays put (clHCA
    // `group_limit`); up to v2.0 it walks down for every group.
    let group_limit = if version <= 0x200 { hfr_group_count } else { hfr_group_count >> 1 };
    let mut high = start;
    let mut low = start.wrapping_sub(1);
    'groups: for g in 0..hfr_group_count {
        let step = usize::from(g < group_limit);
        for _ in 0..bands_per_group {
            if high >= total || low >= SAMPLES_PER_SUBFRAME {
                break 'groups;
            }
            let sc = (ch.hfr_scales[g] as i32 - ch.scalefactors[low] as i32 + 64).max(0) as usize;
            ch.spectra[sf][high] = scale_conversion(sc.min(127)) * ch.spectra[sf][low];
            high += 1;
            low = low.wrapping_sub(step);
        }
    }
    ch.spectra[sf][SAMPLES_PER_SUBFRAME - 1] = 0.0;
}

fn apply_intensity_stereo(primary: &mut Channel, secondary: &mut Channel, sf: usize, base: usize, total: usize) {
    let ratio_l = intensity_ratio(secondary.intensity[sf] as usize);
    let ratio_r = ratio_l - 2.0;
    for i in base..total.min(SAMPLES_PER_SUBFRAME) {
        let v = primary.spectra[sf][i];
        primary.spectra[sf][i] = v * ratio_l;
        secondary.spectra[sf][i] = v * ratio_r;
    }
}

fn imdct(ch: &mut Channel, sf: usize, t: &Transform) {
    let mut c = [0f32; SAMPLES_PER_SUBFRAME];
    t.dct4(&ch.spectra[sf], &mut c);
    let w = &MDCT_WINDOW;
    let out = &mut ch.wave[sf * SAMPLES_PER_SUBFRAME..(sf + 1) * SAMPLES_PER_SUBFRAME];
    let prev = &mut ch.imdct_prev;
    for i in 0..64 {
        out[i] = w[i] * c[i + 64] + prev[i];
        out[i + 64] = w[i + 64] * -c[127 - i] - prev[i + 64];
        prev[i] = w[127 - i] * -c[63 - i];
        prev[i + 64] = w[63 - i] * c[i];
    }
}

/// Decode a whole HCA file to interleaved 16-bit PCM (delay and padding trimmed).
pub fn decode(data: &[u8]) -> Result<Pcm> {
    let header = Header::parse(data)?;
    let mut dec = Decoder::new(header.clone())?;
    let n = header.channels as usize;
    let fs = header.frame_size as usize;
    let total = header.sample_count() as usize;
    let mut samples: Vec<i16> = Vec::with_capacity(total * n);
    let mut produced = 0usize;
    let delay = header.encoder_delay as usize;
    let mut p = header.header_size as usize;
    for _ in 0..header.frame_count {
        let Some(frame) = data.get(p..p + fs) else { break };
        p += fs;
        let waves = dec.decode_frame(frame)?;
        for s in 0..SAMPLES_PER_FRAME {
            let abs = produced + s;
            if abs < delay || abs - delay >= total {
                continue;
            }
            for w in &waves {
                let v = (w[s] * 32767.0).round().clamp(-32768.0, 32767.0);
                samples.push(v as i16);
            }
        }
        produced += SAMPLES_PER_FRAME;
    }
    Ok(Pcm { channels: n as u16, sample_rate: header.sample_rate, samples, loop_range: header.loop_samples() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_matches_known_vector() {
        // CRC-16/BUYPASS ("123456789") = 0xFEE8
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }

    #[test]
    fn tables_match_reference_values() {
        assert!((dequantizer_scaling(0) - 1.588_383_4e-7).abs() < 1e-12);
        assert!((dequantizer_scaling(63) - 11.313_708).abs() < 1e-5);
        assert!((dequantizer_range(1) - 2.0 / 3.0).abs() < 1e-7);
        assert!((dequantizer_range(15) - 2.0 / 4095.0).abs() < 1e-9);
        assert!((scale_conversion(64) - 1.0).abs() < 1e-7);
        assert!((scale_conversion(63) - 0.750_507).abs() < 1e-6);
        assert_eq!(scale_conversion(1), 0.0);
        assert_eq!(scale_conversion(127), 0.0);
        assert!((intensity_ratio(0) - 2.0).abs() < 1e-7);
        assert_eq!(intensity_ratio(14), 0.0);
    }

    #[test]
    fn fast_dct4_matches_naive() {
        let t = Transform::new();
        let mut x = [0f32; 128];
        for (i, v) in x.iter_mut().enumerate() {
            *v = ((i * 7919 % 97) as f32 / 97.0 - 0.5) * 3.0;
        }
        let mut fast = [0f32; 128];
        t.dct4(&x, &mut fast);
        for k in 0..128 {
            let mut acc = 0f64;
            for n in 0..128 {
                acc += x[n] as f64 * (std::f64::consts::PI / 128.0 * (n as f64 + 0.5) * (k as f64 + 0.5)).cos();
            }
            let naive = acc * 0.125;
            assert!((fast[k] as f64 - naive).abs() < 1e-4, "k={k}: fast {} naive {naive}", fast[k]);
        }
    }

    #[test]
    fn resolution_is_zero_past_curve_position_65() {
        // v3.0 (`min_res` 0, `ath` 0): noise level 100 → band 0 (sf 1) sits at clHCA curve position
        // 100 + 1 - 2 = 99 > 65 → resolution 0 (a noise band, no bits); band 1 (sf 30) at 26 → 10;
        // band 2 (sf 17) at 59 → 1 (the clamped tail of the table).
        let mut ch = Channel::new(ChannelType::Discrete, 3);
        ch.scalefactors[..3].copy_from_slice(&[1, 30, 17]);
        let ath = [0u8; SAMPLES_PER_SUBFRAME];
        calculate_resolution(&mut ch, 100 << 8, &ath, 0, 15);
        assert_eq!(&ch.resolution[..3], &[0, 10, 1]);
        assert_eq!((ch.noise_count, ch.valid_count), (1, 2));
        // v2.0 (`min_res` 1): the same band is clamped back up to 1.
        calculate_resolution(&mut ch, 100 << 8, &ath, 1, 15);
        assert_eq!(&ch.resolution[..3], &[1, 10, 1]);
    }

    #[test]
    fn prefix_codes_are_complete() {
        // Every resolution 1..=7 must form a complete prefix code over MAX_BITS bits (Kraft sum == 1).
        for r in 1..8 {
            let bits = MAX_BITS[r] as u32;
            let mut sum = 0f64;
            let mut seen = std::collections::BTreeSet::new();
            for code in 0..(1usize << bits) {
                let len = CODE_BITS[r][code] as u32;
                assert!(len >= 1 && len <= bits);
                let prefix = code >> (bits - len);
                if seen.insert((len, prefix)) {
                    sum += 0.5f64.powi(len as i32);
                }
            }
            assert!((sum - 1.0).abs() < 1e-9, "resolution {r}: Kraft sum {sum}");
        }
    }
}
