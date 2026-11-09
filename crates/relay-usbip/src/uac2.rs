//! A full-speed USB Audio Class 2.0 device with one stereo playback path
//! (host -> device, what apps send to "Relay") and one stereo capture path
//! (device -> host, the "Relay" microphone). Windows binds its in-box
//! usbaudio2.sys class driver to it.

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const SUBSLOT: usize = 2; // bytes per sample (PCM16)
pub const FRAME_BYTES: usize = CHANNELS * SUBSLOT;
/// Nominal frames per 1 ms full-speed frame.
pub const FRAMES_PER_MS: usize = (RATE / 1000) as usize;
/// Playback packets are exactly 48 frames (adaptive endpoint).
pub const OUT_MAX_PACKET: u16 = (FRAMES_PER_MS * FRAME_BYTES) as u16;
/// Capture packets may carry one extra frame (asynchronous endpoint).
pub const IN_MAX_PACKET: u16 = ((FRAMES_PER_MS + 1) * FRAME_BYTES) as u16;

pub const EP_OUT: u32 = 1;
pub const EP_IN: u32 = 2;

pub const IF_CONTROL: u8 = 0;
pub const IF_PLAYBACK: u8 = 1;
pub const IF_CAPTURE: u8 = 2;

const CLOCK_ID: u8 = 10;
const IT_USB: u8 = 1;
const OT_SPEAKER: u8 = 3;
const IT_MIC: u8 = 4;
const OT_USB: u8 = 5;

/// pid.codes open-source vendor ID.
pub const VENDOR_ID: u16 = 0x1209;

#[derive(Clone, Debug)]
pub struct Identity {
    pub product: String,
    pub serial: String,
    pub product_id: u16,
}

pub struct Descriptors {
    pub device: Vec<u8>,
    pub config: Vec<u8>,
    pub strings: Vec<Vec<u8>>,
}

fn string_desc(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let mut b = vec![(2 + utf16.len() * 2) as u8, 3];
    for c in utf16 {
        b.extend_from_slice(&c.to_le_bytes());
    }
    b
}

impl Descriptors {
    pub fn new(id: &Identity) -> Self {
        let mut device = vec![
            18, 1, // bLength, DEVICE
            0x00, 0x02, // bcdUSB 2.00
            0xEF, 0x02, 0x01, // misc / IAD
            64,   // bMaxPacketSize0
        ];
        device.extend_from_slice(&VENDOR_ID.to_le_bytes());
        device.extend_from_slice(&id.product_id.to_le_bytes());
        device.extend_from_slice(&[0x00, 0x01]); // bcdDevice 1.00
        device.extend_from_slice(&[1, 2, 3, 1]); // iManufacturer, iProduct, iSerial, bNumConfigurations

        let mut c: Vec<u8> = Vec::new();
        // Configuration header, wTotalLength patched below.
        c.extend_from_slice(&[9, 2, 0, 0, 3, 1, 0, 0x80, 50]);
        // Interface association: interfaces 0..2 form one audio function.
        c.extend_from_slice(&[8, 11, 0, 3, 1, 0, 0x20, 2]);
        // Interface 0: AudioControl.
        c.extend_from_slice(&[9, 4, IF_CONTROL, 0, 0, 1, 1, 0x20, 2]);
        let ac_start = c.len();
        c.extend_from_slice(&[9, 0x24, 1, 0x00, 0x02, 0x08, 0, 0, 0]); // header, bcdADC 2.0, category I/O box
        c.extend_from_slice(&[8, 0x24, 0x0A, CLOCK_ID, 0x01, 0x05, 0, 0]); // internal fixed clock
        // Terminal names (string 4: playback, 5: capture) become endpoint names.
        let it = |id: u8, ttype: u16, name: u8| {
            let t = ttype.to_le_bytes();
            [17, 0x24, 0x02, id, t[0], t[1], 0, CLOCK_ID, CHANNELS as u8, 3, 0, 0, 0, 0, 0, 0, name]
        };
        let ot = |id: u8, ttype: u16, src: u8, name: u8| {
            let t = ttype.to_le_bytes();
            [12, 0x24, 0x03, id, t[0], t[1], 0, src, CLOCK_ID, 0, 0, name]
        };
        c.extend_from_slice(&it(IT_USB, 0x0101, 0));
        c.extend_from_slice(&ot(OT_SPEAKER, 0x0301, IT_USB, 4));
        c.extend_from_slice(&it(IT_MIC, mic_terminal_type(), 5));
        c.extend_from_slice(&ot(OT_USB, 0x0101, IT_MIC, 0));
        let ac_len = (c.len() - ac_start) as u16;
        c[ac_start + 6..ac_start + 8].copy_from_slice(&ac_len.to_le_bytes());

        let streaming = |c: &mut Vec<u8>, iface: u8, terminal: u8, ep_addr: u8, attrs: u8, max: u16| {
            c.extend_from_slice(&[9, 4, iface, 0, 0, 1, 2, 0x20, 0]);
            c.extend_from_slice(&[9, 4, iface, 1, 1, 1, 2, 0x20, 0]);
            c.extend_from_slice(&[16, 0x24, 0x01, terminal, 0, 0x01, 0x01, 0, 0, 0, CHANNELS as u8, 3, 0, 0, 0, 0]);
            c.extend_from_slice(&[6, 0x24, 0x02, 0x01, SUBSLOT as u8, (SUBSLOT * 8) as u8]);
            let m = max.to_le_bytes();
            c.extend_from_slice(&[7, 5, ep_addr, attrs, m[0], m[1], 1]);
            c.extend_from_slice(&[8, 0x25, 0x01, 0, 0, 0, 0, 0]);
        };
        // Interface 1: playback, isochronous adaptive OUT.
        streaming(&mut c, IF_PLAYBACK, IT_USB, EP_OUT as u8, 0x09, OUT_MAX_PACKET);
        // Interface 2: capture, isochronous asynchronous IN.
        streaming(&mut c, IF_CAPTURE, OT_USB, 0x80 | EP_IN as u8, 0x05, IN_MAX_PACKET);

        let total = c.len() as u16;
        c[2..4].copy_from_slice(&total.to_le_bytes());

        let strings = vec![
            vec![4, 3, 0x09, 0x04],
            string_desc("Relay"),
            string_desc(&id.product),
            string_desc(&id.serial),
            string_desc(&id.product),
            string_desc(&format!("{} Mic", id.product)),
        ];
        Self { device, config: c, strings }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub fn parse(b: &[u8; 8]) -> Self {
        Self {
            request_type: b[0],
            request: b[1],
            value: u16::from_le_bytes([b[2], b[3]]),
            index: u16::from_le_bytes([b[4], b[5]]),
            length: u16::from_le_bytes([b[6], b[7]]),
        }
    }
}

/// Mutable USB state of one device (configuration, alternate settings).
#[derive(Debug, Default)]
pub struct UsbState {
    pub configuration: u8,
    pub playback_alt: u8,
    pub capture_alt: u8,
}

pub enum ControlResult {
    Data(Vec<u8>),
    Ack,
    Stall,
}

fn trunc(mut v: Vec<u8>, len: u16) -> ControlResult {
    v.truncate(len as usize);
    ControlResult::Data(v)
}

/// Handles a control transfer. `out` is the OUT data stage, if any.
pub fn control(desc: &Descriptors, st: &mut UsbState, s: Setup, _out: &[u8]) -> ControlResult {
    let kind = s.request_type & 0x60;
    if kind == 0x00 {
        return match s.request {
            0x06 => {
                // GET_DESCRIPTOR
                let (ty, idx) = ((s.value >> 8) as u8, s.value as u8);
                match ty {
                    1 => trunc(desc.device.clone(), s.length),
                    2 if idx == 0 => trunc(desc.config.clone(), s.length),
                    3 => match desc.strings.get(idx as usize) {
                        Some(d) => trunc(d.clone(), s.length),
                        None => ControlResult::Stall,
                    },
                    _ => ControlResult::Stall, // qualifier, BOS, MS OS: not supported
                }
            }
            0x05 => ControlResult::Ack, // SET_ADDRESS
            0x09 => {
                // SET_CONFIGURATION
                if s.value > 1 {
                    return ControlResult::Stall;
                }
                st.configuration = s.value as u8;
                st.playback_alt = 0;
                st.capture_alt = 0;
                ControlResult::Ack
            }
            0x08 => trunc(vec![st.configuration], s.length),
            0x0B => {
                // SET_INTERFACE
                let (iface, alt) = (s.index as u8, s.value as u8);
                if st.configuration != 1 || alt > 1 {
                    return ControlResult::Stall;
                }
                match iface {
                    IF_CONTROL if alt == 0 => {}
                    IF_PLAYBACK => st.playback_alt = alt,
                    IF_CAPTURE => st.capture_alt = alt,
                    _ => return ControlResult::Stall,
                }
                ControlResult::Ack
            }
            0x0A => {
                let alt = match s.index as u8 {
                    IF_CONTROL => 0,
                    IF_PLAYBACK => st.playback_alt,
                    IF_CAPTURE => st.capture_alt,
                    _ => return ControlResult::Stall,
                };
                trunc(vec![alt], s.length)
            }
            0x00 => trunc(vec![0, 0], s.length), // GET_STATUS
            0x01 | 0x03 => ControlResult::Ack,   // CLEAR/SET_FEATURE
            0x0C => trunc(vec![0, 0], s.length), // SYNCH_FRAME
            _ => ControlResult::Stall,
        };
    }
    if kind == 0x20 {
        // UAC2 class requests to an entity: wIndex = entity << 8 | interface.
        let entity = (s.index >> 8) as u8;
        let selector = (s.value >> 8) as u8;
        let get = s.request_type & 0x80 != 0;
        if entity == CLOCK_ID {
            return match (get, s.request, selector) {
                (true, 0x01, 0x01) => trunc(RATE.to_le_bytes().to_vec(), s.length), // CUR SAM_FREQ
                (true, 0x02, 0x01) => {
                    // RANGE: one subrange [48000, 48000, 0]
                    let mut v = vec![1, 0];
                    v.extend_from_slice(&RATE.to_le_bytes());
                    v.extend_from_slice(&RATE.to_le_bytes());
                    v.extend_from_slice(&0u32.to_le_bytes());
                    trunc(v, s.length)
                }
                (true, 0x01, 0x02) => trunc(vec![1], s.length), // CLOCK_VALID
                (false, 0x01, 0x01) => ControlResult::Ack,      // SET CUR SAM_FREQ (only 48k exists)
                _ => ControlResult::Stall,
            };
        }
        return ControlResult::Stall;
    }
    ControlResult::Stall
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(config: &[u8]) -> Vec<(u8, u8)> {
        let mut out = Vec::new();
        let mut p = 0;
        while p < config.len() {
            let len = config[p] as usize;
            assert!(len >= 2 && p + len <= config.len(), "bad descriptor at {p}");
            out.push((config[p + 1], config.get(p + 2).copied().unwrap_or(0)));
            p += len;
        }
        out
    }

    #[test]
    fn descriptor_tree_is_well_formed() {
        let d = Descriptors::new(&Identity { product: "Relay".into(), serial: "R1".into(), product_id: 1 });
        assert_eq!(d.device.len(), 18);
        let total = u16::from_le_bytes([d.config[2], d.config[3]]) as usize;
        assert_eq!(total, d.config.len());
        let items = walk(&d.config);
        let interfaces = items.iter().filter(|(t, _)| *t == 4).count();
        assert_eq!(interfaces, 5); // AC + 2 alts each for playback and capture
        let endpoints = items.iter().filter(|(t, _)| *t == 5).count();
        assert_eq!(endpoints, 2);
    }

    #[test]
    fn clock_range_request() {
        let d = Descriptors::new(&Identity { product: "Relay".into(), serial: "R1".into(), product_id: 1 });
        let mut st = UsbState::default();
        let s = Setup { request_type: 0xA1, request: 2, value: 0x0100, index: (CLOCK_ID as u16) << 8, length: 14 };
        match control(&d, &mut st, s, &[]) {
            ControlResult::Data(v) => assert_eq!(v.len(), 14),
            _ => panic!("expected data"),
        }
    }
}

/// USB terminal type of the capture side. 0x0603 ("Line connector") rather than
/// 0x0201 ("Microphone"): Windows attaches voice processing (noise suppression
/// that erases steady sounds) to microphones without their own effects, which
/// would damage music and other audio routed through Relay.
pub fn mic_terminal_type() -> u16 {
    std::env::var("RELAY_MIC_TERMINAL")
        .ok()
        .and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0603)
}
