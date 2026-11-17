//! Turning encoder output into what goes on the wire: Annex B with 4-byte
//! start codes, and SPS and PPS in front of every keyframe (SPEC.md
//! section 4, step 5).

use opendisplay_proto::video::{NalType, nal_units, normalize_start_codes};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// One encoded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Annex B, 4-byte start codes only. A keyframe starts with SPS and PPS.
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// The most recent SPS and PPS, for encoders that send them only once.
#[derive(Debug, Default)]
pub struct ParameterSets {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

impl ParameterSets {
    pub fn packetize(&mut self, raw: &[u8]) -> Packet {
        let data = normalize_start_codes(raw);
        let (mut keyframe, mut has_sps, mut has_pps) = (false, false, false);
        for nal in nal_units(&data) {
            match NalType::of(nal) {
                Some(NalType::IdrSlice) => keyframe = true,
                Some(NalType::Sps) => {
                    has_sps = true;
                    self.sps = Some(nal.to_vec());
                }
                Some(NalType::Pps) => {
                    has_pps = true;
                    self.pps = Some(nal.to_vec());
                }
                _ => {}
            }
        }
        if keyframe && !(has_sps && has_pps) {
            if let (Some(sps), Some(pps)) = (&self.sps, &self.pps) {
                let mut with = Vec::with_capacity(data.len() + sps.len() + pps.len() + 8);
                for nal in [sps, pps] {
                    with.extend_from_slice(&START_CODE);
                    with.extend_from_slice(nal);
                }
                // The picture's own copies, if it had one of the two, are
                // harmless duplicates.
                with.extend_from_slice(&data);
                return Packet {
                    data: with,
                    keyframe,
                };
            }
        }
        Packet { data, keyframe }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x33];
    const PPS: &[u8] = &[0x68, 0xee, 0x3c];
    const IDR: &[u8] = &[0x65, 0x88, 0x84];
    const P: &[u8] = &[0x41, 0x9a, 0x02];

    fn annexb(nals: &[&[u8]], code: &[u8]) -> Vec<u8> {
        nals.iter().flat_map(|n| [code, n].concat()).collect()
    }

    fn types(data: &[u8]) -> Vec<u8> {
        nal_units(data).map(|n| n[0] & 0x1f).collect()
    }

    #[test]
    fn three_byte_start_codes_become_four_byte_ones() {
        let mut sets = ParameterSets::default();
        let p = sets.packetize(&annexb(&[SPS, PPS, IDR], &[0, 0, 1]));
        assert_eq!(p.data, annexb(&[SPS, PPS, IDR], &START_CODE));
        assert!(p.keyframe);
    }

    #[test]
    fn a_keyframe_without_parameter_sets_gets_the_cached_ones_in_front() {
        let mut sets = ParameterSets::default();
        sets.packetize(&annexb(&[SPS, PPS, IDR], &START_CODE));
        assert!(!sets.packetize(&annexb(&[P], &START_CODE)).keyframe);

        let p = sets.packetize(&annexb(&[IDR], &START_CODE));
        assert!(p.keyframe);
        assert_eq!(types(&p.data), [7, 8, 5]);
    }

    #[test]
    fn a_keyframe_that_has_them_is_left_alone() {
        let mut sets = ParameterSets::default();
        let raw = annexb(&[SPS, PPS, IDR], &START_CODE);
        assert_eq!(sets.packetize(&raw).data, raw);
        assert_eq!(sets.packetize(&raw).data, raw);
    }

    #[test]
    fn a_plain_frame_is_never_given_parameter_sets() {
        let mut sets = ParameterSets::default();
        sets.packetize(&annexb(&[SPS, PPS, IDR], &START_CODE));
        let p = sets.packetize(&annexb(&[P], &START_CODE));
        assert_eq!(types(&p.data), [1]);
    }
}
