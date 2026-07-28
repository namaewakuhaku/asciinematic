use std::{env, path::PathBuf};

/// Select the interactive shell that best matches the current user's platform settings.
pub fn user_shell() -> PathBuf {
    configured_shell()
        .or_else(platform_shell)
        .unwrap_or_else(platform_fallback)
}

fn configured_shell() -> Option<PathBuf> {
    env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn platform_shell() -> Option<PathBuf> {
    use std::{ffi::CStr, mem::MaybeUninit, os::unix::ffi::OsStrExt, ptr};

    let buffer_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_size = if buffer_size <= 0 {
        16 * 1024
    } else {
        usize::try_from(buffer_size).ok()?
    };
    let mut buffer = vec![0_u8; buffer_size];
    let mut passwd = MaybeUninit::<libc::passwd>::uninit();
    let mut result = ptr::null_mut();

    // getpwuid_r writes `passwd` and points `result` at it only when the current
    // account was found. The backing byte buffer remains alive while pw_shell is read.
    let status = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            passwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }

    let passwd = unsafe { passwd.assume_init() };
    if passwd.pw_shell.is_null() {
        return None;
    }
    let shell = unsafe { CStr::from_ptr(passwd.pw_shell) };
    let shell = PathBuf::from(std::ffi::OsStr::from_bytes(shell.to_bytes()));
    (!shell.as_os_str().is_empty()).then_some(shell)
}

#[cfg(unix)]
fn platform_fallback() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn platform_shell() -> Option<PathBuf> {
    find_on_path("pwsh.exe")
        .or_else(windows_powershell)
        .or_else(|| {
            env::var_os("ComSpec")
                .or_else(|| env::var_os("COMSPEC"))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

#[cfg(windows)]
fn windows_powershell() -> Option<PathBuf> {
    let path = env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn find_on_path(executable: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(executable))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn platform_fallback() -> PathBuf {
    PathBuf::from("cmd.exe")
}

#[cfg(test)]
mod tests {
    use super::user_shell;

    #[test]
    fn selected_shell_is_not_empty() {
        assert!(!user_shell().as_os_str().is_empty());
    }
}
