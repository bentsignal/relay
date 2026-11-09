//! The mixer clock: a realtime-priority thread that runs one graph cycle
//! every [`BLOCK`](crate::BLOCK) frames on an absolute cadence.

use crate::graph::{BLOCK, Graph};
use relay_dsp::ENGINE_RATE;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct MixerThread {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl MixerThread {
    pub fn spawn(mut graph: Graph) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let join = std::thread::Builder::new().name("relay-mixer".into()).spawn(move || {
            let _rt = platform::promote_realtime(BLOCK as u32);
            let timer = platform::Timer::new();
            let period = Duration::from_secs_f64(BLOCK as f64 / ENGINE_RATE as f64);
            let mut epoch = Instant::now();
            let mut cycle: u64 = 0;
            let meters = graph.meters().clone();
            while !stop2.load(Ordering::Relaxed) {
                let t0 = Instant::now();
                graph.process();
                let busy = t0.elapsed();
                meters.cycles.fetch_add(1, Ordering::Relaxed);
                meters.busy_ns.fetch_add(busy.as_nanos() as u64, Ordering::Relaxed);
                cycle += 1;
                let deadline = epoch + period.mul_f64(cycle as f64);
                let now = Instant::now();
                if now > deadline + period * 4 {
                    // We fell far behind (system stall). Restart the cadence
                    // rather than bursting; bridges absorb the gap.
                    meters.late_cycles.fetch_add(1, Ordering::Relaxed);
                    epoch = now;
                    cycle = 0;
                    continue;
                }
                if deadline > now {
                    timer.sleep_until(deadline);
                }
            }
        })?;
        Ok(Self { stop, join: Some(join) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for MixerThread {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
pub mod platform {
    use std::time::Instant;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE,
        SetWaitableTimer, TIMER_ALL_ACCESS, WaitForSingleObject,
    };
    use windows::core::w;

    pub struct Realtime;

    /// Registers the thread with MMCSS "Pro Audio" for low-latency scheduling.
    pub fn promote_realtime(_frames: u32) -> Option<Realtime> {
        let mut task = 0u32;
        unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task) }.ok().map(|_| Realtime)
    }

    pub struct Timer(HANDLE);
    unsafe impl Send for Timer {}

    impl Timer {
        pub fn new() -> Self {
            let h = unsafe {
                CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
            }
            .unwrap_or_default();
            Self(h)
        }

        pub fn sleep_until(&self, deadline: Instant) {
            let now = Instant::now();
            if deadline <= now {
                return;
            }
            let d = deadline - now;
            if self.0.is_invalid() {
                std::thread::sleep(d);
                return;
            }
            // Negative = relative time in 100 ns units.
            let due = -((d.as_nanos() / 100) as i64).max(1);
            unsafe {
                if SetWaitableTimer(self.0, &due, 0, None, None, false).is_ok() {
                    WaitForSingleObject(self.0, INFINITE);
                } else {
                    std::thread::sleep(d);
                }
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub mod platform {
    use std::time::Instant;

    pub struct Realtime;

    #[repr(C)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }

    #[repr(C)]
    struct TimeConstraint {
        period: u32,
        computation: u32,
        constraint: u32,
        preemptible: i32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
        fn mach_absolute_time() -> u64;
        fn mach_wait_until(deadline: u64) -> i32;
        fn thread_policy_set(thread: u32, flavor: i32, info: *const TimeConstraint, count: u32) -> i32;
        fn pthread_mach_thread_np(thread: libc::pthread_t) -> u32;
    }

    const THREAD_TIME_CONSTRAINT_POLICY: i32 = 2;
    const THREAD_TIME_CONSTRAINT_POLICY_COUNT: u32 = 4;

    /// Promotes the thread to a Mach time-constraint (realtime) policy.
    pub fn promote_realtime(frames: u32) -> Option<Realtime> {
        let mut info = TimebaseInfo { numer: 0, denom: 0 };
        if unsafe { mach_timebase_info(&mut info) } != 0 || info.numer == 0 {
            return None;
        }
        let ns_to_abs = |ns: f64| (ns * info.denom as f64 / info.numer as f64) as u32;
        let period_ns = frames as f64 / relay_dsp::ENGINE_RATE as f64 * 1e9;
        let policy = TimeConstraint {
            period: ns_to_abs(period_ns),
            computation: ns_to_abs(period_ns * 0.25),
            constraint: ns_to_abs(period_ns * 0.75),
            preemptible: 1,
        };
        let r = unsafe {
            thread_policy_set(
                pthread_mach_thread_np(libc::pthread_self()),
                THREAD_TIME_CONSTRAINT_POLICY,
                &policy,
                THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };
        (r == 0).then_some(Realtime)
    }

    /// Sleeps with `mach_wait_until`, which wakes realtime threads precisely.
    pub struct Timer {
        numer: u32,
        denom: u32,
    }

    impl Default for Timer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Timer {
        pub fn new() -> Self {
            let mut info = TimebaseInfo { numer: 0, denom: 0 };
            if unsafe { mach_timebase_info(&mut info) } != 0 || info.numer == 0 {
                info = TimebaseInfo { numer: 1, denom: 1 };
            }
            Self { numer: info.numer, denom: info.denom }
        }

        pub fn sleep_until(&self, deadline: Instant) {
            let now = Instant::now();
            if deadline <= now {
                return;
            }
            let ns = (deadline - now).as_nanos();
            let ticks = (ns * self.denom as u128 / self.numer as u128) as u64;
            unsafe {
                mach_wait_until(mach_absolute_time().saturating_add(ticks));
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub mod platform {
    use std::time::Instant;
    pub struct Realtime;
    pub fn promote_realtime(_frames: u32) -> Option<Realtime> {
        None
    }
    pub struct Timer;
    impl Timer {
        pub fn new() -> Self {
            Self
        }
        pub fn sleep_until(&self, deadline: Instant) {
            let now = Instant::now();
            if deadline > now {
                std::thread::sleep(deadline - now);
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::platform;
    use std::time::{Duration, Instant};

    #[test]
    fn realtime_promotion_and_timer_work() {
        std::thread::spawn(|| {
            assert!(platform::promote_realtime(crate::BLOCK as u32).is_some(), "thread_policy_set failed");
            let timer = platform::Timer::new();
            let t0 = Instant::now();
            let deadline = t0 + Duration::from_millis(5);
            timer.sleep_until(deadline);
            let late = Instant::now().saturating_duration_since(deadline);
            assert!(Instant::now() >= deadline);
            assert!(late < Duration::from_millis(20), "woke {late:?} late");
        })
        .join()
        .unwrap();
    }
}
