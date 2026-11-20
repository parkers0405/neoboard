use std::{ffi::OsStr, process::Command};

/// Construct a console helper for the GUI without changing its stdio or exit status.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn helper_has_no_console_and_keeps_stdout() {
        const CHILD: &str = "NEOBOARD_TEST_CONSOLE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetConsoleWindow() -> *mut std::ffi::c_void;
            }
            // SAFETY: this only queries the calling process; it takes no arguments.
            assert!(unsafe { GetConsoleWindow() }.is_null());
            println!("child has no console");
            return;
        }
        // Use the console-subsystem test harness as a real child, not the GUI executable.
        let out = super::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "background_process::tests::helper_has_no_console_and_keeps_stdout",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("child has no console"));
    }
}
