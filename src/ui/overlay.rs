//! Native, non-activating Windows floating dictation pill.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::null;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use windows_sys::Win32::Foundation::{GetLastError, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse,
    EndPaint, FillRect, GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromWindow,
    RoundRect, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, UpdateWindow, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_BRUSH, PS_SOLID,
    TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetSystemMetrics, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WM_PAINT, WM_QUIT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

const WINDOW_WIDTH: i32 = 270;
const WINDOW_HEIGHT: i32 = 58;
const VISIBLE_ALPHA: f32 = 238.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    Idle,
    Listening,
}

impl OverlayState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready  |  Right Alt",
            Self::Listening => "Listening...",
        }
    }

    fn accent(self) -> u32 {
        match self {
            Self::Idle => rgb(120, 145, 165),
            Self::Listening => rgb(255, 76, 91),
        }
    }
}

#[derive(Clone, Copy)]
struct VisualState {
    state: OverlayState,
    volume: f32,
    phase: f32,
}

impl Default for VisualState {
    fn default() -> Self {
        Self {
            state: OverlayState::Idle,
            volume: 0.0,
            phase: 0.0,
        }
    }
}

thread_local! {
    static VISUAL_STATE: RefCell<VisualState> = RefCell::new(VisualState::default());
}

enum OverlayCommand {
    Show(OverlayState),
    Hide,
    Volume(f32),
}

pub struct Overlay {
    sender: Sender<OverlayCommand>,
}

impl Overlay {
    pub fn spawn() -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

        thread::Builder::new()
            .name("voice-dictation-overlay".to_string())
            .spawn(move || {
                let hwnd = match create_window() {
                    Ok(hwnd) => hwnd,
                    Err(error) => {
                        let _ = ready_sender.send(Err(format!("{error:#}")));
                        return;
                    }
                };

                if ready_sender.send(Ok(())).is_err() {
                    return;
                }

                if let Err(error) = run_message_loop(hwnd, receiver) {
                    tracing::error!(error = %error, "Overlay thread failed");
                }
            })
            .context("Failed to start the overlay thread")?;

        match ready_receiver
            .recv_timeout(Duration::from_secs(5))
            .context("Timed out while creating the overlay window")?
        {
            Ok(()) => Ok(Self { sender }),
            Err(error) => Err(anyhow!("Failed to create the overlay window: {error}")),
        }
    }

    pub fn show(&self, state: OverlayState) -> anyhow::Result<()> {
        self.sender
            .send(OverlayCommand::Show(state))
            .context("Overlay thread is not available")
    }

    pub fn hide(&self) -> anyhow::Result<()> {
        self.sender
            .send(OverlayCommand::Hide)
            .context("Overlay thread is not available")
    }

    pub fn set_volume(&self, volume: f32) -> anyhow::Result<()> {
        self.sender
            .send(OverlayCommand::Volume(volume.clamp(0.0, 1.0)))
            .context("Overlay thread is not available")
    }
}

fn create_window() -> anyhow::Result<HWND> {
    let class_name = wide("VeeTypeOverlay");
    let window_title = wide(OverlayState::Listening.label());

    unsafe {
        let instance = GetModuleHandleW(null());
        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: 0,
            hCursor: 0,
            hbrBackground: 0,
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };

        if RegisterClassW(&window_class) == 0 {
            return Err(anyhow!(
                "RegisterClassW failed with Windows error {}",
                GetLastError()
            ));
        }

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name.as_ptr(),
            window_title.as_ptr(),
            WS_POPUP,
            0,
            0,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            0,
            0,
            instance,
            null(),
        );
        if hwnd == 0 {
            return Err(anyhow!(
                "CreateWindowExW failed with Windows error {}",
                GetLastError()
            ));
        }

        if SetLayeredWindowAttributes(hwnd, 0, 238, LWA_ALPHA) == 0 {
            return Err(anyhow!(
                "SetLayeredWindowAttributes failed with Windows error {}",
                GetLastError()
            ));
        }

        let region = CreateRoundRectRgn(0, 0, WINDOW_WIDTH, WINDOW_HEIGHT, 28, 28);
        if region != 0 && SetWindowRgn(hwnd, region, 1) == 0 {
            DeleteObject(region);
            return Err(anyhow!(
                "SetWindowRgn failed with Windows error {}",
                GetLastError()
            ));
        }

        Ok(hwnd)
    }
}

fn run_message_loop(hwnd: HWND, receiver: Receiver<OverlayCommand>) -> anyhow::Result<()> {
    let (mut target_x, mut target_y) = (0, 0);
    let mut current_y = 0.0_f32;
    let mut visible = false;
    let mut next_frame = Instant::now();

    loop {
        unsafe {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, 0, 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    DestroyWindow(hwnd);
                    return Ok(());
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        match receiver.recv_timeout(Duration::from_millis(16)) {
            Ok(OverlayCommand::Show(state)) => {
                let position = overlay_position();
                target_x = position.0;
                target_y = position.1;
                if !visible {
                    current_y = (target_y + 12) as f32;
                    visible = true;
                }
                VISUAL_STATE.with(|visual| {
                    let mut visual = visual.borrow_mut();
                    if visual.state != state {
                        visual.phase = 0.0;
                    }
                    visual.state = state;
                    visual.volume = 0.0;
                });

                unsafe {
                    if SetLayeredWindowAttributes(hwnd, 0, VISIBLE_ALPHA as u8, LWA_ALPHA) == 0 {
                        return Err(anyhow!(
                            "SetLayeredWindowAttributes failed with Windows error {}",
                            GetLastError()
                        ));
                    }
                    if SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        target_x,
                        current_y.round() as i32,
                        WINDOW_WIDTH,
                        WINDOW_HEIGHT,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    ) == 0
                    {
                        return Err(anyhow!(
                            "SetWindowPos failed with Windows error {}",
                            GetLastError()
                        ));
                    }
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    InvalidateRect(hwnd, null(), 0);
                    UpdateWindow(hwnd);
                }
            }
            Ok(OverlayCommand::Hide) => {
                visible = false;
                unsafe {
                    if SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA) == 0 {
                        return Err(anyhow!(
                            "SetLayeredWindowAttributes failed with Windows error {}",
                            GetLastError()
                        ));
                    }
                    ShowWindow(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE);
                }
            }
            Ok(OverlayCommand::Volume(volume)) => {
                VISUAL_STATE.with(|visual| visual.borrow_mut().volume = volume);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                unsafe {
                    DestroyWindow(hwnd);
                }
                return Ok(());
            }
        }

        if visible && Instant::now() >= next_frame {
            let y_target = target_y as f32;
            current_y += (y_target - current_y) * 0.24;
            if (y_target - current_y).abs() < 1.0 {
                current_y = y_target;
            }

            unsafe {
                if SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    target_x,
                    current_y.round() as i32,
                    WINDOW_WIDTH,
                    WINDOW_HEIGHT,
                    SWP_NOACTIVATE,
                ) == 0
                {
                    return Err(anyhow!(
                        "SetWindowPos failed with Windows error {}",
                        GetLastError()
                    ));
                }
                InvalidateRect(hwnd, null(), 0);
            }
            VISUAL_STATE.with(|visual| {
                let mut visual = visual.borrow_mut();
                visual.phase = (visual.phase + 0.22) % std::f32::consts::TAU;
            });
            next_frame = Instant::now() + Duration::from_millis(16);
        }
    }
}

fn overlay_position() -> (i32, i32) {
    unsafe {
        let foreground = GetForegroundWindow();
        let monitor = MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
        let mut monitor_info: MONITORINFO = zeroed();
        monitor_info.cbSize = size_of::<MONITORINFO>() as u32;

        if monitor != 0 && GetMonitorInfoW(monitor, &mut monitor_info) != 0 {
            let work = monitor_info.rcWork;
            (
                work.left + (work.right - work.left - WINDOW_WIDTH) / 2,
                work.bottom - WINDOW_HEIGHT - 28,
            )
        } else {
            (
                (GetSystemMetrics(SM_CXSCREEN) - WINDOW_WIDTH) / 2,
                GetSystemMetrics(SM_CYSCREEN) - WINDOW_HEIGHT - 72,
            )
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == WM_PAINT {
        paint_window(hwnd);
        0
    } else {
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

unsafe fn paint_window(hwnd: HWND) {
    let mut paint: windows_sys::Win32::Graphics::Gdi::PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut paint);
    let bounds = RECT {
        left: 0,
        top: 0,
        right: WINDOW_WIDTH,
        bottom: WINDOW_HEIGHT,
    };

    let visual = VISUAL_STATE.with(|state| *state.borrow());
    let accent = visual.state.accent();

    let background = CreateSolidBrush(rgb(18, 23, 33));
    if background != 0 {
        FillRect(hdc, &bounds, background);
    }

    let pen = CreatePen(PS_SOLID, 1, rgb(79, 93, 112));
    if pen != 0 {
        let previous_pen = SelectObject(hdc, pen);
        let previous_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        RoundRect(hdc, 1, 1, WINDOW_WIDTH - 1, WINDOW_HEIGHT - 1, 30, 30);
        SelectObject(hdc, previous_brush);
        SelectObject(hdc, previous_pen);
        DeleteObject(pen);
    }

    let indicator_brush = CreateSolidBrush(accent);
    if indicator_brush != 0 {
        let previous_brush = SelectObject(hdc, indicator_brush);
        match visual.state {
            OverlayState::Listening => {
                let radius = 6.0 + visual.volume * 8.0 + visual.phase.sin().abs() * 1.5;
                let center_x = 30.0;
                let center_y = 29.0;
                Ellipse(
                    hdc,
                    (center_x - radius - 5.0) as i32,
                    (center_y - radius - 5.0) as i32,
                    (center_x + radius + 5.0) as i32,
                    (center_y + radius + 5.0) as i32,
                );
                let radius = radius.min(13.0);
                Ellipse(
                    hdc,
                    (center_x - radius) as i32,
                    (center_y - radius) as i32,
                    (center_x + radius) as i32,
                    (center_y + radius) as i32,
                );

                let bar_brush = CreateSolidBrush(rgb(255, 142, 151));
                if bar_brush != 0 {
                    let previous = SelectObject(hdc, bar_brush);
                    for index in 0..4 {
                        let wave = (visual.phase + index as f32 * 0.8).sin().abs();
                        let height = 4 + (visual.volume * 12.0 + wave * 5.0) as i32;
                        let x = 62 + index * 5;
                        let bar = RECT {
                            left: x,
                            top: 29 - height / 2,
                            right: x + 3,
                            bottom: 29 + height / 2,
                        };
                        FillRect(hdc, &bar, bar_brush);
                    }
                    SelectObject(hdc, previous);
                    DeleteObject(bar_brush);
                }
            }
            OverlayState::Idle => {
                Ellipse(hdc, 23, 22, 37, 36);
            }
        }
        SelectObject(hdc, previous_brush);
        DeleteObject(indicator_brush);
    }

    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, rgb(238, 243, 250));
    let title = wide(visual.state.label());
    let mut text_bounds = RECT {
        left: 88,
        top: 0,
        right: WINDOW_WIDTH - 18,
        bottom: WINDOW_HEIGHT,
    };
    DrawTextW(
        hdc,
        title.as_ptr(),
        visual.state.label().encode_utf16().count() as i32,
        &mut text_bounds,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
    EndPaint(hwnd, &paint);
    if background != 0 {
        DeleteObject(background);
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
}
