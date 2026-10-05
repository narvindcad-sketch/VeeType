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
    BeginPaint, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, EndPaint,
    FillRect, GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromWindow, RoundRect,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, UpdateWindow, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_PEN, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetSystemMetrics, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WM_PAINT, WM_QUIT,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

const WINDOW_WIDTH: i32 = 336;
const WINDOW_HEIGHT: i32 = 78;
const VISIBLE_ALPHA: f32 = 246.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    Idle,
    Listening,
    Processing,
}

impl OverlayState {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready when you are",
            Self::Listening => "Listening to your voice",
            Self::Processing => "Polishing your text",
        }
    }

    fn indicator(self) -> &'static str {
        match self {
            Self::Idle => "READY",
            Self::Listening => "LIVE",
            Self::Processing => "REFINING",
        }
    }

    fn accent(self) -> u32 {
        match self {
            Self::Idle => rgb(165, 156, 184),
            Self::Listening => rgb(126, 109, 255),
            Self::Processing => rgb(166, 135, 255),
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
    let window_title = wide("VeeType");

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

        let region = CreateRoundRectRgn(0, 0, WINDOW_WIDTH, WINDOW_HEIGHT, 38, 38);
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

    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let visual = VISUAL_STATE.with(|state| *state.borrow());
    let background = CreateSolidBrush(rgb(23, 20, 31));
    if background != 0 {
        FillRect(hdc, &bounds, background);
    }

    let outer_glow = CreateSolidBrush(rgb(37, 30, 57));
    if outer_glow != 0 {
        let previous_brush = SelectObject(hdc, outer_glow);
        Ellipse(hdc, 6, 6, 84, 72);
        SelectObject(hdc, previous_brush);
        DeleteObject(outer_glow);
    }

    let logo_tile = CreateSolidBrush(rgb(84, 70, 223));
    if logo_tile != 0 {
        let previous_brush = SelectObject(hdc, logo_tile);
        RoundRect(hdc, 17, 13, 67, 65, 18, 18);
        SelectObject(hdc, previous_brush);
        DeleteObject(logo_tile);
    }

    let logo_mark = CreateSolidBrush(rgb(245, 242, 255));
    if logo_mark != 0 {
        let previous_brush = SelectObject(hdc, logo_mark);
        let mark_heights = [10, 21, 15, 24, 12];
        for (index, height) in mark_heights.iter().enumerate() {
            let x = 27 + index as i32 * 6;
            RoundRect(hdc, x, 39 - height / 2, x + 3, 39 + height / 2, 3, 3);
        }
        SelectObject(hdc, previous_brush);
        DeleteObject(logo_mark);
    }

    match visual.state {
        OverlayState::Listening => {
            let pulse = visual.phase.sin().abs() * 0.25;
            let dynamic_volume = (visual.volume * 14.0).max(pulse);
            let base_heights = [8.0_f32, 15.0, 10.0, 19.0, 12.0, 17.0, 8.0];
            for (index, base_height) in base_heights.iter().enumerate() {
                let wave = ((visual.phase + index as f32 * 0.8).sin() + 1.0) * 0.5;
                let height =
                    (base_height * (0.35 + dynamic_volume + wave * 0.45)).clamp(4.0, 28.0) as i32;
                let x = 268 + index as i32 * 7;
                let color = if index % 2 == 0 {
                    rgb(126, 109, 255)
                } else {
                    rgb(181, 163, 255)
                };
                let brush = CreateSolidBrush(color);
                if brush != 0 {
                    let previous_brush = SelectObject(hdc, brush);
                    RoundRect(hdc, x, 39 - height / 2, x + 4, 39 + height / 2, 4, 4);
                    SelectObject(hdc, previous_brush);
                    DeleteObject(brush);
                }
            }
        }
        OverlayState::Processing => {
            let pulse = (visual.phase.sin() + 1.0) * 0.5;
            let radius = 4.0 + pulse * 3.0;
            let glow = CreateSolidBrush(rgb(70, 51, 112));
            if glow != 0 {
                let previous_brush = SelectObject(hdc, glow);
                Ellipse(
                    hdc,
                    290 - (radius + 7.0) as i32,
                    39 - (radius + 7.0) as i32,
                    290 + (radius + 7.0) as i32,
                    39 + (radius + 7.0) as i32,
                );
                SelectObject(hdc, previous_brush);
                DeleteObject(glow);
            }
            let brush = CreateSolidBrush(rgb(181, 163, 255));
            if brush != 0 {
                let previous_brush = SelectObject(hdc, brush);
                Ellipse(
                    hdc,
                    290 - radius as i32,
                    39 - radius as i32,
                    290 + radius as i32,
                    39 + radius as i32,
                );
                SelectObject(hdc, previous_brush);
                DeleteObject(brush);
            }
            for (index, x) in [270, 310].iter().enumerate() {
                let dot_radius = 3 + ((visual.phase + index as f32).sin().abs() * 2.0) as i32;
                let brush = CreateSolidBrush(rgb(126, 109, 255));
                if brush != 0 {
                    let previous_brush = SelectObject(hdc, brush);
                    Ellipse(
                        hdc,
                        *x - dot_radius,
                        39 - dot_radius,
                        *x + dot_radius,
                        39 + dot_radius,
                    );
                    SelectObject(hdc, previous_brush);
                    DeleteObject(brush);
                }
            }
        }
        OverlayState::Idle => {
            let brush = CreateSolidBrush(visual.state.accent());
            if brush != 0 {
                let previous_brush = SelectObject(hdc, brush);
                Ellipse(hdc, 283, 32, 297, 46);
                SelectObject(hdc, previous_brush);
                DeleteObject(brush);
            }
        }
    }
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, rgb(255, 248, 255));
    let brand = wide("VeeType");
    let mut brand_bounds = RECT {
        left: 83,
        top: 9,
        right: 246,
        bottom: 41,
    };
    DrawTextW(
        hdc,
        brand.as_ptr(),
        "VeeType".encode_utf16().count() as i32,
        &mut brand_bounds,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
    SetTextColor(hdc, rgb(196, 185, 211));
    let status = wide(visual.state.label());
    let mut status_bounds = RECT {
        left: 83,
        top: 37,
        right: 253,
        bottom: 68,
    };
    DrawTextW(
        hdc,
        status.as_ptr(),
        visual.state.label().encode_utf16().count() as i32,
        &mut status_bounds,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
    SetTextColor(hdc, visual.state.accent());
    let indicator = wide(visual.state.indicator());
    let mut indicator_bounds = RECT {
        left: 258,
        top: 59,
        right: WINDOW_WIDTH - 17,
        bottom: 74,
    };
    DrawTextW(
        hdc,
        indicator.as_ptr(),
        visual.state.indicator().encode_utf16().count() as i32,
        &mut indicator_bounds,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(hdc, previous_pen);
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
