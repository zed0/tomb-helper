//! The log file, `tomb-helper.log` next to the exe, for reporting crashes: the exe and the overlays injected into the
//! game (shared with the DLL, which finds it next to itself, as it's injected from beside the exe) all append to it.
//! Each line starts with the time and where it came from, e.g. `[tomb-helper 1234]` or `[overlay in ROTTR.exe 5678]`
//! (the process id), then `!` if it went to stderr. Once it reaches `MAX_SIZE` it's moved to
//! `tomb-helper.log.1` (replacing the last one) and a new one started, so the two together stay under 100MB.
//!
//! The exe's `println!`/`eprintln!` (and `print!`/`eprint!`) are this module's macros, which print as usual and log
//! too; the overlay logs through `tracing` (`make_writer`). `log_panics` logs panics with a backtrace.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const LOG_FILE: &str = "tomb-helper.log";
// The current file's limit; with the previous one kept, 100MB in all
const MAX_SIZE: u64 = 50 * 1024 * 1024;
// How long to keep the file open before opening it again, in case another process moved it
const REOPEN_AFTER: Duration = Duration::from_secs(1);

struct Log {
    path: Option<PathBuf>,
    // e.g. "tomb-helper[1234]"
    source: String,
    file: Option<(File, Instant)>,
    // What's been printed of a line that hasn't ended yet (e.g. a prompt), per stream
    partial: [String; 2],
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

#[derive(Clone, Copy)]
pub enum Stream {
    Out = 0,
    Err = 1,
}

// The exe or DLL this is compiled into
#[cfg(windows)]
fn this_module() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use winapi::um::libloaderapi::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    unsafe {
        // Anything in this module will do to find it
        static MARKER: u8 = 0;
        let mut module = std::ptr::null_mut();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if GetModuleHandleExW(flags, &MARKER as *const u8 as *const u16, &mut module) == 0 {
            return None;
        }
        let mut name = vec![0u16; 32768];
        let length = GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) as usize;
        if length == 0 || length >= name.len() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsString::from_wide(&name[..length])))
    }
}

#[cfg(not(windows))]
fn this_module() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

fn timestamp() -> String {
    #[cfg(windows)]
    unsafe {
        let mut time = std::mem::zeroed();
        winapi::um::sysinfoapi::GetLocalTime(&mut time);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond, time.wMilliseconds
        )
    }
    #[cfg(not(windows))]
    {
        format!("{:?}", std::time::SystemTime::now())
    }
}

impl Log {
    fn new() -> Self {
        let module = this_module();
        let exe = std::env::current_exe().ok();
        // The exe, or an overlay injected into a game: "tomb-helper" or "overlay in ROTTR.exe"
        let name = if module.is_none() || module == exe {
            "tomb-helper".to_string()
        } else {
            let game = exe.as_ref().and_then(|e| e.file_name()).map_or("?".into(), |n| n.to_string_lossy());
            format!("overlay in {}", game)
        };
        Log {
            path: module.map(|m| m.with_file_name(LOG_FILE)),
            source: format!("[{} {}]", name, std::process::id()),
            file: None,
            partial: [String::new(), String::new()],
        }
    }

    // The file to append to, moving it aside first if it's full
    fn file(&mut self) -> Option<&mut File> {
        let path = self.path.as_ref()?;
        let stale = match &self.file {
            Some((file, opened)) => {
                opened.elapsed() > REOPEN_AFTER || file.metadata().map_or(true, |m| m.len() >= MAX_SIZE)
            }
            None => true,
        };
        if stale {
            self.file = None;
            if std::fs::metadata(path).map_or(false, |m| m.len() >= MAX_SIZE) {
                // Fails if another process just did it, which is fine
                let _ = std::fs::rename(path, path.with_file_name(format!("{}.1", LOG_FILE)));
            }
            // Opened to share with the other processes logging (std's default share mode includes delete, so another
            // can move it)
            let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
            self.file = Some((file, Instant::now()));
        }
        self.file.as_mut().map(|(file, _)| file)
    }

    fn write(&mut self, stream: Stream, text: &str) {
        let partial = &mut self.partial[stream as usize];
        partial.push_str(text);
        let Some(end) = partial.rfind('\n') else { return };
        let lines: String = partial.drain(..=end).collect();
        let prefix = format!("{} {}{} ", timestamp(), self.source, if let Stream::Err = stream { " !" } else { "" });
        let mut out = String::with_capacity(lines.len() + prefix.len() * 2);
        for line in lines.lines() {
            out.push_str(&prefix);
            out.push_str(line.trim_end_matches('\r'));
            out.push_str("\r\n");
        }
        if let Some(file) = self.file() {
            // One write so lines from other processes don't land in the middle
            let _ = file.write_all(out.as_bytes());
        }
    }
}

/// Appends to the log; lines are written once they end
pub fn write(stream: Stream, text: &str) {
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.get_or_insert_with(Log::new).write(stream, text);
}

/// Prints to stdout or stderr and logs it (what `println!` and the rest here do)
pub fn print(stream: Stream, args: std::fmt::Arguments) {
    let text = args.to_string();
    let _ = match stream {
        Stream::Out => std::io::stdout().write_all(text.as_bytes()),
        Stream::Err => std::io::stderr().write_all(text.as_bytes()),
    };
    write(stream, &text);
}

/// Logs panics, with a backtrace, before the usual message
pub fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let backtrace = std::backtrace::Backtrace::force_capture();
        write(
            Stream::Err,
            &format!("thread '{}' {}\n{}\n", thread.name().unwrap_or("<unnamed>"), info, backtrace),
        );
        previous(info);
    }));
}

/// For `tracing_subscriber`'s `with_writer` (the overlay's): each event is logged as it's written
#[allow(dead_code)]
pub fn make_writer() -> LogWriter {
    LogWriter
}

#[allow(dead_code)]
pub struct LogWriter;

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        write(Stream::Out, &String::from_utf8_lossy(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Shadow std's, so the exe's output all goes to the log too (they're in scope in every module declared after this one)
#[allow(unused_macros)]
macro_rules! println {
    () => { $crate::logging::print($crate::logging::Stream::Out, format_args!("\n")) };
    ($($arg:tt)*) => { $crate::logging::print($crate::logging::Stream::Out, format_args!("{}\n", format_args!($($arg)*))) };
}

#[allow(unused_macros)]
macro_rules! print {
    ($($arg:tt)*) => { $crate::logging::print($crate::logging::Stream::Out, format_args!($($arg)*)) };
}

#[allow(unused_macros)]
macro_rules! eprintln {
    () => { $crate::logging::print($crate::logging::Stream::Err, format_args!("\n")) };
    ($($arg:tt)*) => { $crate::logging::print($crate::logging::Stream::Err, format_args!("{}\n", format_args!($($arg)*))) };
}

#[allow(unused_macros)]
macro_rules! eprint {
    ($($arg:tt)*) => { $crate::logging::print($crate::logging::Stream::Err, format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_prefixed_and_partial_ones_held_back() {
        let dir = std::env::temp_dir().join(format!("tomb-helper-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let _ = std::fs::remove_file(&path);
        let mut log = Log { path: Some(path.clone()), source: "[test 1]".into(), file: None, partial: Default::default() };
        log.write(Stream::Out, "Continue? ");
        log.write(Stream::Err, "an error\nand another\n");
        log.write(Stream::Out, "yes\n");
        log.file = None;
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with(" [test 1] ! an error"));
        assert!(lines[1].ends_with(" [test 1] ! and another"));
        assert!(lines[2].ends_with(" [test 1] Continue? yes"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
