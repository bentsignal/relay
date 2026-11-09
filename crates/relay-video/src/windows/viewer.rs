//! A plain Win32 window that decodes and shows incoming video with minimal
//! latency: compressed frames in, D3D11 flip-model swap chain out.
//! Double-click toggles full screen; Esc leaves it.
//! Every viewer also feeds the Relay Camera (the newest stream wins).

use super::camera::{CameraFeed, owner};
use super::convert::Converter;
use super::decode::{Decoded, Decoder};
use super::{Gpu, create_device};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use relay_proto::EncodedVideo;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub enum ViewerMsg {
    Frame(EncodedVideo),
    Close,
}

/// Handle to an open viewer window.
pub struct Viewer {
    tx: Sender<ViewerMsg>,
    state: Arc<WindowState>,
}

impl Viewer {
    /// Returns false once the user has closed the window.
    pub fn push(&self, f: EncodedVideo) -> bool {
        if self.state.closed.load(Ordering::Relaxed) {
            return false;
        }
        let _ = self.tx.try_send(ViewerMsg::Frame(f));
        true
    }

    pub fn is_open(&self) -> bool {
        !self.state.closed.load(Ordering::Relaxed)
    }

    /// This stream is what the Relay Camera shows.
    pub fn is_camera(&self) -> bool {
        self.state.camera.load(Ordering::Relaxed)
    }

    /// True (once) after the viewer had to drop frames and needs a keyframe.
    pub fn take_keyframe_request(&self) -> bool {
        self.state.need_key.swap(false, Ordering::Relaxed)
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.tx.send(ViewerMsg::Close);
    }
}

struct WindowState {
    closed: AtomicBool,
    need_key: AtomicBool,
    camera: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
    fullscreen: AtomicBool,
    restore: parking_lot::Mutex<(WINDOW_STYLE, RECT)>,
}

pub fn open(title: &str) -> Result<Viewer> {
    let (tx, rx) = bounded::<ViewerMsg>(16);
    let state = Arc::new(WindowState {
        closed: AtomicBool::new(false),
        need_key: AtomicBool::new(false),
        camera: AtomicBool::new(false),
        width: AtomicU32::new(1280),
        height: AtomicU32::new(720),
        fullscreen: AtomicBool::new(false),
        restore: parking_lot::Mutex::new((WINDOW_STYLE(0), RECT::default())),
    });
    let st = state.clone();
    let title = title.to_string();
    let (ready_tx, ready_rx) = bounded::<Result<()>>(1);
    std::thread::Builder::new().name("relay-viewer".into()).spawn(move || {
        if let Err(e) = run(&title, rx, st.clone(), &ready_tx) {
            tracing::warn!("viewer stopped: {e:#}");
            let _ = ready_tx.try_send(Err(e));
        }
        st.closed.store(true, Ordering::Relaxed);
    })?;
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(Ok(()))?;
    Ok(Viewer { tx, state })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
        let state = ptr.as_ref();
        match msg {
            WM_SIZE => {
                if let Some(s) = state {
                    s.width.store((lparam.0 & 0xffff) as u32, Ordering::Relaxed);
                    s.height.store(((lparam.0 >> 16) & 0xffff) as u32, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                if let Some(s) = state {
                    toggle_fullscreen(hwnd, s);
                }
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                if let Some(s) = state
                    && s.fullscreen.load(Ordering::Relaxed)
                {
                    toggle_fullscreen(hwnd, s);
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_DESTROY => {
                if let Some(s) = state {
                    s.closed.store(true, Ordering::Relaxed);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn toggle_fullscreen(hwnd: HWND, s: &WindowState) {
    unsafe {
        if !s.fullscreen.load(Ordering::Relaxed) {
            let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            *s.restore.lock() = (style, rect);
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            SetWindowLongW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as i32);
            let r = mi.rcMonitor;
            let _ =
                SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED);
            s.fullscreen.store(true, Ordering::Relaxed);
        } else {
            let (style, r) = *s.restore.lock();
            SetWindowLongW(hwnd, GWL_STYLE, style.0 as i32);
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_FRAMECHANGED | SWP_NOZORDER,
            );
            s.fullscreen.store(false, Ordering::Relaxed);
        }
    }
}

fn register_class() -> PCWSTR {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let name = w!("RelayViewer");
    ONCE.call_once(|| unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: name,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
    name
}

fn fit(src: (u32, u32), dst: (u32, u32)) -> RECT {
    let scale = (dst.0 as f32 / src.0.max(1) as f32).min(dst.1 as f32 / src.1.max(1) as f32);
    let (w, h) = ((src.0 as f32 * scale) as i32, (src.1 as f32 * scale) as i32);
    let (x, y) = ((dst.0 as i32 - w) / 2, (dst.1 as i32 - h) / 2);
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

fn run(title: &str, rx: Receiver<ViewerMsg>, state: Arc<WindowState>, ready: &Sender<Result<()>>) -> Result<()> {
    let gpu: Gpu = create_device(None)?;
    let class = register_class();
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR(wide.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            760,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?
    };
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::as_ptr(&state) as isize);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        state.width.store((rc.right - rc.left) as u32, Ordering::Relaxed);
        state.height.store((rc.bottom - rc.top) as u32, Ordering::Relaxed);
    }

    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1()? };
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: 0,
        Height: 0,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: FALSE,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };
    let swap = unsafe { factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None)? };
    let _ = ready.try_send(Ok(()));

    let mut decoder: Option<Decoder> = None;
    let mut converter: Option<(Converter, (u32, u32), (u32, u32))> = None;
    let mut swap_size = (0u32, 0u32);
    let mut decoded: Vec<Decoded> = Vec::new();
    let mut last: Option<Decoded> = None;
    let mut last_pts = 0i64;
    // Camera: claimed when our first picture decodes, released when we stop.
    struct Release(u64);
    impl Drop for Release {
        fn drop(&mut self) {
            owner::release(self.0);
        }
    }
    let cam = Release(owner::new_id());
    let mut claimed = false;
    let mut feed = CameraFeed::new();
    loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if state.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let msg = match rx.recv_timeout(Duration::from_millis(8)) {
            Ok(m) => Some(m),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
            Err(_) => return Ok(()),
        };
        let mut redraw = false;
        match msg {
            Some(ViewerMsg::Close) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Ok(());
            }
            Some(ViewerMsg::Frame(f)) => {
                let size = (f.width as u32, f.height as u32);
                if decoder.as_ref().map(|d| (d.width, d.height) != size).unwrap_or(true) && f.key {
                    decoder = Some(Decoder::new(&gpu, size.0, size.1).context("decoder")?);
                    converter = None;
                }
                if let Some(d) = decoder.as_mut() {
                    if let Err(e) = d.decode(&f.data, f.pts_us as i64 * 10, &mut decoded) {
                        tracing::debug!("decode error: {e:#}");
                        state.need_key.store(true, Ordering::Relaxed);
                        decoder = None;
                    }
                } else {
                    state.need_key.store(true, Ordering::Relaxed);
                }
                if let Some(d) = decoded.drain(..).last() {
                    last = Some(d);
                    last_pts = f.pts_us as i64 * 10;
                    redraw = true;
                    if !claimed {
                        owner::claim(cam.0);
                        claimed = true;
                    }
                }
                // Skip straight to the newest frame if more are queued.
            }
            None => {}
        }
        let is_camera = claimed && owner::check(cam.0);
        state.camera.store(is_camera, Ordering::Relaxed);
        if is_camera
            && let Some(frame) = &last
            && (redraw || !feed.pushed_within(Duration::from_millis(250)))
            && let Err(e) = feed.push(&gpu, &frame.texture, frame.slice, frame.width, frame.height, last_pts)
        {
            tracing::debug!("camera feed: {e:#}");
        }
        let win = (state.width.load(Ordering::Relaxed).max(1), state.height.load(Ordering::Relaxed).max(1));
        if win != swap_size {
            unsafe { swap.ResizeBuffers(0, win.0, win.1, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))? };
            swap_size = win;
            converter = None;
            redraw = true;
        }
        if !redraw {
            continue;
        }
        let Some(frame) = &last else { continue };
        let src = (frame.width, frame.height);
        if converter.as_ref().map(|(_, s, d)| *s != src || *d != win).unwrap_or(true) {
            let c = Converter::new(&gpu, src, win, 60, false)?;
            c.set_dest(fit(src, win));
            converter = Some((c, src, win));
        }
        let back: ID3D11Texture2D = unsafe { swap.GetBuffer(0)? };
        if let Some((c, ..)) = &converter
            && let Err(e) = c.convert(&frame.texture, frame.slice, &back)
        {
            tracing::debug!("present convert failed: {e:#}");
        }
        drop(back);
        unsafe {
            let _ = swap.Present(0, DXGI_PRESENT(0));
        }
    }
}
