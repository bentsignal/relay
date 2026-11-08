//! Types shared by the Relay engine and its UI.
//!
//! The UI is a remote control: it sends [`Command`]s and renders the
//! [`EngineState`] snapshots and [`Meters`] the engine publishes.

use relay_dsp::{CompressorParams, EqBand, GateParams, LimiterParams};
use serde::{Deserialize, Serialize};

/// Bumped whenever the UI <-> engine messages change incompatibly.
pub const IPC_VERSION: u32 = 1;

pub type StripId = u32;
pub type BusId = u32;

/// Stable device identifier: the backend's persistent ID string.
pub type DeviceKey = String;

/// A peer's identity: the hex SHA-256 of its public key, shortened.
pub type PeerId = String;

// ------------------------------------------------------------------ Mixer

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// A physical capture device such as a microphone.
    Device { key: DeviceKey, name: String },
    /// One of Relay's virtual playback devices: what apps send to "Relay".
    Virtual { index: u8 },
    /// Audio of one application, captured without a virtual device.
    App { process: String },
    /// Everything playing on a physical output (loopback).
    Loopback { key: DeviceKey, name: String },
    /// Audio arriving from another device on the network.
    Network { peer: PeerId, stream: u32 },
    /// No source selected yet.
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sink {
    /// A physical playback device.
    Device {
        key: DeviceKey,
        name: String,
    },
    /// One of Relay's virtual microphones.
    VirtualMic {
        index: u8,
    },
    /// Send to another device on the network.
    Network {
        peer: PeerId,
    },
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum PortStatus {
    #[default]
    Idle,
    Starting,
    Running,
    /// The device or peer isn't present right now.
    Missing(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    pub eq: Vec<EqBand>,
    pub eq_enabled: bool,
    pub gate: GateParams,
    pub compressor: CompressorParams,
    pub limiter: LimiterParams,
    /// RNNoise-style noise suppression (inputs only).
    pub denoise: bool,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            eq: EqBand::default_layout(),
            eq_enabled: false,
            gate: GateParams::default(),
            compressor: CompressorParams::default(),
            limiter: LimiterParams::default(),
            denoise: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub source: Source,
    pub gain_db: f32,
    pub mute: bool,
    /// Buses this strip feeds.
    pub routes: Vec<BusId>,
    pub effects: Effects,
    #[serde(default)]
    pub status: PortStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bus {
    pub id: BusId,
    pub name: String,
    pub sink: Sink,
    pub gain_db: f32,
    pub mute: bool,
    pub effects: Effects,
    #[serde(default)]
    pub status: PortStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct MixerConfig {
    pub strips: Vec<Strip>,
    pub buses: Vec<Bus>,
}

// ------------------------------------------------------------------ Devices

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub key: DeviceKey,
    pub name: String,
    pub is_default: bool,
    /// True for Relay's own virtual devices; never offered as hardware.
    pub is_relay: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Devices {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub pid: u32,
    pub process: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum VirtualAudioState {
    #[default]
    Unknown,
    /// The platform component (driver/plugin) needs a one-time install.
    NeedsInstall,
    Installing,
    Starting,
    Ready,
    Unsupported,
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct VirtualAudio {
    pub state: VirtualAudioState,
    /// Relay's devices are the OS defaults.
    pub is_default: bool,
}

// ------------------------------------------------------------------ Network

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    Macos,
    Linux,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum LinkState {
    /// Seen on the network, not connected.
    Available,
    /// Waiting for the other device to allow the connection.
    Requesting,
    /// The other device asked to connect; waiting for this user.
    Incoming,
    Connecting,
    Live,
    /// Connected, but quality is poor. Detail says why.
    Degraded(String),
    /// Was connected and stopped answering. Detail says why.
    Lost(String),
    /// The other side declined or the versions can't talk.
    Refused(String),
    /// Paired before but not seen on the network now.
    Offline,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct LinkStats {
    pub rtt_ms: f32,
    pub jitter_ms: f32,
    pub loss_pct: f32,
    pub buffer_ms: f32,
    pub send_kbps: f32,
    pub recv_kbps: f32,
    pub glitches: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub id: PeerId,
    pub name: String,
    pub platform: Platform,
    pub version: String,
    pub paired: bool,
    pub state: LinkState,
    pub addresses: Vec<String>,
    pub stats: LinkStats,
    /// Streams the peer is currently sending us.
    pub incoming: Vec<RemoteStream>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteStream {
    pub id: u32,
    pub kind: StreamKind,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Audio,
    Video,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Network {
    pub this_id: PeerId,
    pub this_name: String,
    pub enabled: bool,
    /// Why discovery or listening isn't working, if it isn't.
    pub problem: Option<String>,
    pub port: u16,
    pub peers: Vec<Peer>,
}

// ------------------------------------------------------------------ Video

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSource {
    pub key: String,
    pub name: String,
    pub kind: VideoSourceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoSourceKind {
    Screen,
    Window,
    Camera,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSend {
    pub id: u32,
    pub source: String,
    pub peer: PeerId,
    pub with_audio: bool,
    pub status: PortStatus,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub kbps: f32,
    pub encoder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoReceive {
    pub peer: PeerId,
    pub stream: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub latency_ms: f32,
    /// Exposed as a system webcam other apps (e.g. OBS) can use.
    pub as_camera: bool,
    pub decoder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Video {
    pub sources: Vec<VideoSource>,
    pub sending: Vec<VideoSend>,
    pub receiving: Vec<VideoReceive>,
    pub camera_supported: bool,
}

// ------------------------------------------------------------------ Updates

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Dev,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Dev => "dev",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Downloading(f32),
    /// Downloaded; applying restarts Relay.
    Ready(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Update {
    pub channel: Channel,
    pub current: String,
    pub state: UpdateState,
    pub rolled_back_from: Option<String>,
}

// ------------------------------------------------------------------ Engine

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EngineState {
    pub version: String,
    pub mixer: MixerConfig,
    pub devices: Devices,
    pub apps: Vec<AppInfo>,
    pub virtual_audio: VirtualAudio,
    pub network: Network,
    pub video: Video,
    pub update: Update,
    pub engine: EngineHealth,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EngineHealth {
    pub cpu_pct: f32,
    pub block_ms: f32,
    pub overruns: u32,
    pub uptime_s: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Level {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    /// Combined dynamics gain reduction, dB.
    pub reduction_db: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Meters {
    pub strips: Vec<(StripId, Level)>,
    pub buses: Vec<(BusId, Level)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    AddStrip {
        source: Source,
    },
    RemoveStrip {
        id: StripId,
    },
    SetStripSource {
        id: StripId,
        source: Source,
    },
    SetStripName {
        id: StripId,
        name: String,
    },
    SetStripGain {
        id: StripId,
        gain_db: f32,
    },
    SetStripMute {
        id: StripId,
        mute: bool,
    },
    SetRoute {
        strip: StripId,
        bus: BusId,
        on: bool,
    },
    SetStripEffects {
        id: StripId,
        effects: Effects,
    },

    AddBus {
        sink: Sink,
    },
    RemoveBus {
        id: BusId,
    },
    SetBusSink {
        id: BusId,
        sink: Sink,
    },
    SetBusName {
        id: BusId,
        name: String,
    },
    SetBusGain {
        id: BusId,
        gain_db: f32,
    },
    SetBusMute {
        id: BusId,
        mute: bool,
    },
    SetBusEffects {
        id: BusId,
        effects: Effects,
    },

    InstallVirtualAudio,
    MakeRelayDefault {
        on: bool,
    },

    SetNetworkEnabled {
        on: bool,
    },
    SetDeviceName {
        name: String,
    },
    Connect {
        peer: PeerId,
    },
    AcceptPeer {
        peer: PeerId,
    },
    RejectPeer {
        peer: PeerId,
    },
    ForgetPeer {
        peer: PeerId,
    },

    StartVideo {
        source: String,
        peer: PeerId,
        with_audio: bool,
    },
    StopVideo {
        id: u32,
    },
    SetVideoCamera {
        peer: PeerId,
        stream: u32,
        on: bool,
    },

    SetChannel {
        channel: Channel,
    },
    CheckForUpdates,
    ApplyUpdate,

    Quit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        ipc_version: u32,
        client: String,
    },
    Command(Command),
    /// Ask for meter updates (~30 Hz) while the UI is visible.
    Meters {
        on: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        ipc_version: u32,
        engine_version: String,
    },
    State(Box<EngineState>),
    Meters(Meters),
    /// A one-off message the UI should surface, e.g. a failed command.
    Notice {
        text: String,
        error: bool,
    },
}
