//! ⚠️ **Dangerous by design: this file forks and execs.**
//!
//! Every process-spawning call in Mica lives here and nowhere else. That is the
//! whole reason the module exists as its own file — `fork`, `execve`, signal
//! delivery, and file-descriptor surgery are exactly the operations that should
//! be reviewable in one sitting, not scattered across a session layer.
//!
//! libghostty-vt deliberately provides no PTY (`libghostty-pty` does not
//! exist), so this is ours under either backend.
//!
//! ## The fork-to-exec window
//!
//! Between `fork` and `execve` the child holds a copy of a multi-threaded
//! parent's address space with exactly one thread running. Only
//! async-signal-safe calls are legal in that window. Everything that needs
//! allocation — argv, envp, the slave device path — is built **before** the
//! fork and only pointer-shuffled after it. Do not add a `String` allocation,
//! a `format!`, or a logging call between the two.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

/// How the child process should be started.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// Absolute path to the program. Resolved from `$SHELL` by
    /// [`PtyConfig::for_login_shell`]; never looked up on `PATH` at exec time,
    /// because `execve` does not consult `PATH` and doing the lookup ourselves
    /// keeps it out of the post-fork window.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Extra variables layered over the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Variables to remove from the inherited environment.
    pub env_remove: Vec<OsString>,
    pub cols: u16,
    pub rows: u16,
}

/// The shell used when `$SHELL` is unset or relative.
///
/// POSIX requires this path to exist; every other shell on macOS is a
/// convention.
pub const ROOT_SHELL: &str = "/bin/sh";

impl PtyConfig {
    /// A login shell, honouring `[shell]` from the settings file.
    ///
    /// `program` overrides `$SHELL`; `starting-dir` overrides the home
    /// directory a login shell would otherwise choose for itself. A `~` at the
    /// front of the directory is expanded here, because a settings file is a
    /// document a person types into and `~/Projects` is what they will type.
    pub fn for_shell_settings(
        cols: u16,
        rows: u16,
        shell: &crate::settings::ShellSettings,
    ) -> PtyConfig {
        let mut config = PtyConfig::for_login_shell(cols, rows);
        if let Some(program) = shell.program.as_deref().map(PathBuf::from) {
            // Ignored rather than obeyed if it is not absolute or not there:
            // a typo in the settings file must not leave the user with a
            // terminal that cannot open a shell.
            if program.is_absolute() && program.exists() {
                let name = program.file_name().unwrap_or_else(|| OsStr::new("sh")).to_owned();
                let mut argv0 = OsString::from("-");
                argv0.push(&name);
                config.program = program;
                config.args = vec![argv0];
            } else {
                eprintln!(
                    "mica: [shell] program = {:?} is not an absolute path to an existing \
                     file; falling back to $SHELL",
                    program.display()
                );
            }
        }
        if let Some(dir) = shell.starting_dir.as_deref().map(expand_tilde) {
            if dir.is_dir() {
                config.cwd = Some(dir);
            } else {
                eprintln!("mica: [shell] starting-dir = {:?} is not a directory", dir.display());
            }
        }
        config
    }

    /// A login shell, as a terminal is expected to start.
    pub fn for_login_shell(cols: u16, rows: u16) -> PtyConfig {
        // `/bin/sh` is the root, not `/bin/zsh`. `$SHELL` is what the user
        // chose and is used whenever it names an absolute path, but the
        // fallback has to be the one program POSIX guarantees is there. A
        // default of `/bin/zsh` is a guess about this decade's macOS, and the
        // failure mode when the guess is wrong is a terminal that cannot open
        // a shell at all.
        let program = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(ROOT_SHELL));

        // A leading `-` in argv[0] is how a shell is told it is a login shell.
        let name = program.file_name().unwrap_or_else(|| OsStr::new("sh"));
        let mut argv0 = OsString::from("-");
        argv0.push(name);

        PtyConfig {
            program,
            args: vec![argv0],
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> PtyConfig {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// `~` and `~/…` against `$HOME`. Anything else is returned unchanged.
///
/// Only the leading `~` — `~otheruser` needs the password database, and a
/// settings file that silently resolved another user's home directory would be
/// a surprise nobody asked for.
fn expand_tilde(text: &str) -> PathBuf {
    let Some(home) = std::env::var_os("HOME") else { return PathBuf::from(text) };
    match text {
        "~" => PathBuf::from(home),
        _ => match text.strip_prefix("~/") {
            Some(rest) => PathBuf::from(home).join(rest),
            None => PathBuf::from(text),
        },
    }
}

/// A running child on the far side of a pseudoterminal.
#[derive(Debug)]
pub struct Pty {
    master: Arc<OwnedFd>,
    pid: libc::pid_t,
    reaped: Option<ExitStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Exited(i32),
    Signaled(i32),
}

impl ExitStatus {
    pub fn code(self) -> Option<i32> {
        match self {
            ExitStatus::Exited(c) => Some(c),
            ExitStatus::Signaled(_) => None,
        }
    }
}

impl Pty {
    /// Opens a pseudoterminal and starts the child on it.
    pub fn spawn(config: &PtyConfig) -> io::Result<Pty> {
        // --- everything allocating happens here, before the fork ------------
        let master = open_master()?;
        let slave_path = slave_path(master.as_raw_fd())?;
        let slave_c = CString::new(slave_path.as_os_str().as_bytes())
            .map_err(|_| io::Error::other("pty device path contains a NUL"))?;

        // The slave is opened here, in the parent, and not only in the child.
        //
        // On macOS the master side has no line discipline attached until a
        // slave exists: `ioctl(master, TIOCSWINSZ)` fails with `ENOTTY` before
        // that first open and succeeds afterwards. Verified on this machine —
        // it is not a hypothetical. The parent's copy is closed immediately
        // after the fork so that the child holds the only reference and a
        // hangup still surfaces as `EIO` on the master.
        let slave = open_slave(&slave_c)?;

        let program_c = CString::new(config.program.as_os_str().as_bytes())
            .map_err(|_| io::Error::other("program path contains a NUL"))?;
        let argv_owned = build_argv(config)?;
        let envp_owned = build_envp(config)?;
        let cwd_c = match &config.cwd {
            Some(dir) => Some(
                CString::new(dir.as_os_str().as_bytes())
                    .map_err(|_| io::Error::other("cwd contains a NUL"))?,
            ),
            None => None,
        };

        let mut argv: Vec<*const libc::c_char> =
            argv_owned.iter().map(|s| s.as_ptr()).chain(std::iter::once(std::ptr::null())).collect();
        let mut envp: Vec<*const libc::c_char> =
            envp_owned.iter().map(|s| s.as_ptr()).chain(std::iter::once(std::ptr::null())).collect();

        set_window_size(master.as_raw_fd(), config.cols, config.rows)?;

        let slave_fd = slave.as_raw_fd();

        // --- fork -----------------------------------------------------------
        // SAFETY: between here and `execve` the child calls only
        // async-signal-safe functions. Nothing below allocates.
        let pid = unsafe { libc::fork() };
        match pid {
            -1 => Err(io::Error::last_os_error()),
            0 => {
                // Child. There is no safe way to report an error from here, so
                // every failure path ends in `_exit` with a distinct code —
                // the parent sees it as the child's exit status.
                unsafe {
                    if libc::setsid() < 0 {
                        libc::_exit(126);
                    }
                    // The inherited slave fd was opened before `setsid`, so it
                    // is not yet a controlling terminal. `TIOCSCTTY` makes it
                    // one; without it, job control and Ctrl-C do not work.
                    if libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                        libc::_exit(126);
                    }

                    if libc::dup2(slave_fd, 0) < 0
                        || libc::dup2(slave_fd, 1) < 0
                        || libc::dup2(slave_fd, 2) < 0
                    {
                        libc::_exit(126);
                    }
                    if slave_fd > 2 {
                        libc::close(slave_fd);
                    }

                    if let Some(dir) = &cwd_c {
                        if libc::chdir(dir.as_ptr()) < 0 {
                            libc::_exit(126);
                        }
                    }

                    // The parent may have masked or ignored signals; a fresh
                    // shell must not inherit that. SIGPIPE in particular is
                    // set to SIG_IGN by the Rust runtime, and a shell that
                    // inherits it mishandles broken pipes in ways that are
                    // maddening to debug.
                    libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                    libc::signal(libc::SIGHUP, libc::SIG_DFL);
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    libc::signal(libc::SIGTERM, libc::SIG_DFL);
                    libc::signal(libc::SIGCHLD, libc::SIG_DFL);
                    let mut empty: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut empty);
                    libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());

                    libc::execve(program_c.as_ptr(), argv.as_mut_ptr(), envp.as_mut_ptr());
                    libc::_exit(127); // exec failed — the conventional code
                }
            }
            pid => {
                // The child owns the slave now. Holding a second reference
                // here would keep the pty open after the child exits, and the
                // reader thread would block forever instead of seeing EIO.
                drop(slave);
                Ok(Pty { master: Arc::new(master), pid, reaped: None })
            }
        }
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Resizes the terminal and tells the child about it.
    ///
    /// `TIOCSWINSZ` normally delivers `SIGWINCH` on its own; the explicit
    /// `killpg` covers the case where the child moved itself into a different
    /// process group.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_window_size(self.master.as_raw_fd(), cols.max(1), rows.max(1))?;
        unsafe {
            let pgrp = libc::tcgetpgrp(self.master.as_raw_fd());
            if pgrp > 0 {
                libc::killpg(pgrp, libc::SIGWINCH);
            }
        }
        Ok(())
    }

    /// Sends keystrokes to the child.
    pub fn write(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let n = unsafe {
                libc::write(
                    self.master.as_raw_fd(),
                    bytes.as_ptr() as *const libc::c_void,
                    bytes.len(),
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            bytes = &bytes[n as usize..];
        }
        Ok(())
    }

    /// Reaps the child if it has exited, without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.reaped {
            return Ok(Some(status));
        }
        let mut raw: libc::c_int = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut raw, libc::WNOHANG) };
        match r {
            0 => Ok(None),
            -1 => Err(io::Error::last_os_error()),
            _ => {
                let status = decode_status(raw);
                self.reaped = Some(status);
                Ok(Some(status))
            }
        }
    }

    /// Blocks until the child exits.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        loop {
            let mut raw: libc::c_int = 0;
            let r = unsafe { libc::waitpid(self.pid, &mut raw, 0) };
            if r == -1 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            let status = decode_status(raw);
            self.reaped = Some(status);
            return Ok(status);
        }
    }

    /// Starts a reader thread and returns the channel it feeds.
    ///
    /// `sync_channel` rather than an unbounded one: `cat` of a 200 MB log
    /// produces bytes faster than any renderer consumes them, and an unbounded
    /// queue turns that into unbounded memory. Back-pressure onto the PTY is
    /// the correct behaviour — it is what a real terminal does.
    pub fn reader(&self, buffer_chunks: usize) -> io::Result<Receiver<PtyEvent>> {
        self.reader_with_wakeup(buffer_chunks, None)
    }

    /// The same, plus a callback fired after every chunk is queued.
    ///
    /// This is what lets the UI be genuinely event-driven rather than polled.
    /// Without it the only way for the main thread to notice new output is to
    /// ask on a timer — which works, but means an idle terminal wakes the CPU
    /// hundreds of times a second to be told nothing happened. The callback
    /// runs on the reader thread and must therefore do the smallest possible
    /// thing: signal the main thread and return.
    pub fn reader_with_wakeup(
        &self,
        buffer_chunks: usize,
        wakeup: Option<Wakeup>,
    ) -> io::Result<Receiver<PtyEvent>> {
        let (tx, rx) = mpsc::sync_channel(buffer_chunks.max(1));
        let master = Arc::clone(&self.master);
        std::thread::Builder::new()
            .name("mica-pty-reader".to_owned())
            .spawn(move || read_loop(master, tx, wakeup))?;
        Ok(rx)
    }
}

/// Fired on the reader thread whenever output is available.
///
/// Must be cheap and must not block: it runs between a read and the next one.
pub type Wakeup = Arc<dyn Fn() + Send + Sync>;

/// What the reader thread reports.
#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The child closed the terminal — `read` returned 0 or `EIO`, which is
    /// how a hangup surfaces on a PTY master.
    Hangup,
    Error(io::Error),
}

fn read_loop(master: Arc<OwnedFd>, tx: SyncSender<PtyEvent>, wakeup: Option<Wakeup>) {
    let signal = || {
        if let Some(wakeup) = &wakeup {
            wakeup();
        }
    };
    // 64 KiB matches the typical PTY buffer; larger reads mostly return short.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = unsafe {
            libc::read(master.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        if n > 0 {
            if tx.send(PtyEvent::Output(buf[..n as usize].to_vec())).is_err() {
                return; // the consumer went away; so do we
            }
            signal();
            continue;
        }
        if n == 0 {
            let _ = tx.send(PtyEvent::Hangup);
            signal();
            return;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            // On macOS a closed slave surfaces as EIO, not as end-of-file.
            _ if err.raw_os_error() == Some(libc::EIO) => {
                let _ = tx.send(PtyEvent::Hangup);
                signal();
                return;
            }
            _ => {
                let _ = tx.send(PtyEvent::Error(err));
                signal();
                return;
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if self.reaped.is_some() {
            return;
        }
        // Hang up, then *actually reap*. Closing the master alone leaves a
        // shell blocked on read; a single non-blocking `waitpid` leaves a
        // zombie, because the child has not finished exiting yet at the moment
        // the window closes. Fifty closed tabs would mean fifty zombies for the
        // life of the application.
        self.signal_group(libc::SIGHUP);

        // Bounded poll: a shell handles SIGHUP in microseconds, but a wedged
        // one must not hang the main thread while a window closes.
        const ATTEMPTS: u32 = 40;
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);
        for _ in 0..ATTEMPTS {
            match self.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(INTERVAL),
            }
        }

        // It ignored the hangup. SIGKILL cannot be ignored, so the blocking
        // wait that follows is guaranteed to terminate.
        self.signal_group(libc::SIGKILL);
        let _ = self.wait();
    }
}

impl Pty {
    /// Signals the child's foreground process group, falling back to the child
    /// itself. The group matters: a shell running `make -j8` has children of
    /// its own, and signalling only the shell orphans them.
    fn signal_group(&self, signal: libc::c_int) {
        unsafe {
            let pgrp = libc::tcgetpgrp(self.master.as_raw_fd());
            if pgrp > 0 {
                libc::killpg(pgrp, signal);
            }
            // Always signal the child too: `tcgetpgrp` reports the *foreground*
            // group, which is not the shell's own group while a job is running.
            libc::kill(self.pid, signal);
        }
    }
}

// --- plumbing ---------------------------------------------------------------

fn open_master() -> io::Result<OwnedFd> {
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(fd);
        if libc::grantpt(fd) < 0 || libc::unlockpt(fd) < 0 {
            return Err(io::Error::last_os_error());
        }
        // The master must not survive an exec in any child we later spawn.
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(master)
    }
}

/// Opens the slave side. Deliberately **not** `FD_CLOEXEC`: the child inherits
/// this descriptor across the exec and uses it as its standard streams.
fn open_slave(path: &CStr) -> io::Result<OwnedFd> {
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

fn slave_path(master: RawFd) -> io::Result<PathBuf> {
    // `ptsname` returns a pointer to static storage and is not thread-safe.
    // It is called here in the parent, before the fork, and the result is
    // copied immediately — which is the only way to use it correctly, since
    // macOS has no `ptsname_r`.
    unsafe {
        let ptr = libc::ptsname(master);
        if ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        let bytes = CStr::from_ptr(ptr).to_bytes().to_vec();
        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
}

fn set_window_size(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    let r = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as libc::c_ulong, &ws) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn build_argv(config: &PtyConfig) -> io::Result<Vec<CString>> {
    let mut out = Vec::with_capacity(config.args.len() + 1);
    if config.args.is_empty() {
        let name = config.program.file_name().unwrap_or_else(|| OsStr::new("sh"));
        out.push(cstring(name.as_bytes())?);
    }
    for arg in &config.args {
        out.push(cstring(arg.as_bytes())?);
    }
    Ok(out)
}

fn build_envp(config: &PtyConfig) -> io::Result<Vec<CString>> {
    let mut pairs: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| !config.env_remove.iter().any(|r| r == k))
        .filter(|(k, _)| !config.env.iter().any(|(ek, _)| ek == k))
        .collect();
    pairs.extend(config.env.iter().cloned());

    pairs
        .into_iter()
        .map(|(k, v)| {
            let mut entry = k.into_vec();
            entry.push(b'=');
            entry.extend_from_slice(v.as_bytes());
            cstring(&entry)
        })
        .collect()
}

fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::other("environment or argument contains a NUL"))
}

fn decode_status(raw: libc::c_int) -> ExitStatus {
    // libc exposes these as macros in C; the bit layout is stable.
    if raw & 0x7f == 0 {
        ExitStatus::Exited((raw >> 8) & 0xff)
    } else {
        ExitStatus::Signaled(raw & 0x7f)
    }
}

/// Resolves a program name against `PATH`, so the lookup never happens in the
/// post-fork window.
pub fn resolve_program(name: &OsStr) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let var = std::env::var_os("PATH")?;
    std::env::split_paths(&var).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    // Deliberately not `#[test] fn` further down: this belongs next to the
    // constant it pins.

    use super::*;
    use std::time::{Duration, Instant};

    /// Reads until `needle` appears or the deadline passes.
    ///
    /// Not a sleep: the read blocks on the channel and returns as soon as the
    /// bytes arrive, so the timeout is a failure bound rather than a delay.
    fn read_until(rx: &Receiver<PtyEvent>, needle: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        let mut acc = String::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Output(bytes)) => {
                    acc.push_str(&String::from_utf8_lossy(&bytes));
                    if acc.contains(needle) {
                        return acc;
                    }
                }
                Ok(PtyEvent::Hangup) => break,
                Ok(PtyEvent::Error(e)) => panic!("pty read failed: {e}"),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        acc
    }

    fn sh(cols: u16, rows: u16) -> PtyConfig {
        PtyConfig {
            program: PathBuf::from("/bin/sh"),
            args: vec![OsString::from("sh")],
            cwd: None,
            // A bare prompt keeps the output predictable across machines.
            env: vec![(OsString::from("PS1"), OsString::from(""))],
            env_remove: vec![OsString::from("ENV")],
            cols,
            rows,
        }
    }

    #[test]
    fn a_command_runs_and_its_output_comes_back() {
        let pty = Pty::spawn(&sh(80, 24)).unwrap();
        let rx = pty.reader(64).unwrap();
        pty.write(b"echo mica-hello\n").unwrap();
        let out = read_until(&rx, "mica-hello", Duration::from_secs(10));
        assert!(out.contains("mica-hello"), "child produced: {out:?}");
    }

    #[test]
    fn the_child_sees_the_window_size_it_was_given() {
        let pty = Pty::spawn(&sh(97, 31)).unwrap();
        let rx = pty.reader(64).unwrap();
        pty.write(b"stty size\n").unwrap();
        let out = read_until(&rx, "31 97", Duration::from_secs(10));
        assert!(out.contains("31 97"), "expected `31 97` in: {out:?}");
    }

    #[test]
    fn a_resize_reaches_the_child() {
        let pty = Pty::spawn(&sh(80, 24)).unwrap();
        let rx = pty.reader(64).unwrap();
        pty.resize(40, 12).unwrap();
        pty.write(b"stty size\n").unwrap();
        let out = read_until(&rx, "12 40", Duration::from_secs(10));
        assert!(out.contains("12 40"), "expected `12 40` in: {out:?}");
    }

    #[test]
    fn environment_additions_reach_the_child() {
        let mut config = sh(80, 24);
        config.env.push((OsString::from("MICA_PROBE"), OsString::from("present")));
        let pty = Pty::spawn(&config).unwrap();
        let rx = pty.reader(64).unwrap();
        pty.write(b"echo \"[$MICA_PROBE]\"\n").unwrap();
        let out = read_until(&rx, "[present]", Duration::from_secs(10));
        assert!(out.contains("[present]"), "child produced: {out:?}");
    }

    #[test]
    fn an_exiting_child_is_reaped_with_its_status() {
        let mut pty = Pty::spawn(&sh(80, 24)).unwrap();
        let _rx = pty.reader(64).unwrap();
        pty.write(b"exit 3\n").unwrap();
        assert_eq!(pty.wait().unwrap(), ExitStatus::Exited(3));
        // A second wait must not block or error — the status is remembered.
        assert_eq!(pty.try_wait().unwrap(), Some(ExitStatus::Exited(3)));
    }

    #[test]
    fn a_closed_child_reports_hangup_rather_than_hanging() {
        let mut pty = Pty::spawn(&sh(80, 24)).unwrap();
        let rx = pty.reader(64).unwrap();
        pty.write(b"exit 0\n").unwrap();
        let _ = pty.wait();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_hangup = false;
        while Instant::now() < deadline && !saw_hangup {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Hangup) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    saw_hangup = true
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
        assert!(saw_hangup, "reader thread never reported the child going away");
    }

    #[test]
    fn a_nonexistent_program_surfaces_as_exit_127() {
        let mut config = sh(80, 24);
        config.program = PathBuf::from("/nonexistent/mica-not-a-program");
        config.args = vec![OsString::from("mica-not-a-program")];
        let mut pty = Pty::spawn(&config).unwrap();
        assert_eq!(pty.wait().unwrap(), ExitStatus::Exited(127));
    }

    #[test]
    fn program_resolution_finds_a_real_binary_and_rejects_a_fake_one() {
        assert!(resolve_program(OsStr::new("sh")).is_some());
        assert_eq!(resolve_program(OsStr::new("mica-definitely-not-installed")), None);
        assert_eq!(resolve_program(OsStr::new("/bin/sh")), Some(PathBuf::from("/bin/sh")));
    }
}

#[cfg(test)]
mod root_shell_tests {
    use super::*;

    #[test]
    fn the_root_shell_exists_on_this_machine() {
        // The point of the constant is that this is the one path that is
        // always there. If it ever is not, the fallback is worthless and the
        // failure should be loud here rather than at the first fork.
        assert!(
            Path::new(ROOT_SHELL).exists(),
            "{ROOT_SHELL} is missing — the fallback shell must be a path POSIX guarantees"
        );
    }

    #[test]
    fn an_absent_shell_variable_falls_back_to_the_root_shell() {
        // `for_login_shell` reads the real environment, so this asserts the
        // resolution rule directly rather than mutating process state, which
        // would race every other test in this binary.
        let resolved = |shell: Option<&str>| -> PathBuf {
            shell
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| PathBuf::from(ROOT_SHELL))
        };
        assert_eq!(resolved(None), PathBuf::from("/bin/sh"));
        assert_eq!(resolved(Some("zsh")), PathBuf::from("/bin/sh"), "a relative $SHELL is not a choice");
        assert_eq!(resolved(Some("/bin/bash")), PathBuf::from("/bin/bash"));
    }

    #[test]
    fn a_login_shell_is_named_with_a_leading_dash() {
        let config = PtyConfig::for_login_shell(80, 24);
        let argv0 = config.args[0].to_string_lossy().into_owned();
        assert!(argv0.starts_with('-'), "argv[0] was {argv0:?}; a login shell needs the dash");
        assert!(config.program.is_absolute());
    }
}

#[cfg(test)]
mod shell_settings_tests {
    use super::*;
    use crate::settings::ShellSettings;

    #[test]
    fn an_absolute_existing_program_replaces_the_login_shell() {
        let shell = ShellSettings { program: Some(ROOT_SHELL.into()), ..Default::default() };
        let config = PtyConfig::for_shell_settings(80, 24, &shell);
        assert_eq!(config.program, PathBuf::from(ROOT_SHELL));
        // Still a login shell: the leading `-` is what tells it so, and losing
        // it means the user's profile never runs.
        assert_eq!(config.args, vec![OsString::from("-sh")]);
    }

    #[test]
    fn a_program_that_is_not_there_falls_back_rather_than_breaking_the_terminal() {
        // A typo in a hand-edited settings file must not leave someone with a
        // terminal that cannot open a shell — which is the one thing they
        // would need in order to fix the file.
        let shell = ShellSettings {
            program: Some("/usr/bin/definitely-not-a-shell".into()),
            ..Default::default()
        };
        let config = PtyConfig::for_shell_settings(80, 24, &shell);
        assert_eq!(config.program, PtyConfig::for_login_shell(80, 24).program);
    }

    #[test]
    fn a_relative_program_is_refused() {
        // `execve` does not consult `PATH`, so a relative program would fail
        // in the post-fork window where there is nowhere to report it.
        let shell = ShellSettings { program: Some("bash".into()), ..Default::default() };
        let config = PtyConfig::for_shell_settings(80, 24, &shell);
        assert_eq!(config.program, PtyConfig::for_login_shell(80, 24).program);
    }

    #[test]
    fn a_starting_directory_expands_a_leading_tilde() {
        let home = std::env::var("HOME").expect("HOME is set on macOS");
        let shell = ShellSettings { starting_dir: Some("~".into()), ..Default::default() };
        assert_eq!(
            PtyConfig::for_shell_settings(80, 24, &shell).cwd,
            Some(PathBuf::from(&home))
        );

        // `~otheruser` needs the password database and is deliberately not
        // expanded; it is simply not a directory, so it is refused.
        let shell = ShellSettings { starting_dir: Some("~nobody".into()), ..Default::default() };
        assert_eq!(PtyConfig::for_shell_settings(80, 24, &shell).cwd, None);
    }

    #[test]
    fn a_starting_directory_that_is_not_a_directory_is_refused() {
        let shell =
            ShellSettings { starting_dir: Some("/etc/hosts".into()), ..Default::default() };
        assert_eq!(PtyConfig::for_shell_settings(80, 24, &shell).cwd, None);
    }

    #[test]
    fn the_default_settings_change_nothing() {
        let plain = PtyConfig::for_login_shell(80, 24);
        let configured = PtyConfig::for_shell_settings(80, 24, &ShellSettings::default());
        assert_eq!(plain.program, configured.program);
        assert_eq!(plain.args, configured.args);
        assert_eq!(plain.cwd, configured.cwd);
    }
}
