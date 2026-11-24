//! The computer's /proc and /sys, and signals and priorities for its
//! processes. Installed as a package, that's std::fs and libc, as they are.
//! In the Flatpak the sandbox sees only its own processes, so these go to
//! the helper (agent/, installed as /app/libexec/patrone-task-manager-agent),
//! which runs on the computer through `flatpak-spawn --host` and answers
//! over a pipe. Only paths under /proc and /sys go to it; the rest, such as
//! the account's own autostart folder, are read here.

use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// Whether this is the Flatpak.
pub fn sandboxed() -> bool {
    static SANDBOXED: OnceLock<bool> = OnceLock::new();
    *SANDBOXED.get_or_init(|| Path::new("/.flatpak-info").exists())
}

/// A command line for gio::Subprocess: as given, or in the Flatpak,
/// `flatpak-spawn --host` running it on the computer.
pub fn argv(command: Vec<String>) -> Vec<String> {
    if sandboxed() {
        ["flatpak-spawn".to_owned(), "--host".to_owned()]
            .into_iter()
            .chain(command)
            .collect()
    } else {
        command
    }
}

/// Where to read one of the computer's own files under /usr or /etc: there,
/// or in the Flatpak, where its sandbox has them (/run/host, with host-os
/// and host-etc). The path the computer knows it by stays the one shown,
/// and the one given to its programs.
pub fn os_path(path: &Path) -> PathBuf {
    match path.strip_prefix("/") {
        Ok(rest) if sandboxed() && (path.starts_with("/usr") || path.starts_with("/etc")) => {
            Path::new("/run/host").join(rest)
        }
        _ => path.to_path_buf(),
    }
}

/// The account's own folder under its home (.config, .local/share): the
/// sandbox's XDG folders are the Flatpak's own, not the account's.
pub fn home_folder(xdg: PathBuf, under_home: &str) -> PathBuf {
    if sandboxed() {
        gtk::glib::home_dir().join(under_home)
    } else {
        xdg
    }
}

fn via_agent(path: &Path) -> bool {
    sandboxed() && (path.starts_with("/proc") || path.starts_with("/sys"))
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    let path = path.as_ref();
    if via_agent(path) {
        String::from_utf8(ask(b'R', path.as_os_str().as_bytes())?)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    } else {
        std::fs::read_to_string(path)
    }
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    if via_agent(path) {
        ask(b'R', path.as_os_str().as_bytes())
    } else {
        std::fs::read(path)
    }
}

/// A folder's entry, as std::fs::DirEntry gives its name and path.
pub struct Entry {
    name: OsString,
    path: PathBuf,
}

impl Entry {
    pub fn file_name(&self) -> OsString {
        self.name.clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
}

/// A folder's entries, shaped as std::fs::read_dir's (each an io::Result).
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<std::vec::IntoIter<io::Result<Entry>>> {
    let path = path.as_ref();
    let names: Vec<OsString> = if via_agent(path) {
        ask(b'D', path.as_os_str().as_bytes())?
            .split(|&b| b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| OsStr::from_bytes(name).to_os_string())
            .collect()
    } else {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<_>>()?
    };
    Ok(names
        .into_iter()
        .map(|name| {
            Ok(Entry {
                path: path.join(&name),
                name,
            })
        })
        .collect::<Vec<_>>()
        .into_iter())
}

pub fn read_link(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = path.as_ref();
    if via_agent(path) {
        Ok(PathBuf::from(OsString::from_vec(ask(
            b'L',
            path.as_os_str().as_bytes(),
        )?)))
    } else {
        std::fs::read_link(path)
    }
}

pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = path.as_ref();
    if via_agent(path) {
        Ok(PathBuf::from(OsString::from_vec(ask(
            b'C',
            path.as_os_str().as_bytes(),
        )?)))
    } else {
        std::fs::canonicalize(path)
    }
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    if via_agent(path) {
        ask(b'E', path.as_os_str().as_bytes()).is_ok_and(|answer| answer == b"1")
    } else {
        path.exists()
    }
}

/// An account's login name and GECOS field, from the computer's user
/// database (the sandbox's has few accounts); None outside the Flatpak,
/// where the caller looks it up itself.
pub fn account(uid: u32) -> Option<(String, String)> {
    if !sandboxed() {
        return None;
    }
    let answer = ask(b'U', uid.to_string().as_bytes()).ok()?;
    let text = String::from_utf8_lossy(&answer);
    let (login, gecos) = text.split_once('\t')?;
    Some((login.to_owned(), gecos.to_owned()))
}

/// kill(2): Ok, or the error.
pub fn kill(pid: i32, signal: i32) -> io::Result<()> {
    if sandboxed() {
        ask(b'K', format!("{pid} {signal}").as_bytes()).map(drop)
    } else {
        // SAFETY: a plain system call on integers.
        if unsafe { libc::kill(pid, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// setpriority(2), for a process: Ok, or the error.
pub fn set_nice(pid: i32, nice: i32) -> io::Result<()> {
    if sandboxed() {
        ask(b'N', format!("{pid} {nice}").as_bytes()).map(drop)
    } else {
        // SAFETY: a plain system call on integers.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

struct Agent {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

/// The helper on the computer: the Flatpak's own copy, by the path the
/// computer knows the app's files by (/.flatpak-info's app-path).
fn start() -> io::Result<Agent> {
    let info = std::fs::read_to_string("/.flatpak-info")?;
    let files = info
        .lines()
        .find_map(|line| line.strip_prefix("app-path="))
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let mut child = Command::new("flatpak-spawn")
        .args([
            "--host",
            &format!("{files}/libexec/patrone-task-manager-agent"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
    let output = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?,
    );
    Ok(Agent {
        _child: child,
        input,
        output,
    })
}

/// One request and its answer. A helper that's gone is started again, once.
fn ask(op: u8, argument: &[u8]) -> io::Result<Vec<u8>> {
    static AGENT: Mutex<Option<Agent>> = Mutex::new(None);
    let mut agent = AGENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for attempt in 0..2 {
        if agent.is_none() {
            *agent = Some(start()?);
        }
        let Some(running) = agent.as_mut() else {
            continue;
        };
        match exchange(running, op, argument) {
            Ok(answer) => return answer,
            Err(error) => {
                *agent = None;
                if attempt == 1 {
                    return Err(error);
                }
            }
        }
    }
    Err(io::Error::from(io::ErrorKind::BrokenPipe))
}

/// Outer error: the pipe failed. Inner: the helper's answer.
fn exchange(agent: &mut Agent, op: u8, argument: &[u8]) -> io::Result<io::Result<Vec<u8>>> {
    let mut request = Vec::with_capacity(argument.len() + 3);
    request.extend_from_slice(&[op, b'\t']);
    request.extend_from_slice(argument);
    request.push(b'\n');
    agent.input.write_all(&request)?;
    agent.input.flush()?;
    let mut header = String::new();
    if agent.output.read_line(&mut header)? == 0 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let mut fields = header.trim_end().split(' ');
    let (Some(code), Some(length)) = (
        fields.next().and_then(|f| f.parse::<i32>().ok()),
        fields.next().and_then(|f| f.parse::<usize>().ok()),
    ) else {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    };
    let mut body = vec![0; length];
    agent.output.read_exact(&mut body)?;
    Ok(if code == 0 {
        Ok(body)
    } else {
        Err(io::Error::from_raw_os_error(code))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_flatpak_its_std_fs() {
        if sandboxed() {
            return;
        }
        assert!(read_to_string("/proc/uptime").is_ok_and(|text| !text.is_empty()));
        assert!(
            read_dir("/proc")
                .is_ok_and(|mut entries| entries.any(|e| e.is_ok_and(|e| e.file_name() == "self")))
        );
        assert!(read_link("/proc/self/exe").is_ok());
        assert!(exists("/proc/self"));
        assert!(!exists("/proc/no-such-thing"));
    }
}
