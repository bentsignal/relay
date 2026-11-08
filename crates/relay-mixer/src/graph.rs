//! The realtime mixing graph.
//!
//! The control side ([`GraphHandle`]) owns all allocation. It sends boxed
//! objects to the audio side ([`Graph`]) through a lock-free queue and gets
//! replaced objects back through another queue so nothing is freed on the
//! audio thread.

use crate::bridge::{InputBridge, OutputBridge};
use relay_dsp::{Compressor, ENGINE_RATE, Eq, Gate, Limiter, Meter, SmoothedGain, db_to_gain};
use relay_proto::{Effects, Level};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Frames processed per mixer cycle: 5 ms at 48 kHz.
pub const BLOCK: usize = 240;
pub const MAX_STRIPS: usize = 64;
pub const MAX_BUSES: usize = 64;

pub trait SourcePort: Send {
    /// Fills both channels completely. Returns false if nothing real was delivered.
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> bool;
}

pub trait SinkPort: Send {
    fn write(&mut self, left: &[f32], right: &[f32]);
}

impl SourcePort for InputBridge {
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        InputBridge::read(self, left, right)
    }
}

impl SinkPort for OutputBridge {
    fn write(&mut self, left: &[f32], right: &[f32]) {
        OutputBridge::write(self, left, right)
    }
}

pub struct Silence;
impl SourcePort for Silence {
    fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        left.fill(0.0);
        right.fill(0.0);
        false
    }
}

pub struct Discard;
impl SinkPort for Discard {
    fn write(&mut self, _: &[f32], _: &[f32]) {}
}

/// Per-strip/bus effect processors.
pub struct EffectsRt {
    eq: Eq,
    eq_on: bool,
    gate: Gate,
    comp: Compressor,
    limiter: Limiter,
    denoise: Option<Box<dyn Processor>>,
}

/// An optional extra processor such as noise suppression.
pub trait Processor: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
}

impl EffectsRt {
    pub fn new(effects: &Effects, denoise: Option<Box<dyn Processor>>) -> Self {
        let rate = ENGINE_RATE as f32;
        let mut e = Self {
            eq: Eq::new(rate),
            eq_on: false,
            gate: Gate::new(rate),
            comp: Compressor::new(rate),
            limiter: Limiter::new(rate),
            denoise,
        };
        e.apply(effects);
        e
    }

    fn apply(&mut self, fx: &Effects) {
        self.eq.set_bands(&fx.eq);
        self.eq_on = fx.eq_enabled;
        self.gate.set_params(fx.gate);
        self.comp.set_params(fx.compressor);
        self.limiter.set_params(fx.limiter);
    }

    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        if let Some(d) = self.denoise.as_mut() {
            d.process(l, r);
        }
        self.gate.process(l, r);
        if self.eq_on {
            self.eq.process(l, r);
        }
        self.comp.process(l, r);
        self.limiter.process(l, r);
    }

    fn reduction_db(&self) -> f32 {
        self.gate.reduction_db().max(self.comp.reduction_db()).max(self.limiter.reduction_db())
    }
}

pub struct StripRt {
    slot: usize,
    source: Box<dyn SourcePort>,
    gain: f32,
    mute: bool,
    routes: u64,
    fx: EffectsRt,
    g_gain: SmoothedGain,
    g_mute: SmoothedGain,
    meter: Meter,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl StripRt {
    pub fn new(
        slot: usize,
        source: Box<dyn SourcePort>,
        gain_db: f32,
        mute: bool,
        routes: u64,
        fx: EffectsRt,
    ) -> Box<Self> {
        Box::new(Self {
            slot,
            source,
            gain: db_to_gain(gain_db),
            mute,
            routes,
            fx,
            g_gain: SmoothedGain::new(db_to_gain(gain_db)),
            g_mute: SmoothedGain::new(if mute { 0.0 } else { 1.0 }),
            meter: Meter::default(),
            l: vec![0.0; BLOCK],
            r: vec![0.0; BLOCK],
        })
    }
}

pub struct BusRt {
    slot: usize,
    sink: Box<dyn SinkPort>,
    gain: f32,
    mute: bool,
    fx: EffectsRt,
    g_gain: SmoothedGain,
    g_mute: SmoothedGain,
    meter: Meter,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl BusRt {
    pub fn new(slot: usize, sink: Box<dyn SinkPort>, gain_db: f32, mute: bool, fx: EffectsRt) -> Box<Self> {
        Box::new(Self {
            slot,
            sink,
            gain: db_to_gain(gain_db),
            mute,
            fx,
            g_gain: SmoothedGain::new(db_to_gain(gain_db)),
            g_mute: SmoothedGain::new(if mute { 0.0 } else { 1.0 }),
            meter: Meter::default(),
            l: vec![0.0; BLOCK],
            r: vec![0.0; BLOCK],
        })
    }
}

pub enum MixerCmd {
    AddStrip(Box<StripRt>),
    RemoveStrip(usize),
    SetStripSource(usize, Box<dyn SourcePort>),
    SetStripGain(usize, f32),
    SetStripMute(usize, bool),
    SetStripRoutes(usize, u64),
    SetStripEffects(usize, Box<Effects>),
    SetStripDenoise(usize, Option<Box<dyn Processor>>),
    AddBus(Box<BusRt>),
    RemoveBus(usize),
    SetBusSink(usize, Box<dyn SinkPort>),
    SetBusGain(usize, f32),
    SetBusMute(usize, bool),
    SetBusEffects(usize, Box<Effects>),
}

/// Objects handed back to the control thread to be dropped there.
pub enum Garbage {
    Strip(Box<StripRt>),
    Bus(Box<BusRt>),
    Source(Box<dyn SourcePort>),
    Sink(Box<dyn SinkPort>),
    Effects(Box<Effects>),
    Processor(Box<dyn Processor>),
}

const METER_FIELDS: usize = 5;

/// Meter values shared between the audio thread and readers.
pub struct MeterBank {
    strips: Vec<AtomicU32>,
    buses: Vec<AtomicU32>,
    /// Audio-thread health.
    pub cycles: AtomicU64,
    pub busy_ns: AtomicU64,
    pub late_cycles: AtomicU32,
}

impl MeterBank {
    fn new() -> Self {
        let mk = |n: usize| (0..n * METER_FIELDS).map(|_| AtomicU32::new(0)).collect();
        Self {
            strips: mk(MAX_STRIPS),
            buses: mk(MAX_BUSES),
            cycles: AtomicU64::new(0),
            busy_ns: AtomicU64::new(0),
            late_cycles: AtomicU32::new(0),
        }
    }

    fn publish(arr: &[AtomicU32], slot: usize, m: &mut Meter, reduction: f32) {
        let r = m.take();
        let base = slot * METER_FIELDS;
        let max = |a: &AtomicU32, v: f32| {
            let _ = a.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (v > f32::from_bits(old)).then_some(v.to_bits())
            });
        };
        max(&arr[base], r.peak_l);
        max(&arr[base + 1], r.peak_r);
        // RMS: one-pole smoothing across blocks.
        for (i, v) in [(2, r.rms_l), (3, r.rms_r)] {
            let old = f32::from_bits(arr[base + i].load(Ordering::Relaxed));
            arr[base + i].store((old * 0.8 + v * 0.2).to_bits(), Ordering::Relaxed);
        }
        arr[base + 4].store(reduction.to_bits(), Ordering::Relaxed);
    }

    fn read(arr: &[AtomicU32], slot: usize) -> Level {
        let base = slot * METER_FIELDS;
        let get = |i: usize| f32::from_bits(arr[base + i].load(Ordering::Relaxed));
        let peak_l = f32::from_bits(arr[base].swap(0, Ordering::Relaxed));
        let peak_r = f32::from_bits(arr[base + 1].swap(0, Ordering::Relaxed));
        Level { peak_l, peak_r, rms_l: get(2), rms_r: get(3), reduction_db: get(4) }
    }

    pub fn strip(&self, slot: usize) -> Level {
        Self::read(&self.strips, slot)
    }

    pub fn bus(&self, slot: usize) -> Level {
        Self::read(&self.buses, slot)
    }

    fn clear(arr: &[AtomicU32], slot: usize) {
        for i in 0..METER_FIELDS {
            arr[slot * METER_FIELDS + i].store(0, Ordering::Relaxed);
        }
    }
}

/// Audio-thread side.
pub struct Graph {
    strips: Vec<Option<Box<StripRt>>>,
    buses: Vec<Option<Box<BusRt>>>,
    cmds: rtrb::Consumer<MixerCmd>,
    garbage: rtrb::Producer<Garbage>,
    meters: Arc<MeterBank>,
}

/// Control-thread side.
pub struct GraphHandle {
    cmds: rtrb::Producer<MixerCmd>,
    garbage: rtrb::Consumer<Garbage>,
    meters: Arc<MeterBank>,
}

pub fn graph() -> (GraphHandle, Graph) {
    let (cp, cc) = rtrb::RingBuffer::new(4096);
    let (gp, gc) = rtrb::RingBuffer::new(4096);
    let meters = Arc::new(MeterBank::new());
    (
        GraphHandle { cmds: cp, garbage: gc, meters: meters.clone() },
        Graph {
            strips: (0..MAX_STRIPS).map(|_| None).collect(),
            buses: (0..MAX_BUSES).map(|_| None).collect(),
            cmds: cc,
            garbage: gp,
            meters,
        },
    )
}

impl GraphHandle {
    /// Queues a command. Fails only if the audio thread has stalled for a long time.
    pub fn send(&mut self, cmd: MixerCmd) -> Result<(), MixerCmd> {
        self.collect_garbage();
        self.cmds.push(cmd).map_err(|rtrb::PushError::Full(c)| c)
    }

    pub fn collect_garbage(&mut self) {
        while let Ok(g) = self.garbage.pop() {
            drop(g);
        }
    }

    pub fn meters(&self) -> &Arc<MeterBank> {
        &self.meters
    }
}

impl Graph {
    pub fn meters(&self) -> &Arc<MeterBank> {
        &self.meters
    }

    fn trash(&mut self, g: Garbage) {
        // If the garbage queue is full the control thread is stuck; leaking is
        // better than freeing on the audio thread.
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            std::mem::forget(g);
        }
    }

    fn apply(&mut self, cmd: MixerCmd) {
        match cmd {
            MixerCmd::AddStrip(s) => {
                let slot = s.slot;
                MeterBank::clear(&self.meters.strips, slot);
                if let Some(old) = self.strips[slot].replace(s) {
                    self.trash(Garbage::Strip(old));
                }
            }
            MixerCmd::RemoveStrip(slot) => {
                if let Some(old) = self.strips[slot].take() {
                    self.trash(Garbage::Strip(old));
                }
                MeterBank::clear(&self.meters.strips, slot);
            }
            MixerCmd::SetStripSource(slot, src) => match self.strips[slot].as_mut() {
                Some(s) => {
                    let old = std::mem::replace(&mut s.source, src);
                    self.trash(Garbage::Source(old));
                }
                None => self.trash(Garbage::Source(src)),
            },
            MixerCmd::SetStripGain(slot, g) => {
                if let Some(s) = self.strips[slot].as_mut() {
                    s.gain = g;
                }
            }
            MixerCmd::SetStripMute(slot, m) => {
                if let Some(s) = self.strips[slot].as_mut() {
                    s.mute = m;
                }
            }
            MixerCmd::SetStripRoutes(slot, routes) => {
                if let Some(s) = self.strips[slot].as_mut() {
                    s.routes = routes;
                }
            }
            MixerCmd::SetStripEffects(slot, fx) => {
                if let Some(s) = self.strips[slot].as_mut() {
                    s.fx.apply(&fx);
                }
                self.trash(Garbage::Effects(fx));
            }
            MixerCmd::SetStripDenoise(slot, d) => {
                let old = match self.strips[slot].as_mut() {
                    Some(s) => std::mem::replace(&mut s.fx.denoise, d),
                    None => d,
                };
                if let Some(old) = old {
                    self.trash(Garbage::Processor(old));
                }
            }
            MixerCmd::AddBus(b) => {
                let slot = b.slot;
                MeterBank::clear(&self.meters.buses, slot);
                if let Some(old) = self.buses[slot].replace(b) {
                    self.trash(Garbage::Bus(old));
                }
            }
            MixerCmd::RemoveBus(slot) => {
                if let Some(old) = self.buses[slot].take() {
                    self.trash(Garbage::Bus(old));
                }
                MeterBank::clear(&self.meters.buses, slot);
            }
            MixerCmd::SetBusSink(slot, sink) => match self.buses[slot].as_mut() {
                Some(b) => {
                    let old = std::mem::replace(&mut b.sink, sink);
                    self.trash(Garbage::Sink(old));
                }
                None => self.trash(Garbage::Sink(sink)),
            },
            MixerCmd::SetBusGain(slot, g) => {
                if let Some(b) = self.buses[slot].as_mut() {
                    b.gain = g;
                }
            }
            MixerCmd::SetBusMute(slot, m) => {
                if let Some(b) = self.buses[slot].as_mut() {
                    b.mute = m;
                }
            }
            MixerCmd::SetBusEffects(slot, fx) => {
                if let Some(b) = self.buses[slot].as_mut() {
                    b.fx.apply(&fx);
                }
                self.trash(Garbage::Effects(fx));
            }
        }
    }

    /// Runs one mixer cycle of [`BLOCK`] frames.
    pub fn process(&mut self) {
        while let Ok(cmd) = self.cmds.pop() {
            self.apply(cmd);
        }

        for b in self.buses.iter_mut().flatten() {
            b.l.fill(0.0);
            b.r.fill(0.0);
        }

        for s in self.strips.iter_mut().flatten() {
            let StripRt { source, l, r, .. } = &mut **s;
            source.read(l, r);
            s.fx.process(&mut s.l, &mut s.r);
            s.g_gain.apply(s.gain, &mut s.l, &mut s.r);
            // Meter before mute so a muted input still shows it's alive.
            s.meter.process(&s.l, &s.r);
            MeterBank::publish(&self.meters.strips, s.slot, &mut s.meter, s.fx.reduction_db());
            let silent = s.mute && s.g_mute.current() == 0.0;
            s.g_mute.apply(if s.mute { 0.0 } else { 1.0 }, &mut s.l, &mut s.r);
            if s.routes == 0 || silent {
                continue;
            }
            let mut routes = s.routes;
            while routes != 0 {
                let slot = routes.trailing_zeros() as usize;
                routes &= routes - 1;
                if let Some(b) = self.buses[slot].as_mut() {
                    for i in 0..BLOCK {
                        b.l[i] += s.l[i];
                        b.r[i] += s.r[i];
                    }
                }
            }
        }

        for b in self.buses.iter_mut().flatten() {
            b.fx.process(&mut b.l, &mut b.r);
            b.g_gain.apply(b.gain, &mut b.l, &mut b.r);
            b.g_mute.apply(if b.mute { 0.0 } else { 1.0 }, &mut b.l, &mut b.r);
            b.meter.process(&b.l, &b.r);
            MeterBank::publish(&self.meters.buses, b.slot, &mut b.meter, b.fx.reduction_db());
            b.sink.write(&b.l, &b.r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Const(f32);
    impl SourcePort for Const {
        fn read(&mut self, l: &mut [f32], r: &mut [f32]) -> bool {
            l.fill(self.0);
            r.fill(self.0);
            true
        }
    }

    struct Capture(Arc<Mutex<Vec<f32>>>);
    impl SinkPort for Capture {
        fn write(&mut self, l: &[f32], _r: &[f32]) {
            self.0.lock().unwrap().extend_from_slice(l);
        }
    }

    #[test]
    fn routes_and_mixes() {
        let (mut h, mut g) = graph();
        let fx = Effects::default();
        let out = Arc::new(Mutex::new(Vec::new()));
        h.send(MixerCmd::AddBus(BusRt::new(3, Box::new(Capture(out.clone())), 0.0, false, EffectsRt::new(&fx, None))))
            .ok();
        h.send(MixerCmd::AddStrip(StripRt::new(
            0,
            Box::new(Const(0.25)),
            0.0,
            false,
            1 << 3,
            EffectsRt::new(&fx, None),
        )))
        .ok();
        h.send(MixerCmd::AddStrip(StripRt::new(
            1,
            Box::new(Const(0.5)),
            0.0,
            false,
            1 << 3,
            EffectsRt::new(&fx, None),
        )))
        .ok();
        h.send(MixerCmd::AddStrip(StripRt::new(2, Box::new(Const(1.0)), 0.0, false, 0, EffectsRt::new(&fx, None))))
            .ok();
        g.process();
        g.process();
        let v = out.lock().unwrap();
        assert_eq!(v.len(), BLOCK * 2);
        assert!((v[BLOCK + 5] - 0.75).abs() < 1e-6);
        let lvl = h.meters().strip(2);
        assert!((lvl.peak_l - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mute_ramps_to_silence() {
        let (mut h, mut g) = graph();
        let fx = Effects::default();
        let out = Arc::new(Mutex::new(Vec::new()));
        h.send(MixerCmd::AddBus(BusRt::new(0, Box::new(Capture(out.clone())), 0.0, false, EffectsRt::new(&fx, None))))
            .ok();
        h.send(MixerCmd::AddStrip(StripRt::new(0, Box::new(Const(0.5)), 0.0, false, 1, EffectsRt::new(&fx, None))))
            .ok();
        g.process();
        h.send(MixerCmd::SetStripMute(0, true)).ok();
        g.process();
        g.process();
        let v = out.lock().unwrap();
        assert!(v[BLOCK] > 0.49 && v[2 * BLOCK - 1].abs() < 0.01, "{} {} {}", v[0], v[BLOCK], v[2 * BLOCK - 1]);
        assert_eq!(v[2 * BLOCK + 10], 0.0);
    }
}
