//! Global hotkeys: the keys a hotkey can be (`KeyCode`, Windows' virtual key codes, with livesplit-hotkey 0.5's names,
//! which configs use), and a thread polling the bound keys (`Hotkeys`) that calls a key's callback when it's pressed,
//! then again while it's held, at the keyboard's repeat delay and rate (as the key repeat a keyboard hook sees), saying
//! which calls are repeats.
//! It polls rather than hooking the keyboard: livesplit-hotkey's hook could be called again from inside itself (while
//! another program's hook was slow), which panicked and aborted the exe.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use winapi::um::winuser::{GetAsyncKeyState, SystemParametersInfoW, SPI_GETKEYBOARDDELAY, SPI_GETKEYBOARDSPEED};

// How often the keys are checked
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// A key, by its Windows virtual key code (from livesplit-hotkey 0.5, MIT/Apache-2.0)
#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone, Serialize, Deserialize)]
pub enum KeyCode {
    LButton = 0x01,
    RButton = 0x02,
    Cancel = 0x03,
    MButton = 0x04,
    XButton1 = 0x05,
    XButton2 = 0x06,
    Back = 0x08,
    Tab = 0x09,
    Clear = 0x0C,
    Return = 0x0D,
    Shift = 0x10,
    Control = 0x11,
    Menu = 0x12,
    Pause = 0x13,
    Capital = 0x14,
    Kana = 0x15,
    Junja = 0x17,
    Final = 0x18,
    Kanji = 0x19,
    Escape = 0x1B,
    Convert = 0x1C,
    NonConvert = 0x1D,
    Accept = 0x1E,
    ModeChange = 0x1F,
    Space = 0x20,
    Prior = 0x21,
    Next = 0x22,
    End = 0x23,
    Home = 0x24,
    Left = 0x25,
    Up = 0x26,
    Right = 0x27,
    Down = 0x28,
    Select = 0x29,
    Print = 0x2A,
    Execute = 0x2B,
    Snapshot = 0x2C,
    Insert = 0x2D,
    Delete = 0x2E,
    Help = 0x2F,
    D0 = 0x30,
    D1 = 0x31,
    D2 = 0x32,
    D3 = 0x33,
    D4 = 0x34,
    D5 = 0x35,
    D6 = 0x36,
    D7 = 0x37,
    D8 = 0x38,
    D9 = 0x39,
    A = 0x41,
    B = 0x42,
    C = 0x43,
    D = 0x44,
    E = 0x45,
    F = 0x46,
    G = 0x47,
    H = 0x48,
    I = 0x49,
    J = 0x4A,
    K = 0x4B,
    L = 0x4C,
    M = 0x4D,
    N = 0x4E,
    O = 0x4F,
    P = 0x50,
    Q = 0x51,
    R = 0x52,
    S = 0x53,
    T = 0x54,
    U = 0x55,
    V = 0x56,
    W = 0x57,
    X = 0x58,
    Y = 0x59,
    Z = 0x5A,
    LeftWin = 0x5B,
    RightWin = 0x5C,
    Apps = 0x5D,
    Sleep = 0x5F,
    NumPad0 = 0x60,
    NumPad1 = 0x61,
    NumPad2 = 0x62,
    NumPad3 = 0x63,
    NumPad4 = 0x64,
    NumPad5 = 0x65,
    NumPad6 = 0x66,
    NumPad7 = 0x67,
    NumPad8 = 0x68,
    NumPad9 = 0x69,
    Multiply = 0x6A,
    Add = 0x6B,
    Separator = 0x6C,
    Subtract = 0x6D,
    Decimal = 0x6E,
    Divide = 0x6F,
    F1 = 0x70,
    F2 = 0x71,
    F3 = 0x72,
    F4 = 0x73,
    F5 = 0x74,
    F6 = 0x75,
    F7 = 0x76,
    F8 = 0x77,
    F9 = 0x78,
    F10 = 0x79,
    F11 = 0x7A,
    F12 = 0x7B,
    F13 = 0x7C,
    F14 = 0x7D,
    F15 = 0x7E,
    F16 = 0x7F,
    F17 = 0x80,
    F18 = 0x81,
    F19 = 0x82,
    F20 = 0x83,
    F21 = 0x84,
    F22 = 0x85,
    F23 = 0x86,
    F24 = 0x87,
    NumLock = 0x90,
    Scroll = 0x91,
    LeftShift = 0xA0,
    RightShift = 0xA1,
    LeftControl = 0xA2,
    RightControl = 0xA3,
    LeftMenu = 0xA4,
    RightMenu = 0xA5,
    BrowserBack = 0xA6,
    BrowserForward = 0xA7,
    BrowserRefresh = 0xA8,
    BrowserStop = 0xA9,
    BrowserSearch = 0xAA,
    BrowserFavorites = 0xAB,
    BrowserHome = 0xAC,
    VolumeMute = 0xAD,
    VolumeDown = 0xAE,
    VolumeUp = 0xAF,
    MediaNextTrack = 0xB0,
    MediaPrevTrack = 0xB1,
    MediaStop = 0xB2,
    MediaPlayPause = 0xB3,
    LaunchMail = 0xB4,
    LaunchMediaSelect = 0xB5,
    LaunchApp1 = 0xB6,
    LaunchApp2 = 0xB7,
    Oem1 = 0xBA,
    OemPlus = 0xBB,
    OemComma = 0xBC,
    OemMinus = 0xBD,
    OemPeriod = 0xBE,
    Oem2 = 0xBF,
    Oem3 = 0xC0,
    Oem4 = 0xDB,
    Oem5 = 0xDC,
    Oem6 = 0xDD,
    Oem7 = 0xDE,
    Oem8 = 0xDF,
    Oem102 = 0xE2,
    ProcessKey = 0xE5,
    Packet = 0xE7,
    Attn = 0xF6,
    CrSel = 0xF7,
    ExSel = 0xF8,
    ErEof = 0xF9,
    Play = 0xFA,
    Zoom = 0xFB,
    NoName = 0xFC,
    Pa1 = 0xFD,
    OemClear = 0xFE,
}

// Called with whether it's a repeat (the key's still held) rather than a press
pub type Callback = Box<dyn FnMut(bool) + Send + 'static>;

/// Calls each key's callback when it's pressed and while it's held, until dropped
pub struct Hotkeys {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Hotkeys {
    pub fn start(callbacks: HashMap<KeyCode, Callback>) -> Hotkeys {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            thread::spawn(move || poll(callbacks, &stop))
        };
        Hotkeys { stop, thread: Some(thread) }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn poll(mut callbacks: HashMap<KeyCode, Callback>, stop: &AtomicBool) {
    let repeat = Repeat::from_system();
    // When each held key next repeats
    let mut held = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        for (&key, callback) in &mut callbacks {
            if let Some(repeated) = repeat.fires(&mut held, key, is_down(key), now) {
                callback(repeated);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn is_down(key: KeyCode) -> bool {
    unsafe { GetAsyncKeyState(key as i32) as u16 & 0x8000 != 0 }
}

// Windows' key repeat: how long a key is held before it repeats, then how often
struct Repeat {
    delay: Duration,
    interval: Duration,
}

impl Repeat {
    // The keyboard settings: a delay of 0 to 3 (250ms to 1s) and a speed of 0 to 31 (about 2.5 to 30 a second)
    fn from_system() -> Repeat {
        let setting = |action| {
            let mut value: u32 = 0;
            let read = unsafe { SystemParametersInfoW(action, 0, &mut value as *mut u32 as *mut _, 0) } != 0;
            read.then(|| value)
        };
        let delay = setting(SPI_GETKEYBOARDDELAY).unwrap_or(1).min(3);
        let speed = setting(SPI_GETKEYBOARDSPEED).unwrap_or(31).min(31);
        Repeat {
            delay: Duration::from_millis(250 * (delay as u64 + 1)),
            interval: Duration::from_secs_f32(1.0 / (2.5 + speed as f32 * 27.5 / 31.0)),
        }
    }

    // Whether a key's callback is called now, and if so whether it's a repeat: when it's pressed, and once it's been
    // held for the delay, every interval. `held` is when each held key next repeats.
    fn fires(&self, held: &mut HashMap<KeyCode, Instant>, key: KeyCode, down: bool, now: Instant) -> Option<bool> {
        if !down {
            held.remove(&key);
            return None;
        }
        match held.get_mut(&key) {
            None => {
                held.insert(key, now + self.delay);
                Some(false)
            }
            Some(next) if now >= *next => {
                *next = now + self.interval;
                Some(true)
            }
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_keys_repeat_after_the_delay() {
        let repeat = Repeat { delay: Duration::from_millis(500), interval: Duration::from_millis(30) };
        let mut held = HashMap::new();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut fires = |down, ms| repeat.fires(&mut held, KeyCode::W, down, at(ms));
        assert_eq!(fires(true, 0), Some(false));
        assert_eq!(fires(true, 5), None);
        assert_eq!(fires(true, 495), None);
        assert_eq!(fires(true, 500), Some(true));
        assert_eq!(fires(true, 520), None);
        assert_eq!(fires(true, 530), Some(true));
        assert_eq!(fires(false, 535), None);
        assert_eq!(fires(true, 540), Some(false));
        assert_eq!(fires(true, 600), None);
    }

    #[test]
    fn configs_keep_livesplit_hotkeys_names() {
        let keys: Vec<KeyCode> = serde_json::from_str(r#"["F5", "W", "Space", "NumPad0", "D1", "Left", "LeftShift"]"#).unwrap();
        assert_eq!(keys[3] as u8, 0x60);
        assert_eq!(format!("{:?}", keys[4]), "D1");
    }
}
