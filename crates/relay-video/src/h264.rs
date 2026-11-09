//! H.264 bitstream helpers: Annex B (start codes) <-> AVCC (length-prefixed)
//! conversion. Platform-neutral so it can be unit-tested anywhere.

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// NAL unit type (low 5 bits of the header byte).
pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|b| b & 0x1f).unwrap_or(0)
}

/// Converts an AVCC access unit (each NAL prefixed by a big-endian length of
/// `len_size` bytes) to Annex B. `prefix` NAL units (SPS/PPS for keyframes)
/// are written first. Returns `None` if the input is malformed.
pub fn avcc_to_annexb(avcc: &[u8], len_size: usize, prefix: &[&[u8]]) -> Option<Vec<u8>> {
    if !(1..=4).contains(&len_size) {
        return None;
    }
    let mut out = Vec::with_capacity(avcc.len() + prefix.iter().map(|p| p.len() + 4).sum::<usize>() + 16);
    for p in prefix {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(p);
    }
    let mut i = 0;
    while i < avcc.len() {
        let hdr = avcc.get(i..i + len_size)?;
        let len = hdr.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize);
        i += len_size;
        let nal = avcc.get(i..i + len)?;
        i += len;
        if nal.is_empty() {
            continue;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
    }
    Some(out)
}

/// Splits an Annex B stream into NAL units (without start codes). Accepts
/// 3- and 4-byte start codes. Trailing zero bytes of a NAL are dropped.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                push_trimmed(&mut nals, &data[s..i]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push_trimmed(&mut nals, &data[s..]);
    }
    nals
}

fn push_trimmed<'a>(nals: &mut Vec<&'a [u8]>, mut nal: &'a [u8]) {
    while let Some((&0, rest)) = nal.split_last() {
        nal = rest;
    }
    if !nal.is_empty() {
        nals.push(nal);
    }
}

/// Builds an AVCC (4-byte length) payload from NAL units.
pub fn to_avcc<'a>(nals: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nals {
        out.extend_from_slice(&(n.len() as u32).to_be_bytes());
        out.extend_from_slice(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9];
    const PPS: &[u8] = &[0x68, 0xeb, 0xe3, 0xcb];
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x01, 0xff];
    const P: &[u8] = &[0x41, 0x9a, 0x02];

    #[test]
    fn avcc_to_annexb_with_param_sets() {
        let avcc = to_avcc([IDR]);
        let out = avcc_to_annexb(&avcc, 4, &[SPS, PPS]).unwrap();
        let mut want = vec![0, 0, 0, 1];
        want.extend_from_slice(SPS);
        want.extend_from_slice(&[0, 0, 0, 1]);
        want.extend_from_slice(PPS);
        want.extend_from_slice(&[0, 0, 0, 1]);
        want.extend_from_slice(IDR);
        assert_eq!(out, want);
        let nals = split_annexb(&out);
        assert_eq!(nals, vec![SPS, PPS, IDR]);
        assert_eq!(nals.iter().map(|n| nal_type(n)).collect::<Vec<_>>(), vec![NAL_SPS, NAL_PPS, NAL_IDR]);
    }

    #[test]
    fn multiple_nals_and_short_lengths() {
        // 2-byte length prefixes, SEI + slice.
        let sei: &[u8] = &[0x06, 0x05, 0x01, 0x80];
        let mut avcc = Vec::new();
        for n in [sei, P] {
            avcc.extend_from_slice(&(n.len() as u16).to_be_bytes());
            avcc.extend_from_slice(n);
        }
        let out = avcc_to_annexb(&avcc, 2, &[]).unwrap();
        assert_eq!(split_annexb(&out), vec![sei, P]);
    }

    #[test]
    fn malformed_avcc_is_rejected() {
        assert!(avcc_to_annexb(&[0, 0, 0, 9, 0x41], 4, &[]).is_none());
        assert!(avcc_to_annexb(&[0, 0], 4, &[]).is_none());
        assert!(avcc_to_annexb(&[], 5, &[]).is_none());
        assert_eq!(avcc_to_annexb(&[], 4, &[]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn split_handles_three_byte_codes_and_trailing_zeros() {
        let data = [0, 0, 1, 0x67, 0x42, 0, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x88, 0x00];
        assert_eq!(split_annexb(&data), vec![&[0x67, 0x42][..], &[0x68, 0xce][..], &[0x65, 0x88][..]]);
        assert!(split_annexb(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn round_trip() {
        let annexb = avcc_to_annexb(&to_avcc([SPS, PPS, IDR, P]), 4, &[]).unwrap();
        let back = to_avcc(split_annexb(&annexb));
        assert_eq!(back, to_avcc([SPS, PPS, IDR, P]));
    }
}
