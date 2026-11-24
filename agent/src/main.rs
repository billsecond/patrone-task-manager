//! Task Manager's helper for its Flatpak. The app starts it on the computer
//! with `flatpak-spawn --host` and asks it, over standard input and output,
//! for what the sandbox can't see: the computer's /proc and /sys, and
//! signals and priorities for its processes (src/host.rs in the app).
//!
//! Each request is one line, an operation, a tab and its argument:
//!
//!   R PATH         a file's contents
//!   D PATH         a folder's entries, each name ending in a NUL
//!   L PATH         where a link points
//!   C PATH         the path with its links resolved
//!   E PATH         whether it exists (answers "1" or "0")
//!   K PID SIGNAL   kill(2)
//!   N PID NICE     setpriority(2), for a process
//!
//! Each answer is a line, "0 LENGTH", then that many bytes; or "ERRNO 0"
//! for a failure. Only /proc and /sys are read. It ends when its input
//! does, which is when the app goes.

use std::io::{self, BufRead, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn setpriority(which: i32, who: u32, priority: i32) -> i32;
}

const PRIO_PROCESS: i32 = 0;

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let answer = answer(&line);
        let written = match &answer {
            Ok(bytes) => writeln!(output, "0 {}", bytes.len()).and_then(|()| output.write_all(bytes)),
            // EIO for an error without a number
            Err(error) => writeln!(output, "{} 0", error.raw_os_error().unwrap_or(5)),
        };
        if written.and_then(|()| output.flush()).is_err() {
            break;
        }
    }
}

fn answer(line: &[u8]) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::from_raw_os_error(22);
    let (op, argument) = match line.iter().position(|&b| b == b'\t') {
        Some(at) => (&line[..at], &line[at + 1..]),
        None => return Err(invalid()),
    };
    match op {
        b"K" | b"N" => {
            let text = std::str::from_utf8(argument).map_err(|_| invalid())?;
            let mut words = text.split(' ');
            let pid: i32 = words.next().and_then(|w| w.parse().ok()).ok_or_else(invalid)?;
            let value: i32 = words.next().and_then(|w| w.parse().ok()).ok_or_else(invalid)?;
            if pid <= 0 {
                return Err(invalid());
            }
            // SAFETY: plain system calls on integers.
            let result = unsafe {
                if op == b"K" {
                    kill(pid, value)
                } else {
                    setpriority(PRIO_PROCESS, pid as u32, value)
                }
            };
            if result == 0 { Ok(Vec::new()) } else { Err(io::Error::last_os_error()) }
        }
        _ => {
            let path = Path::new(std::ffi::OsStr::from_bytes(argument));
            if !(path.starts_with("/proc") || path.starts_with("/sys")) {
                return Err(io::Error::from_raw_os_error(13));
            }
            match op {
                b"R" => {
                    let mut bytes = Vec::new();
                    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
                    Ok(bytes)
                }
                b"D" => {
                    let mut names = Vec::new();
                    for entry in std::fs::read_dir(path)? {
                        names.extend_from_slice(entry?.file_name().as_bytes());
                        names.push(0);
                    }
                    Ok(names)
                }
                b"L" => Ok(std::fs::read_link(path)?.into_os_string().into_vec()),
                b"C" => Ok(std::fs::canonicalize(path)?.into_os_string().into_vec()),
                b"E" => Ok(if path.exists() { b"1".to_vec() } else { b"0".to_vec() }),
                _ => Err(invalid()),
            }
        }
    }
}
