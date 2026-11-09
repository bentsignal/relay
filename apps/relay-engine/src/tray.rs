//! Windows notification-area icon: shows that Relay is running when its window
//! is closed. Click opens Relay; right-click offers Quit.

use crate::Event;
use crossbeam_channel::Sender;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: usize = 1;
const ID_QUIT: usize = 2;

static mut EVENTS: Option<Sender<Event>> = None;

fn open_app() {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::process::Command::new(dir.join("relay.exe")).spawn();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                match (lparam.0 & 0xffff) as u32 {
                    WM_LBUTTONUP => open_app(),
                    WM_RBUTTONUP => {
                        let menu = CreatePopupMenu().unwrap_or_default();
                        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN, w!("Open Relay"));
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit Relay"));
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = SetForegroundWindow(hwnd);
                        let cmd =
                            TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
                        let _ = DestroyMenu(menu);
                        match cmd.0 as usize {
                            ID_OPEN => open_app(),
                            ID_QUIT =>
                            {
                                #[allow(static_mut_refs)]
                                if let Some(tx) = EVENTS.as_ref() {
                                    let _ = tx.send(Event::Shutdown);
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Adds the tray icon on its own thread. The icon goes away with the process.
pub fn spawn(events: Sender<Event>) {
    let _ = std::thread::Builder::new().name("relay-tray".into()).spawn(move || unsafe {
        EVENTS = Some(events);
        let Ok(hinstance) = GetModuleHandleW(None) else { return };
        let class = w!("RelayTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Relay"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        ) else {
            return;
        };
        let icon = LoadIconW(Some(hinstance.into()), PCWSTR(1 as _))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
        let mut tip = [0u16; 128];
        for (i, c) in "Relay".encode_utf16().enumerate() {
            tip[i] = c;
        }
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: icon,
            szTip: tip,
            ..Default::default()
        };
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            tracing::warn!("tray icon unavailable");
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    });
}
