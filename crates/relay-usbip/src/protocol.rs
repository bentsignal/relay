//! USB/IP wire protocol (version 1.1.1). All integers are big-endian.

use std::io::{self, Read, Write};

pub const VERSION: u16 = 0x0111;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;

pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const RET_SUBMIT: u32 = 3;
pub const RET_UNLINK: u32 = 4;

pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

pub const SPEED_FULL: u32 = 2;

/// Sent as `number_of_packets` for non-isochronous URBs.
pub const NO_ISO: u32 = 0xffff_ffff;

pub const ST_OK: i32 = 0;
pub const ST_PIPE: i32 = -32;
pub const ST_CONNRESET: i32 = -104;
pub const ST_INVAL: i32 = -22;

pub fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

pub fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub command: u32,
    pub seq: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
}

impl Header {
    pub fn read(r: &mut impl Read) -> io::Result<Self> {
        Ok(Self {
            command: read_u32(r)?,
            seq: read_u32(r)?,
            devid: read_u32(r)?,
            direction: read_u32(r)?,
            ep: read_u32(r)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IsoDesc {
    pub offset: u32,
    pub length: u32,
    pub actual: u32,
    pub status: i32,
}

#[derive(Debug)]
pub struct Submit {
    pub header: Header,
    pub flags: u32,
    pub length: u32,
    pub start_frame: u32,
    pub packets: u32,
    pub interval: u32,
    pub setup: [u8; 8],
    /// OUT payload.
    pub data: Vec<u8>,
    pub iso: Vec<IsoDesc>,
}

impl Submit {
    pub fn is_iso(&self) -> bool {
        self.packets != NO_ISO && self.packets != 0
    }

    pub fn read_body(header: Header, r: &mut impl Read) -> io::Result<Self> {
        let flags = read_u32(r)?;
        let length = read_u32(r)?;
        let start_frame = read_u32(r)?;
        let packets = read_u32(r)?;
        let interval = read_u32(r)?;
        let mut setup = [0u8; 8];
        r.read_exact(&mut setup)?;
        if length > 4 * 1024 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "transfer too large"));
        }
        let mut data = Vec::new();
        if header.direction == DIR_OUT && length > 0 {
            data = vec![0u8; length as usize];
            r.read_exact(&mut data)?;
        }
        let mut iso = Vec::new();
        if packets != NO_ISO && packets != 0 {
            if packets > 1024 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "too many iso packets"));
            }
            iso.reserve(packets as usize);
            for _ in 0..packets {
                iso.push(IsoDesc {
                    offset: read_u32(r)?,
                    length: read_u32(r)?,
                    actual: read_u32(r)?,
                    status: read_u32(r)? as i32,
                });
            }
        }
        Ok(Self { header, flags, length, start_frame, packets, interval, setup, data, iso })
    }
}

/// Encodes a RET_SUBMIT. Reply headers carry zero devid/direction/ep.
pub fn ret_submit(
    seq: u32,
    status: i32,
    actual: u32,
    start_frame: u32,
    iso: Option<&[IsoDesc]>,
    data: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(48 + data.len() + iso.map_or(0, |i| i.len() * 16));
    for v in [RET_SUBMIT, seq, 0, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    let (n, errors) = match iso {
        Some(i) => (i.len() as u32, i.iter().filter(|d| d.status != 0).count() as u32),
        None => (NO_ISO, 0),
    };
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&actual.to_be_bytes());
    out.extend_from_slice(&start_frame.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&errors.to_be_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(data);
    if let Some(iso) = iso {
        for d in iso {
            out.extend_from_slice(&d.offset.to_be_bytes());
            out.extend_from_slice(&d.length.to_be_bytes());
            out.extend_from_slice(&d.actual.to_be_bytes());
            out.extend_from_slice(&d.status.to_be_bytes());
        }
    }
    out
}

pub fn ret_unlink(seq: u32, status: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    for v in [RET_UNLINK, seq, 0, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&[0u8; 24]);
    out
}

pub fn op_header(w: &mut impl Write, code: u16, status: u32) -> io::Result<()> {
    w.write_all(&VERSION.to_be_bytes())?;
    w.write_all(&code.to_be_bytes())?;
    w.write_all(&status.to_be_bytes())
}

pub fn fixed_str<const N: usize>(s: &str) -> [u8; N] {
    let mut b = [0u8; N];
    let n = s.len().min(N - 1);
    b[..n].copy_from_slice(&s.as_bytes()[..n]);
    b
}
