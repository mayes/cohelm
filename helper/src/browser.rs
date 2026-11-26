//! Starts the person's Cohelm browser with a private DevTools pipe.
//!
//! The browser gets `--remote-debugging-pipe` and never opens a debugging
//! port, so only the Helper, which holds the other end of the pipe, can drive
//! it (ADR 0005). Chromium reads DevTools commands from one pipe and writes
//! replies and events to another, each message ending in a NUL byte.

use std::{
    ffi::OsString,
    io::{PipeReader, PipeWriter},
    path::PathBuf,
    process::Stdio,
};

use crate::config::HelperError;

/// How to start the browser.
#[derive(Debug, Clone)]
pub struct BrowserCommand {
    /// The Cohelm (Chromium) executable.
    pub program: PathBuf,
    /// Extra arguments, such as a page to open. The Helper adds the profile
    /// and the pipe itself.
    pub args: Vec<OsString>,
    /// Cohelm's own profile folder. Chromium ignores debugging switches on
    /// the default Chrome profile, and Cohelm keeps its logins apart anyway.
    pub profile_dir: PathBuf,
}

impl BrowserCommand {
    pub fn new(program: impl Into<PathBuf>, profile_dir: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            profile_dir: profile_dir.into(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), HelperError> {
        if !self.profile_dir.is_absolute() {
            return Err(HelperError::Profile(self.profile_dir.clone()));
        }
        Ok(())
    }

    /// The arguments the browser is started with, before the pipe's own.
    pub(crate) fn base_args(&self) -> Vec<OsString> {
        let mut profile = OsString::from("--user-data-dir=");
        profile.push(&self.profile_dir);
        let mut args = vec![profile, "--remote-debugging-pipe".into()];
        args.extend(self.args.iter().cloned());
        args
    }
}

/// A browser the Helper started, with the Helper's ends of its pipe.
pub(crate) struct LaunchedBrowser {
    pub child: tokio::process::Child,
    /// Commands go in here.
    pub to_browser: PipeWriter,
    /// Replies and events come out here.
    pub from_browser: PipeReader,
}

pub(crate) fn launch(command: &BrowserCommand) -> Result<LaunchedBrowser, HelperError> {
    command.validate()?;
    std::fs::create_dir_all(&command.profile_dir)?;
    // The browser reads commands from `browser_reads` and writes to
    // `browser_writes`.
    let (browser_reads, to_browser) = std::io::pipe()?;
    let (from_browser, browser_writes) = std::io::pipe()?;

    let mut cmd = tokio::process::Command::new(&command.program);
    cmd.args(command.base_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    platform::hand_over(&mut cmd, &browser_reads, &browser_writes)?;
    let child = cmd.spawn().map_err(|e| {
        HelperError::BrowserStart(command.program.display().to_string(), e.to_string())
    })?;
    // The browser holds its own copies now. Closing ours means the Helper
    // sees the pipe end when the browser exits.
    drop(browser_reads);
    drop(browser_writes);
    Ok(LaunchedBrowser {
        child,
        to_browser,
        from_browser,
    })
}

#[cfg(unix)]
mod platform {
    use std::{
        io::{PipeReader, PipeWriter},
        os::fd::{AsRawFd, RawFd},
    };

    /// Chromium reads its DevTools pipe on fd 3 and writes on fd 4.
    pub(super) fn hand_over(
        cmd: &mut tokio::process::Command,
        reads: &PipeReader,
        writes: &PipeWriter,
    ) -> std::io::Result<()> {
        let reads = reads.as_raw_fd();
        let writes = writes.as_raw_fd();
        // SAFETY: the closure only calls fcntl and dup2, which are safe to
        // call between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                // Move both out of the way first, in case either already sits
                // on 3 or 4. The copies close on exec; 3 and 4 stay open.
                let reads = moved_high(reads)?;
                let writes = moved_high(writes)?;
                onto(reads, 3)?;
                onto(writes, 4)
            });
        }
        Ok(())
    }

    fn moved_high(fd: RawFd) -> std::io::Result<RawFd> {
        // SAFETY: fcntl on a file descriptor this process owns.
        let moved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
        if moved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(moved)
    }

    fn onto(fd: RawFd, target: RawFd) -> std::io::Result<()> {
        // SAFETY: dup2 onto a descriptor number the child is about to use.
        if unsafe { libc::dup2(fd, target) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        io::{PipeReader, PipeWriter},
        os::windows::io::AsRawHandle,
    };

    use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};

    /// On Windows, Chromium takes the pipe's handles by number with
    /// `--remote-debugging-io-pipes=<read>,<write>`. The handles are made
    /// inheritable; the Helper closes its copies right after the start.
    pub(super) fn hand_over(
        cmd: &mut tokio::process::Command,
        reads: &PipeReader,
        writes: &PipeWriter,
    ) -> std::io::Result<()> {
        let reads = reads.as_raw_handle();
        let writes = writes.as_raw_handle();
        for handle in [reads, writes] {
            // SAFETY: a handle this process owns, kept open past the call.
            let ok = unsafe {
                SetHandleInformation(handle as HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        cmd.arg(format!(
            "--remote-debugging-io-pipes={},{}",
            reads as usize, writes as usize
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_gets_its_own_profile_and_the_pipe() {
        let mut command = BrowserCommand::new("/opt/cohelm/chromium", "/home/ada/.cohelm/profile");
        command.args.push("https://example.com/".into());
        let args = command.base_args();
        assert_eq!(args[0], "--user-data-dir=/home/ada/.cohelm/profile");
        assert!(args.iter().any(|a| a == "--remote-debugging-pipe"));
        assert!(
            !args
                .iter()
                .any(|a| a.to_string_lossy().starts_with("--remote-debugging-port")),
            "no debugging port"
        );
        assert_eq!(args.last().unwrap(), "https://example.com/");
    }

    #[test]
    fn a_relative_profile_is_refused() {
        let command = BrowserCommand::new("chromium", "profile");
        assert!(matches!(command.validate(), Err(HelperError::Profile(_))));
    }
}
