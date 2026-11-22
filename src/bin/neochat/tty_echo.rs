use std::{fs::File, io, mem::MaybeUninit, os::fd::AsRawFd};

/// Keep echo off on rpassword's controlling TTY from before the first prompt
/// through confirmation. Restore the caller's settings on every return path.
pub(super) struct Guard {
    tty: File,
    original: libc::termios,
    signals: libc::sigset_t,
}

impl Guard {
    pub(super) fn new() -> io::Result<Self> {
        let tty = File::open("/dev/tty")?;
        let fd = tty.as_raw_fd();
        let mut original = MaybeUninit::uninit();
        let mut block = MaybeUninit::uninit();
        let mut previous = MaybeUninit::uninit();
        // SAFETY: the fd is live, and each output is read only after its
        // initializing libc call succeeds. Guard stays on the calling thread.
        unsafe {
            if libc::tcgetattr(fd, original.as_mut_ptr()) != 0 || libc::sigemptyset(block.as_mut_ptr()) != 0
                || libc::sigaddset(block.as_mut_ptr(), libc::SIGINT) != 0 {
                return Err(io::Error::last_os_error());
            }
            // rpassword raises SIGINT on this thread for Ctrl-C. Deliver it
            // after restoring the outer settings, not in the middle of Drop.
            let err = libc::pthread_sigmask(libc::SIG_BLOCK, block.as_ptr(), previous.as_mut_ptr());
            if err != 0 { return Err(io::Error::from_raw_os_error(err)); }
            let guard = Self { tty, original: original.assume_init(), signals: previous.assume_init() };
            let mut hidden = guard.original;
            hidden.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(fd, libc::TCSANOW, &hidden) != 0 { return Err(io::Error::last_os_error()); }
            Ok(guard)
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &self.original);
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.signals, std::ptr::null_mut());
        }
    }
}