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
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
    GetStockObject, InvalidateRect, MonitorFromWindow, RoundRect, SelectObject, UpdateWindow,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_PEN,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetSystemMetrics, PeekMessageW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, LWA_COLORKEY, MSG,
    PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE,
    WM_PAINT, WM_QUIT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP,
};

const WINDOW_WIDTH: i32 = 300;
const WINDOW_HEIGHT: i32 = 100;
const WAVEFORM_BAR_COUNT: usize = 15;
const TRANSPARENT_COLOR: u32 = 0x00FF00FF;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    Idle,
    Listening,
    Processing,
}

impl OverlayState {
    fn waveform_scale(self) -> f32 {
        match self {
            Self::Idle => 0.0,
            Self::Listening => 1.0,
            Self::Processing => 0.68,
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

        if SetLayeredWindowAttributes(hwnd, TRANSPARENT_COLOR, u8::MAX, LWA_COLORKEY) == 0 {
            return Err(anyhow!(
                "SetLayeredWindowAttributes failed with Windows error {}",
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

        // A hidden overlay has nothing to animate.  Waiting for a command
        // instead of polling at 60 Hz keeps the app effectively idle between
        // dictations.
        let wait = if visible {
            Duration::from_millis(16)
        } else {
            Duration::from_secs(60 * 60)
        };
        match receiver.recv_timeout(wait) {
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
                unsafe { ShowWindow(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE) };
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
                work.bottom - WINDOW_HEIGHT - 42,
            )
        } else {
            (
                (GetSystemMetrics(SM_CXSCREEN) - WINDOW_WIDTH) / 2,
                GetSystemMetrics(SM_CYSCREEN) - WINDOW_HEIGHT - 86,
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
    let background = CreateSolidBrush(TRANSPARENT_COLOR);
    if background != 0 {
        FillRect(hdc, &bounds, background);
    }

    draw_waveform(hdc, visual);
    SelectObject(hdc, previous_pen);
    EndPaint(hwnd, &paint);
    if background != 0 {
        DeleteObject(background);
    }
}

unsafe fn draw_waveform(hdc: windows_sys::Win32::Graphics::Gdi::HDC, visual: VisualState) {
    let heights = waveform_heights(visual.phase, visual.volume, visual.state);
    let color = CreateSolidBrush(rgb(86, 85, 214));
    if color == 0 {
        return;
    }

    let previous_brush = SelectObject(hdc, color);
    let center_x = WINDOW_WIDTH / 2;
    let center_y = WINDOW_HEIGHT / 2;
    for (index, height) in heights.into_iter().enumerate() {
        let x = center_x + (index as i32 - (WAVEFORM_BAR_COUNT as i32 - 1) / 2) * 14;
        RoundRect(
            hdc,
            x - 4,
            center_y - height / 2,
            x + 4,
            center_y + height / 2,
            8,
            8,
        );
    }
    SelectObject(hdc, previous_brush);
    DeleteObject(color);
}

fn waveform_heights(phase: f32, volume: f32, state: OverlayState) -> [i32; WAVEFORM_BAR_COUNT] {
    let scale = state.waveform_scale();
    std::array::from_fn(|index| {
        let wave = ((phase + index as f32 * 0.4).sin() * 0.5 + 0.5).abs();
        let microphone_boost = volume.clamp(0.0, 1.0) * wave * 18.0;
        ((15.0 + 60.0 * wave + microphone_boost) * scale)
            .round()
            .clamp(6.0, 90.0) as i32
    })
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
}

#[cfg(test)]
mod tests {
    use super::{waveform_heights, OverlayState, WAVEFORM_BAR_COUNT};

    #[test]
    fn waveform_has_fifteen_bounded_bars() {
        let heights = waveform_heights(1.2, 0.5, OverlayState::Listening);
        assert_eq!(heights.len(), WAVEFORM_BAR_COUNT);
        assert!(heights.iter().all(|height| (6..=90).contains(height)));
    }

    #[test]
    fn microphone_level_changes_waveform_height() {
        let quiet = waveform_heights(0.8, 0.0, OverlayState::Listening);
        let loud = waveform_heights(0.8, 1.0, OverlayState::Listening);
        assert!(loud.iter().zip(quiet).any(|(loud, quiet)| loud > &quiet));
    }
}
