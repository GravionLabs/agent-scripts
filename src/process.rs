//! The harness process: found on the `PATH`, started without a shell, waited for with a timeout,
//! and ended with its whole process tree.
//!
//! Unix: the child leads a session of its own. Its tree is every process of that session and every
//! descendant (an agent starts commands in groups and sessions of their own). SIGTERM first,
//! SIGKILL to what is left after [`GRACE`]. Windows: a job object that ends all its processes.

// The process calls of the operating system have no safe form in std.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The time between SIGTERM and SIGKILL.
pub(crate) const GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
/// How long the output of an ended tree may take to arrive. Only a process that escaped the tree
/// keeps a pipe open longer.
const DRAIN: Duration = Duration::from_secs(1);

/// How the process ended.
#[derive(Debug)]
pub(crate) enum End {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct Finished {
    pub end: End,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The file of `program`: an absolute path as it is, a bare name from the absolute directories of
/// `PATH`. A relative path or `PATH` entry would be looked up in the directory of the run, which
/// belongs to the repository.
pub(crate) fn resolve(program: &str) -> Result<PathBuf, String> {
    let path = Path::new(program);
    let found = if path.is_absolute() {
        Some(path.to_owned())
    } else if path.components().count() != 1 || program.contains(['/', '\\']) {
        return Err(format!(
            "{program}: a command is a program name or an absolute path"
        ));
    } else {
        let name: OsString = if cfg!(windows) && path.extension().is_none() {
            format!("{program}.exe").into()
        } else {
            program.into()
        };
        let search = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&search)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(&name))
            .find(|file| is_program(file))
    };
    let found = found.ok_or_else(|| format!("{program} was not found on the PATH"))?;
    // Windows runs a batch file through `cmd.exe`, a shell
    if cfg!(windows)
        && found
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("cmd") || x.eq_ignore_ascii_case("bat"))
    {
        return Err(format!(
            "{program}: a batch file needs a shell, which the runner does not use"
        ));
    }
    Ok(found)
}

#[cfg(unix)]
fn is_program(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    file.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_program(file: &Path) -> bool {
    file.is_file()
}

/// Reads a pipe on a thread; what arrived can be taken at any time.
struct Drain {
    data: Arc<Mutex<Vec<u8>>>,
    done: Receiver<()>,
}

impl Drain {
    fn new(mut pipe: impl Read + Send + 'static) -> Drain {
        let data = Arc::new(Mutex::new(Vec::new()));
        let (send, done) = mpsc::channel();
        let into = data.clone();
        std::thread::spawn(move || {
            let mut chunk = [0; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => into
                        .lock()
                        .expect("no panic while locked")
                        .extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = send.send(());
        });
        Drain { data, done }
    }

    fn take(self, deadline: Instant) -> Vec<u8> {
        let _ = self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        std::mem::take(&mut self.data.lock().expect("no panic while locked"))
    }
}

/// Runs `argv` in `dir` with no input until it ends, `timeout` passes or `cancel` is set. When it
/// returns, the process tree has ended. An error means that the program did not run.
pub(crate) fn run(
    argv: &[String],
    dir: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Finished, String> {
    let (program, args) = argv.split_first().ok_or("the harness has no command")?;
    let mut command = Command::new(resolve(program)?);
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tree::prepare(&mut command);
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| format!("{program} could not be started: {e}"))?;
    let mut tree = match tree::Tree::new(&child) {
        Ok(tree) => tree,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program}: its process tree cannot be held: {e}"));
        }
    };
    let stdout = Drain::new(child.stdout.take().expect("piped"));
    let stderr = Drain::new(child.stderr.take().expect("piped"));
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(End::Exited(status)),
            Ok(None) => {}
            Err(e) => break Err(format!("{program} cannot be waited for: {e}")),
        }
        if cancel.load(Ordering::SeqCst) {
            break Ok(End::Cancelled);
        }
        if started.elapsed() >= timeout {
            break Ok(End::TimedOut);
        }
        tree.watch();
        std::thread::sleep(POLL);
    };
    // also after a normal end: nothing of the run stays behind
    tree.end(&mut child);
    let end = end?;
    let deadline = Instant::now() + DRAIN;
    Ok(Finished {
        end,
        stdout: stdout.take(deadline),
        stderr: stderr.take(deadline),
    })
}

#[cfg(unix)]
mod tree {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::process::CommandExt;
    use std::process::Child;

    /// How often the descendants are looked up while the child runs.
    const WATCH: Duration = Duration::from_millis(500);

    pub(super) fn prepare(command: &mut Command) {
        // SAFETY: both calls are async-signal-safe and touch no memory.
        unsafe {
            command.pre_exec(|| {
                // A runner that is killed cannot end the tree; Linux ends the harness with the
                // thread that waits for it.
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Process {
        pid: u32,
        parent: u32,
        /// With the pid it names one process: a reused pid has another start.
        start: u64,
    }

    pub(super) struct Tree {
        /// The pid of the child, which is the id of its session.
        leader: u32,
        /// Every process seen as part of the tree, with its start.
        seen: HashMap<u32, u64>,
        looked: Instant,
    }

    impl Tree {
        pub(super) fn new(child: &Child) -> io::Result<Tree> {
            Ok(Tree {
                leader: child.id(),
                seen: HashMap::new(),
                looked: Instant::now(),
            })
        }

        /// Remembers the descendants while their parents still live: an orphan in a session of its
        /// own could not be found later.
        pub(super) fn watch(&mut self) {
            if self.looked.elapsed() >= WATCH {
                self.members();
                self.looked = Instant::now();
            }
        }

        /// The live processes of the tree, never pid 1 or this process.
        fn members(&mut self) -> Vec<u32> {
            let me = std::process::id();
            let all: Vec<Process> = processes()
                .into_iter()
                .filter(|p| p.pid > 1 && p.pid != me)
                .collect();
            let mut members: HashMap<u32, u64> = all
                .iter()
                .filter(|p| {
                    // SAFETY: getsid has no memory effects.
                    let session = unsafe { libc::getsid(p.pid as libc::pid_t) };
                    u32::try_from(session) == Ok(self.leader)
                        || self.seen.get(&p.pid) == Some(&p.start)
                })
                .map(|p| (p.pid, p.start))
                .collect();
            loop {
                let before = members.len();
                for p in &all {
                    if members.contains_key(&p.parent) {
                        members.insert(p.pid, p.start);
                    }
                }
                if members.len() == before {
                    break;
                }
            }
            self.seen.extend(members.iter().map(|(p, s)| (*p, *s)));
            members.into_keys().collect()
        }

        /// Sends `signal` to every member; `false` when there is none.
        fn signal(&mut self, signal: libc::c_int) -> bool {
            let members = self.members();
            for &pid in &members {
                // SAFETY: kill sends a signal, no memory is touched.
                unsafe { libc::kill(pid as libc::pid_t, signal) };
            }
            !members.is_empty()
        }

        /// Waits until no member is left, reaping the child.
        fn gone(&mut self, child: &mut Child, within: Duration) -> bool {
            let deadline = Instant::now() + within;
            loop {
                let reaped = !matches!(child.try_wait(), Ok(None));
                if reaped && self.members().is_empty() {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(POLL);
            }
        }

        pub(super) fn end(&mut self, child: &mut Child) {
            if !self.signal(libc::SIGTERM) && !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            if !self.gone(child, GRACE) {
                // twice: a member may start a process between the look and the signal
                self.signal(libc::SIGKILL);
                if !self.gone(child, Duration::from_millis(500)) {
                    self.signal(libc::SIGKILL);
                    let _ = child.kill();
                    self.gone(child, Duration::from_millis(500));
                }
            }
        }
    }

    /// The processes that are alive (a zombie is gone for our purpose).
    #[cfg(target_os = "linux")]
    fn processes() -> Vec<Process> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|entry| {
            let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
            let stat = String::from_utf8_lossy(&stat);
            // `pid (comm) state ppid ...`: comm may hold spaces and `)`, so look after the last one
            let mut fields = stat.rsplit_once(')')?.1.split_ascii_whitespace();
            if fields.next()? == "Z" {
                return None;
            }
            let parent = fields.next()?.parse().ok()?;
            // field 22 of the line; two fields after the name are taken
            let start = fields.nth(17)?.parse().ok()?;
            Some(Process { pid, parent, start })
        })
        .collect()
    }

    #[cfg(target_os = "macos")]
    fn processes() -> Vec<Process> {
        // SAFETY: a null buffer asks for the number; the second call fills a buffer of that size.
        let pids = unsafe {
            let count = libc::proc_listallpids(std::ptr::null_mut(), 0);
            if count <= 0 {
                return Vec::new();
            }
            // room for processes started between the two calls
            let mut pids = vec![0 as libc::pid_t; count as usize + 64];
            let size = std::mem::size_of_val(pids.as_slice()) as libc::c_int;
            let got = libc::proc_listallpids(pids.as_mut_ptr().cast(), size);
            pids.truncate(usize::try_from(got).unwrap_or(0));
            pids
        };
        pids.into_iter()
            .filter_map(|pid| {
                // SAFETY: the info struct is plain data the call fills.
                let info = unsafe {
                    let mut info: libc::proc_bsdinfo = std::mem::zeroed();
                    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
                    let got = libc::proc_pidinfo(
                        pid,
                        libc::PROC_PIDTBSDINFO,
                        0,
                        (&mut info as *mut libc::proc_bsdinfo).cast(),
                        size,
                    );
                    (got == size && info.pbi_status != libc::SZOMB).then_some(info)
                }?;
                Some(Process {
                    pid: u32::try_from(pid).ok()?,
                    parent: info.pbi_ppid,
                    start: info
                        .pbi_start_tvsec
                        .wrapping_mul(1_000_000)
                        .wrapping_add(info.pbi_start_tvusec),
                })
            })
            .collect()
    }

    /// No list of processes: only the session of the child is known to be its tree.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn processes() -> Vec<Process> {
        Vec::new()
    }
}

#[cfg(windows)]
mod tree {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    /// Ctrl-C reaches the runner only, which ends the job.
    pub(super) fn prepare(command: &mut Command) {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    /// A job object holding the child and everything it starts. Closing it, also by the end of the
    /// runner, kills what is left.
    pub(super) struct Tree(HANDLE);

    impl Tree {
        pub(super) fn new(child: &Child) -> io::Result<Tree> {
            // SAFETY: the calls take our own handles and a struct of the size given.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let tree = Tree(job);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of_val(&info) as u32,
                );
                if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle().cast()) == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(tree)
            }
        }

        pub(super) fn watch(&mut self) {}

        pub(super) fn end(&mut self, child: &mut Child) {
            // SAFETY: the handle is ours and open until drop.
            unsafe { TerminateJobObject(self.0, 1) };
            let deadline = Instant::now() + GRACE;
            while (!matches!(child.try_wait(), Ok(Some(_))) || self.active() > 0)
                && Instant::now() < deadline
            {
                std::thread::sleep(POLL);
            }
        }

        /// The processes still in the job.
        fn active(&self) -> u32 {
            // SAFETY: the info struct is plain data the call fills.
            unsafe {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                let ok = QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                );
                if ok == 0 { 0 } else { info.ActiveProcesses }
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            // SAFETY: closes our own handle once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_never_looked_up_in_the_directory_of_the_run() {
        assert!(resolve("./claude").unwrap_err().contains("absolute"));
        assert!(resolve("bin/claude").unwrap_err().contains("absolute"));
        assert!(
            resolve("no-such-program-agent-scripts")
                .unwrap_err()
                .contains("was not found")
        );
    }
}
