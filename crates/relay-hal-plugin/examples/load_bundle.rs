//! Loads RelayAudio.driver the way the audio server does (CFPlugIn factory
//! lookup by kAudioServerPlugInTypeUUID), initializes it with a stub host and
//! exercises the device model and an IO loop. Doesn't need the plugin installed.
//!
//!   cargo run -p relay-hal-plugin --example load_bundle -- target/release/RelayAudio.driver

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(target_os = "macos")]
#[allow(non_snake_case, non_upper_case_globals)]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU32, Ordering};

    type CFRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Uuid([u8; 16]);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Addr {
        sel: u32,
        scope: u32,
        elem: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct TimeStamp {
        sample: f64,
        host: u64,
        rate: f64,
        word: u64,
        smpte: [u32; 6],
        flags: u32,
        reserved: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Cycle {
        counter: u64,
        nominal: u32,
        current: TimeStamp,
        input: TimeStamp,
        output: TimeStamp,
        main_ticks: f64,
        dev_ticks: f64,
    }

    type Fp = *const c_void;

    #[repr(C)]
    struct Vtbl {
        _reserved: Fp,
        query_interface: unsafe extern "C" fn(*mut c_void, Uuid, *mut *mut c_void) -> i32,
        add_ref: Fp,
        release: unsafe extern "C" fn(*mut c_void) -> u32,
        initialize: unsafe extern "C" fn(*mut c_void, *const Host) -> i32,
        _create: Fp,
        _destroy: Fp,
        _add_client: Fp,
        _remove_client: Fp,
        _perform: Fp,
        _abort: Fp,
        has_property: unsafe extern "C" fn(*mut c_void, u32, i32, *const Addr) -> u8,
        _settable: Fp,
        get_size: unsafe extern "C" fn(*mut c_void, u32, i32, *const Addr, u32, *const c_void, *mut u32) -> i32,
        get_data:
            unsafe extern "C" fn(*mut c_void, u32, i32, *const Addr, u32, *const c_void, u32, *mut u32, *mut c_void) -> i32,
        _set: Fp,
        start_io: unsafe extern "C" fn(*mut c_void, u32, u32) -> i32,
        stop_io: unsafe extern "C" fn(*mut c_void, u32, u32) -> i32,
        zero_ts: unsafe extern "C" fn(*mut c_void, u32, u32, *mut f64, *mut u64, *mut u64) -> i32,
        _will_do: Fp,
        _begin: Fp,
        do_io: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, *const Cycle, *mut c_void, *mut c_void) -> i32,
        _end: Fp,
    }

    #[repr(C)]
    struct Host {
        properties_changed: unsafe extern "C" fn(*const Host, u32, u32, *const Addr) -> i32,
        _rest: [Fp; 4],
    }

    static CHANGES: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn properties_changed(_: *const Host, obj: u32, n: u32, addrs: *const Addr) -> i32 {
        let addrs = unsafe { std::slice::from_raw_parts(addrs, n as usize) };
        let names: Vec<String> = addrs.iter().map(|a| fcc(a.sel)).collect();
        println!("  host: PropertiesChanged(object {obj}, {names:?})");
        CHANGES.fetch_add(1, Ordering::SeqCst);
        0
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(a: CFRef, p: *const u8, len: isize, dir: u8) -> CFRef;
        fn CFPlugInCreate(a: CFRef, url: CFRef) -> CFRef;
        fn CFPlugInFindFactoriesForPlugInType(t: CFRef) -> CFRef;
        fn CFPlugInInstanceCreate(a: CFRef, factory: CFRef, t: CFRef) -> *mut c_void;
        fn CFArrayGetCount(a: CFRef) -> isize;
        fn CFArrayGetValueAtIndex(a: CFRef, i: isize) -> CFRef;
        fn CFUUIDCreateFromUUIDBytes(a: CFRef, b: Uuid) -> CFRef;
        fn CFStringGetCString(s: CFRef, buf: *mut u8, size: isize, enc: u32) -> u8;
        fn CFStringCreateWithBytes(a: CFRef, b: *const u8, len: isize, enc: u32, ext: u8) -> CFRef;
        fn CFRelease(cf: CFRef);
    }

    const TYPE: Uuid =
        Uuid([0x44, 0x3A, 0xBA, 0xB8, 0xE7, 0xB3, 0x49, 0x1A, 0xB9, 0x85, 0xBE, 0xB9, 0x18, 0x70, 0x30, 0xDB]);
    const IFACE: Uuid =
        Uuid([0xEE, 0xA5, 0x77, 0x3D, 0xCC, 0x43, 0x49, 0xF1, 0x8E, 0x00, 0x8F, 0x96, 0xE7, 0xD2, 0x3B, 0x17]);

    const fn c(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }

    fn fcc(v: u32) -> String {
        String::from_utf8_lossy(&v.to_be_bytes()).into_owned()
    }

    struct Driver {
        d: *mut c_void,
        v: &'static Vtbl,
    }

    impl Driver {
        fn get<T: Copy + Default>(&self, obj: u32, sel: u32, scope: u32, qual: Option<CFRef>) -> Result<Vec<T>, String> {
            let a = Addr { sel, scope, elem: 0 };
            let (qs, qp) = match &qual {
                Some(q) => (8, q as *const CFRef as *const c_void),
                None => (0, std::ptr::null()),
            };
            let mut size = 0u32;
            let e = unsafe { (self.v.get_size)(self.d, obj, 0, &a, qs, qp, &mut size) };
            if e != 0 {
                return Err(format!("size error {}", fcc(e as u32)));
            }
            let n = (size as usize).div_ceil(std::mem::size_of::<T>()).max(1);
            let mut buf = vec![T::default(); n];
            let mut got = 0u32;
            let e = unsafe {
                (self.v.get_data)(self.d, obj, 0, &a, qs, qp, size, &mut got, buf.as_mut_ptr() as *mut c_void)
            };
            if e != 0 {
                return Err(format!("data error {}", fcc(e as u32)));
            }
            buf.truncate(got as usize / std::mem::size_of::<T>());
            Ok(buf)
        }

        fn u32(&self, obj: u32, sel: &[u8; 4]) -> u32 {
            self.get::<u32>(obj, c(sel), c(b"glob"), None).unwrap()[0]
        }

        fn ids(&self, obj: u32, sel: &[u8; 4], scope: &[u8; 4]) -> Vec<u32> {
            self.get::<u32>(obj, c(sel), c(scope), None).unwrap()
        }

        fn string(&self, obj: u32, sel: &[u8; 4]) -> String {
            let s = self.get::<usize>(obj, c(sel), c(b"glob"), None).unwrap()[0] as CFRef;
            let mut buf = [0u8; 256];
            unsafe { CFStringGetCString(s, buf.as_mut_ptr(), 256, 0x0800_0100) };
            unsafe { CFRelease(s) };
            let len = buf.iter().position(|&b| b == 0).unwrap();
            String::from_utf8_lossy(&buf[..len]).into_owned()
        }

        fn translate(&self, uid: &str) -> u32 {
            let s = unsafe { CFStringCreateWithBytes(std::ptr::null(), uid.as_ptr(), uid.len() as isize, 0x0800_0100, 0) };
            let r = self.get::<u32>(1, c(b"uidd"), c(b"glob"), Some(s)).unwrap()[0];
            unsafe { CFRelease(s) };
            r
        }
    }

    pub fn run() {
        let path = std::env::args().nth(1).unwrap_or_else(|| "target/release/RelayAudio.driver".into());
        let cpath = std::fs::canonicalize(&path).expect("bundle path");
        let p = cpath.to_string_lossy().into_owned();
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), p.as_ptr(), p.len() as isize, 1);
            let plugin = CFPlugInCreate(std::ptr::null(), url);
            assert!(!plugin.is_null(), "CFPlugInCreate failed");
            let type_uuid = CFUUIDCreateFromUUIDBytes(std::ptr::null(), TYPE);
            let factories = CFPlugInFindFactoriesForPlugInType(type_uuid);
            let count = if factories.is_null() { 0 } else { CFArrayGetCount(factories) };
            println!("factories for kAudioServerPlugInTypeUUID: {count}");
            assert_eq!(count, 1);
            let unknown = CFPlugInInstanceCreate(std::ptr::null(), CFArrayGetValueAtIndex(factories, 0), type_uuid);
            assert!(!unknown.is_null(), "factory returned NULL");
            let v: &Vtbl = &**(unknown as *const *const Vtbl);
            let mut iface = std::ptr::null_mut();
            assert_eq!((v.query_interface)(unknown, IFACE, &mut iface), 0, "QueryInterface");
            (v.release)(unknown);
            let drv = Driver { d: iface, v: &**(iface as *const *const Vtbl) };

            let host = Host { properties_changed, _rest: [std::ptr::null(); 4] };
            let host = Box::leak(Box::new(host));
            assert_eq!((drv.v.initialize)(drv.d, host), 0, "Initialize");
            println!("initialized; manufacturer {:?}", drv.string(1, b"lmak"));

            let devices = drv.ids(1, b"dev#", b"glob");
            println!("devices while the engine is idle: {devices:?}");
            for &dev in &devices {
                println!(
                    "  device {dev}: {:?} uid {:?} hidden {} streams in {:?} out {:?}",
                    drv.string(dev, b"lnam"),
                    drv.string(dev, b"uid "),
                    drv.u32(dev, b"hidn"),
                    drv.ids(dev, b"stm#", b"inpt"),
                    drv.ids(dev, b"stm#", b"outp"),
                );
            }
            let engine = drv.translate("relay.device.engine");
            assert_eq!(devices, vec![engine]);
            assert_eq!(drv.translate("relay.device.main"), 0, "Relay must be hidden while the engine is idle");
            let fmt = drv.get::<u32>(drv.ids(engine, b"stm#", b"inpt")[0], c(b"sfmt"), c(b"glob"), None).unwrap();
            let rate = f64::from_bits((fmt[0] as u64) | ((fmt[1] as u64) << 32));
            println!("  stream format: {rate} Hz, '{}', {} ch", fcc(fmt[2]), fmt[7]);

            println!("engine starts IO");
            assert_eq!((drv.v.start_io)(drv.d, engine, 7), 0);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let devices = drv.ids(1, b"dev#", b"glob");
            println!("devices while the engine runs: {devices:?}");
            let main = drv.translate("relay.device.main");
            assert!(main != 0 && devices.contains(&main), "Relay should be published");
            assert_eq!(drv.string(main, b"lnam"), "Relay");

            // One loop: an app plays to Relay, the engine reads it; the engine
            // writes the mic, an app records Relay.
            let (mut s0, mut h0, mut seed) = (0f64, 0u64, 0u64);
            assert_eq!((drv.v.zero_ts)(drv.d, engine, 7, &mut s0, &mut h0, &mut seed), 0);
            println!("zero timestamp: sample {s0} host {h0} seed {seed}");
            let frames = 512u32;
            let t = s0 + 20_000.0;
            let mut cyc = Cycle { output: TimeStamp { sample: t, ..Default::default() }, ..Default::default() };
            let tone: Vec<f32> = (0..frames * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5).collect();
            let mut buf = tone.clone();
            let write = c(b"rite");
            let read = c(b"read");
            let main_streams = drv.ids(main, b"stm#", b"glob");
            let eng_streams = drv.ids(engine, b"stm#", b"glob");
            (drv.v.do_io)(drv.d, main, main_streams[1], 1, write, frames, &cyc, buf.as_mut_ptr().cast(), std::ptr::null_mut());
            cyc.input.sample = t;
            let mut got = vec![0f32; buf.len()];
            (drv.v.do_io)(drv.d, engine, eng_streams[0], 7, read, frames, &cyc, got.as_mut_ptr().cast(), std::ptr::null_mut());
            assert_eq!(got, tone, "engine input should equal what was played to Relay");
            println!("Relay output -> Relay Engine input: {frames} frames identical");
            let mic: Vec<f32> = tone.iter().map(|s| -s).collect();
            let mut buf = mic.clone();
            cyc.output.sample = t + 4096.0;
            (drv.v.do_io)(drv.d, engine, eng_streams[1], 7, write, frames, &cyc, buf.as_mut_ptr().cast(), std::ptr::null_mut());
            cyc.input.sample = t + 4096.0;
            (drv.v.do_io)(drv.d, main, main_streams[0], 1, read, frames, &cyc, got.as_mut_ptr().cast(), std::ptr::null_mut());
            assert_eq!(got, mic, "Relay input should equal what the engine wrote");
            println!("Relay Engine output -> Relay input: {frames} frames identical");

            println!("engine stops IO");
            assert_eq!((drv.v.stop_io)(drv.d, engine, 7), 0);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let devices = drv.ids(1, b"dev#", b"glob");
            println!("devices after the engine stopped: {devices:?}");
            assert_eq!(devices, vec![engine]);

            println!("engine starts IO but its cycles stall");
            (drv.v.start_io)(drv.d, engine, 7);
            std::thread::sleep(std::time::Duration::from_millis(300));
            assert_eq!(drv.ids(1, b"dev#", b"glob").len(), 2);
            std::thread::sleep(std::time::Duration::from_millis(1200));
            let devices = drv.ids(1, b"dev#", b"glob");
            println!("devices after 1.5 s without IO cycles: {devices:?}");
            assert_eq!(devices, vec![engine]);
            (drv.v.stop_io)(drv.d, engine, 7);

            let has = (drv.v.has_property)(drv.d, 99, 0, &Addr { sel: c(b"clas"), scope: c(b"glob"), elem: 0 });
            assert_eq!(has, 0);
            println!("device-list notifications: {}", CHANGES.load(Ordering::SeqCst));
            assert_eq!(CHANGES.load(Ordering::SeqCst), 4);
            println!("OK");
        }
    }
}
