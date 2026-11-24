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
//!   U UID          the account's login name, a tab, and its GECOS field
//!                  (getpwuid_r, so network and systemd-homed accounts too)
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
    fn getpwuid_r(
        uid: u32,
        entry: *mut Passwd,
        buffer: *mut std::ffi::c_char,
        length: usize,
        result: *mut *mut Passwd,
    ) -> i32;
}

/// glibc's struct passwd
#[repr(C)]
struct Passwd {
    name: *const std::ffi::c_char,
    password: *const std::ffi::c_char,
    uid: u32,
    gid: u32,
    gecos: *const std::ffi::c_char,
    home: *const std::ffi::c_char,
    shell: *const std::ffi::c_char,
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
            Ok(bytes) => {
                writeln!(output, "0 {}", bytes.len()).and_then(|()| output.write_all(bytes))
            }
            // EIO for an error without a number
            Err(error) => writeln!(output, "{} 0", error.raw_os_error().unwrap_or(5)),
        };
        if written.and_then(|()| output.flush()).is_err() {
            break;
        }
    }
}

fn account(uid: u32) -> io::Result<Vec<u8>> {
    // SAFETY: an all-zero passwd is a valid out-parameter, and every pointer
    // is valid for the lengths given; getpwuid_r fills the strings inside
    // the buffer, which outlives their use here.
    unsafe {
        let mut entry: Passwd = std::mem::zeroed();
        let mut result: *mut Passwd = std::ptr::null_mut();
        let mut buffer = vec![0 as std::ffi::c_char; 16384];
        let status = getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        );
        if status != 0 || result.is_null() {
            return Err(io::Error::from_raw_os_error(if status != 0 {
                status
            } else {
                2
            }));
        }
        let mut answer = std::ffi::CStr::from_ptr(entry.name).to_bytes().to_vec();
        answer.push(b'\t');
        if !entry.gecos.is_null() {
            answer.extend_from_slice(std::ffi::CStr::from_ptr(entry.gecos).to_bytes());
        }
        Ok(answer)
    }
}

fn answer(line: &[u8]) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::from_raw_os_error(22);
    let (op, argument) = match line.iter().position(|&b| b == b'\t') {
        Some(at) => (&line[..at], &line[at + 1..]),
        None => return Err(invalid()),
    };
    match op {
        b"U" => {
            let uid: u32 = std::str::from_utf8(argument)
                .ok()
                .and_then(|text| text.parse().ok())
                .ok_or_else(invalid)?;
            account(uid)
        }
        b"K" | b"N" => {
            let text = std::str::from_utf8(argument).map_err(|_| invalid())?;
            let mut words = text.split(' ');
            let pid: i32 = words
                .next()
                .and_then(|w| w.parse().ok())
                .ok_or_else(invalid)?;
            let value: i32 = words
                .next()
                .and_then(|w| w.parse().ok())
                .ok_or_else(invalid)?;
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
            if result == 0 {
                Ok(Vec::new())
            } else {
                Err(io::Error::last_os_error())
            }
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
                b"E" => Ok(if path.exists() {
                    b"1".to_vec()
                } else {
                    b"0".to_vec()
                }),
                _ => Err(invalid()),
            }
        }
    }
}
