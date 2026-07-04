use anyhow::{anyhow, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

const TERM_PROGRAM_NAME: &str = "jterm3";
const TERM_PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const VTE_VERSION: &str = "7802";
const DEFAULT_SHELL_NAME: &str = "rsh";

// 声明全局环境变量指针
extern "C" {
    static environ: *const *const libc::c_char;
}

#[cfg(unix)]
mod unix_pty {
    use super::*;
    use std::path::Path;

    /// Outcome of polling the PTY fd alongside the shutdown pipe.
    pub enum ReaderPoll {
        Data,
        Timeout,
        Shutdown,
    }

    fn is_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }

    fn find_executable_in_path(exe_name: &str) -> Option<String> {
        let path_var = std::env::var_os("PATH")?;
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(exe_name))
            .find(|candidate| is_executable(candidate))
            .map(|p| p.to_string_lossy().to_string())
    }

    fn shell_from_passwd() -> Option<String> {
        let uid = unsafe { libc::getuid() };
        let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buf = vec![0u8; 16384];
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 || result.is_null() {
            return None;
        }
        let pwd = unsafe { pwd.assume_init() };
        if pwd.pw_shell.is_null() {
            return None;
        }
        let shell = unsafe { std::ffi::CStr::from_ptr(pwd.pw_shell) }
            .to_string_lossy()
            .to_string();
        if is_executable(Path::new(&shell)) {
            Some(shell)
        } else {
            None
        }
    }

    fn choose_shell(configured_shell: Option<&str>) -> String {
        // Priority 1: explicit config (needed when PATH is stripped by launchers like wofi)
        if let Some(path) = configured_shell {
            if is_executable(Path::new(path)) {
                return path.to_string();
            }
            eprintln!(
                "[PTY] Configured shell '{}' is not executable, falling back",
                path
            );
        }

        // Priority 2: rsh is jterm3's preferred shell. It gets a session id
        // argv below when one is available, while still letting users override
        // it through config.shell.
        if let Some(rsh_path) = find_executable_in_path(DEFAULT_SHELL_NAME) {
            return rsh_path;
        }

        // Priority 3: the user's login shell, matching VTE terminals such as
        // GNOME Terminal and Terminator. `$SHELL` is usually already absolute;
        // passwd is the fallback when launchers sanitize the environment.
        if let Some(shell) = std::env::var_os("SHELL").and_then(|s| s.into_string().ok()) {
            if is_executable(Path::new(&shell)) {
                return shell;
            }
        }
        if let Some(shell) = shell_from_passwd() {
            return shell;
        }

        // Priority 4: bash (fallback)
        if let Some(bash_path) = find_executable_in_path("bash") {
            return bash_path;
        }

        // Priority 5: sh (last resort)
        "sh".to_string()
    }

    pub struct Pty {
        master: RawFd,
        child_pid: i32,
        exit_code_cached: Option<i32>,
    }

    impl Pty {
        #[allow(dead_code)]
        pub fn new(cols: usize, rows: usize) -> Result<Self> {
            Self::new_with_cwd(cols, rows, None, None, None)
        }

        pub fn new_with_cwd(
            cols: usize,
            rows: usize,
            cwd: Option<&str>,
            session_id: Option<&str>,
            configured_shell: Option<&str>,
        ) -> Result<Self> {
            // SAFETY: 这个 unsafe 块包含多个 libc 系统调用用于 PTY 创建和进程 fork。
            // 所有的 libc 调用都检查了返回值并正确处理错误。
            // 文件描述符的生命周期被正确管理（成功时存储在 PtySession 中，失败时关闭）。
            // fork 后的子进程分支永不返回（通过 execve 或 exit），避免了未定义行为。
            unsafe {
                // 1. 创建 PTY
                let mut master = 0;
                let mut slave = 0;

                let win_size = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                if libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &win_size,
                ) != 0
                {
                    return Err(anyhow!("Failed to open PTY"));
                }

                // 2. 设置 master 非阻塞模式
                let flags = libc::fcntl(master, libc::F_GETFL, 0);
                if flags >= 0 {
                    let _ = libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }

                // 设置 FD_CLOEXEC，防止子进程继承
                let fd_flags = libc::fcntl(master, libc::F_GETFD, 0);
                if fd_flags >= 0 {
                    let _ = libc::fcntl(master, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
                }

                // Prepare everything that needs heap allocation or environment
                // lookups BEFORE fork(). iced spawns winit/wgpu worker threads, so
                // this is a multithreaded process; between fork() and execve() only
                // async-signal-safe calls are legal. malloc/getenv/setenv are not —
                // if another thread held the allocator lock at fork time the child
                // could deadlock. So argv and a custom envp are built here and the
                // child branch only reads already-allocated memory.
                let shell_path = choose_shell(configured_shell);
                let shell_name = std::path::Path::new(&shell_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sh")
                    .to_string();

                macro_rules! cstr_or_bail {
                    ($e:expr, $msg:expr) => {
                        match CString::new($e) {
                            Ok(c) => c,
                            Err(_) => {
                                libc::close(master);
                                libc::close(slave);
                                return Err(anyhow!($msg));
                            }
                        }
                    };
                }

                let shell_cstr = cstr_or_bail!(shell_path.clone(), "shell path contains NUL");
                let dash_shell_cstr =
                    cstr_or_bail!(format!("-{}", shell_name), "shell name contains NUL");
                let login_arg = if shell_name == "bash" {
                    Some(CString::new("-l").unwrap())
                } else {
                    None
                };
                let session_flag = CString::new("--session").unwrap();
                let session_id_cstr = session_id.and_then(|s| CString::new(s).ok());

                // argv pointers borrow the CStrings above; both outlive the fork.
                let mut argv_ptrs: Vec<*const libc::c_char> = Vec::new();
                argv_ptrs.push(dash_shell_cstr.as_ptr());
                if let Some(ref arg) = login_arg {
                    argv_ptrs.push(arg.as_ptr());
                }
                if shell_name == "rsh" {
                    if let Some(ref sid) = session_id_cstr {
                        argv_ptrs.push(session_flag.as_ptr());
                        argv_ptrs.push(sid.as_ptr());
                    }
                }
                argv_ptrs.push(std::ptr::null());

                // Build a custom envp: copy the current environment, override our
                // keys, and add LESS=FR only if the user hasn't set it. Doing this
                // here means the child never calls setenv (which mallocs).
                let mut env_cstrings: Vec<CString> = Vec::new();
                {
                    let overridden: [&str; 5] = [
                        "TERM",
                        "COLORTERM",
                        "TERM_PROGRAM",
                        "TERM_PROGRAM_VERSION",
                        "VTE_VERSION",
                    ];
                    let mut has_less = false;
                    let mut p = environ;
                    while !(*p).is_null() {
                        let bytes = std::ffi::CStr::from_ptr(*p).to_bytes();
                        let key = match bytes.iter().position(|&b| b == b'=') {
                            Some(i) => &bytes[..i],
                            None => bytes,
                        };
                        if key == b"LESS" {
                            has_less = true;
                        }
                        if !overridden.iter().any(|k| k.as_bytes() == key) {
                            if let Ok(c) = CString::new(bytes) {
                                env_cstrings.push(c);
                            }
                        }
                        p = p.offset(1);
                    }
                    env_cstrings.push(CString::new("TERM=xterm-256color").unwrap());
                    env_cstrings.push(CString::new("COLORTERM=truecolor").unwrap());
                    env_cstrings
                        .push(CString::new(format!("TERM_PROGRAM={}", TERM_PROGRAM_NAME)).unwrap());
                    env_cstrings.push(
                        CString::new(format!("TERM_PROGRAM_VERSION={}", TERM_PROGRAM_VERSION))
                            .unwrap(),
                    );
                    env_cstrings
                        .push(CString::new(format!("VTE_VERSION={}", VTE_VERSION)).unwrap());
                    // LESS=FR (not the default FRX, whose -X disables the alternate
                    // screen and leaks pager output into scrollback). Only set when
                    // the user hasn't configured LESS themselves.
                    if !has_less {
                        env_cstrings.push(CString::new("LESS=FR").unwrap());
                    }
                }
                let mut envp: Vec<*const libc::c_char> =
                    env_cstrings.iter().map(|c| c.as_ptr()).collect();
                envp.push(std::ptr::null());

                // cwd prepared before fork; chdir() itself is async-signal-safe.
                let cwd_cstr = match cwd {
                    Some(dir) => Some(cstr_or_bail!(dir, "working directory contains NUL")),
                    None => None,
                };

                // 3. Fork 子进程
                let fork_result = libc::fork();

                if fork_result < 0 {
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!("Failed to fork"));
                }

                if fork_result == 0 {
                    // ===== 子进程分支：以下只允许 async-signal-safe 调用 =====
                    libc::close(master);

                    // 父进程死亡信号：父进程(jterm3)退出时此进程收到 SIGTERM，
                    // 作为 SIGKILL/panic 情况下避免孤儿进程的最后一道防线。
                    #[cfg(target_os = "linux")]
                    {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    }

                    // 新建会话/进程组，使其成为会话 leader，便于按进程组发信号。
                    libc::setsid();

                    // 切换工作目录（CString 已在 fork 前构造好）。
                    if let Some(ref dir_cstr) = cwd_cstr {
                        if libc::chdir(dir_cstr.as_ptr()) != 0 {
                            libc::perror(b"chdir failed\0".as_ptr() as *const i8);
                            libc::exit(127);
                        }
                    }

                    // 设置 slave 为控制终端
                    if libc::ioctl(slave, libc::TIOCSCTTY, 0) != 0 {
                        libc::perror(b"ioctl TIOCSCTTY failed\0".as_ptr() as *const i8);
                    }

                    libc::dup2(slave, libc::STDIN_FILENO);
                    libc::dup2(slave, libc::STDOUT_FILENO);
                    libc::dup2(slave, libc::STDERR_FILENO);
                    if slave > libc::STDERR_FILENO {
                        libc::close(slave);
                    }

                    // 执行 shell，使用 fork 前构造好的 argv 与 envp。
                    libc::execve(shell_cstr.as_ptr(), argv_ptrs.as_ptr(), envp.as_ptr());

                    // execve 仅在出错时返回。
                    libc::perror(b"execve failed\0".as_ptr() as *const i8);
                    libc::exit(127);
                } else {
                    // 父进程分支
                    // 关闭 slave
                    libc::close(slave);

                    Ok(Pty {
                        master,
                        child_pid: fork_result as i32,
                        exit_code_cached: None,
                    })
                }
            }
        }

        pub fn get_child_pid(&self) -> i32 {
            self.child_pid
        }

        pub fn master_fd(&self) -> RawFd {
            self.master
        }

        pub fn wait_fd_readable(fd: RawFd, timeout_ms: i32) -> Result<bool> {
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };

            // SAFETY: poll_fd 是有效的栈上变量，libc::poll 接受可变指针和长度，
            // 超时参数是合法的毫秒值。poll 调用是原子的，不会导致数据竞争。
            let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
            if ready < 0 {
                Err(anyhow!(
                    "Failed to poll PTY: {}",
                    std::io::Error::last_os_error()
                ))
            } else if ready == 0 {
                Ok(false)
            } else if poll_fd.revents & libc::POLLNVAL != 0 {
                // fd was closed (e.g. the session was dropped). Surface this as
                // an error so the reader thread exits instead of busy-looping —
                // poll() returns immediately on an invalid fd, which otherwise
                // spins the CPU and leaks the thread forever.
                Err(anyhow!("PTY fd is invalid (POLLNVAL)"))
            } else {
                Ok((poll_fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0)
            }
        }

        /// Create a self-pipe used to wake the reader thread on shutdown.
        /// Returns (read_end, write_end). Closing the write end makes a `poll`
        /// on the read end report POLLHUP immediately.
        pub fn make_shutdown_pipe() -> Result<(RawFd, RawFd)> {
            let mut fds = [0 as RawFd; 2];
            // SAFETY: fds is a valid 2-element array; pipe() fills both ends.
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            if rc != 0 {
                return Err(anyhow!(
                    "Failed to create shutdown pipe: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok((fds[0], fds[1]))
        }

        /// Poll the PTY fd for readability while also watching `shutdown_fd`.
        /// Shutdown takes priority: if the shutdown pipe's write end was closed
        /// (POLLHUP) — or the PTY fd became invalid because the session was
        /// dropped — this returns `Shutdown` WITHOUT reporting data, so the
        /// caller never reads from a PTY fd whose number may have been reused.
        pub fn wait_fd_or_shutdown(
            fd: RawFd,
            shutdown_fd: RawFd,
            timeout_ms: i32,
        ) -> Result<ReaderPoll> {
            let mut fds = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: shutdown_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: fds is a valid 2-element array on the stack.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
            if ready < 0 {
                return Err(anyhow!(
                    "Failed to poll PTY: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if ready == 0 {
                return Ok(ReaderPoll::Timeout);
            }
            // Shutdown signaled (write end closed -> POLLHUP, or any activity).
            if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
            {
                return Ok(ReaderPoll::Shutdown);
            }
            // PTY fd closed out from under us -> treat as a clean shutdown.
            if fds[0].revents & libc::POLLNVAL != 0 {
                return Ok(ReaderPoll::Shutdown);
            }
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(ReaderPoll::Data);
            }
            Ok(ReaderPoll::Timeout)
        }

        /// Block until the fd is writable (POLLOUT) or the timeout elapses.
        pub fn wait_fd_writable(fd: RawFd, timeout_ms: i32) -> Result<bool> {
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
            if ready < 0 {
                Err(anyhow!(
                    "Failed to poll PTY: {}",
                    std::io::Error::last_os_error()
                ))
            } else if ready == 0 {
                Ok(false)
            } else {
                Ok((poll_fd.revents & (libc::POLLOUT | libc::POLLHUP | libc::POLLERR)) != 0)
            }
        }

        /// Single non-blocking write. Returns bytes written, `Ok(0)` if the
        /// buffer is full (WouldBlock), or an error for a real failure.
        pub fn write(&mut self, data: &[u8]) -> Result<usize> {
            // SAFETY: self.master 是有效的文件描述符，data.as_ptr() 指向有效的内存，
            // data.len() 是正确的长度。write 系统调用不会超出缓冲区边界。
            unsafe {
                let n = libc::write(self.master, data.as_ptr() as *const _, data.len());
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        Ok(0)
                    } else {
                        Err(anyhow!("Failed to write to PTY: {}", err))
                    }
                } else {
                    Ok(n as usize)
                }
            }
        }

        /// Single non-blocking read. `Ok(Some(n))` with n>0 means n bytes were
        /// read; `Ok(Some(0))` means WouldBlock (no data right now); `Ok(None)`
        /// means EOF — the child closed the PTY. Collapsing EOF and WouldBlock
        /// into a bare `Ok(0)` would let a read loop busy-spin on a hung-up PTY.
        pub fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>> {
            // SAFETY: self.master 是有效的文件描述符，buf.as_mut_ptr() 指向有效的可变内存，
            // buf.len() 是正确的缓冲区大小。read 不会超出边界。
            unsafe {
                let n = libc::read(self.master, buf.as_mut_ptr() as *mut _, buf.len());
                if n > 0 {
                    Ok(Some(n as usize))
                } else if n == 0 {
                    Ok(None) // EOF
                } else {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        Ok(Some(0))
                    } else {
                        Err(anyhow!("Failed to read from PTY: {}", err))
                    }
                }
            }
        }

        pub fn resize(&mut self, cols: usize, rows: usize) -> Result<()> {
            // SAFETY: win_size 是有效的栈上变量，符合 libc::winsize 的内存布局。
            // ioctl TIOCSWINSZ 调用是标准的 PTY 窗口大小设置操作。
            unsafe {
                let win_size = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };

                if libc::ioctl(
                    self.master,
                    libc::TIOCSWINSZ,
                    (&win_size) as *const _ as *mut libc::c_void,
                ) < 0
                {
                    return Err(anyhow!("Failed to resize PTY"));
                }
            }
            Ok(())
        }

        pub fn is_alive(&mut self) -> bool {
            // If we already have a cached exit code, the process is not alive.
            if self.exit_code_cached.is_some() {
                return false;
            }

            // SAFETY: waitpid 使用 WNOHANG 非阻塞检查子进程状态。
            // status 是有效的栈变量，child_pid 是有效的进程 ID。
            unsafe {
                let mut status = 0;
                let result = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
                if result == 0 {
                    // Still running.
                    true
                } else if result > 0 {
                    // The child changed state and we just reaped it — decode and
                    // cache the real exit status here so it isn't lost (a later
                    // wait_timeout would otherwise hit ECHILD and fabricate 0).
                    let code = if libc::WIFEXITED(status) {
                        libc::WEXITSTATUS(status) as i32
                    } else if libc::WIFSIGNALED(status) {
                        -(libc::WTERMSIG(status) as i32)
                    } else {
                        -1
                    };
                    self.exit_code_cached = Some(code);
                    false
                } else {
                    // waitpid error (typically ECHILD: already reaped elsewhere).
                    // Treat as dead so callers stop polling.
                    self.exit_code_cached = Some(0);
                    false
                }
            }
        }

        pub fn wait_timeout(&mut self, _timeout_ms: u64) -> Result<i32> {
            // If we already have a cached exit code, return it directly
            if let Some(code) = self.exit_code_cached {
                return Ok(code);
            }

            // SAFETY: waitpid 阻塞等待子进程退出。status 是有效的栈变量，
            // child_pid 是我们 fork 创建的有效进程 ID。
            unsafe {
                let mut status = 0;
                let result = libc::waitpid(self.child_pid, &mut status, 0);

                if result < 0 {
                    // If waitpid fails with ECHILD, it means the process has already been waited on
                    // In this case, return a default exit code of 0
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::ECHILD) {
                        crate::debug_log!("[PTY] waitpid returned ECHILD, process already reaped");
                        self.exit_code_cached = Some(0);
                        return Ok(0);
                    }
                    Err(anyhow!("waitpid failed: {}", err))
                } else {
                    let exit_code = if libc::WIFEXITED(status) {
                        libc::WEXITSTATUS(status) as i32
                    } else if libc::WIFSIGNALED(status) {
                        -(libc::WTERMSIG(status) as i32)
                    } else {
                        -1
                    };
                    self.exit_code_cached = Some(exit_code);
                    Ok(exit_code)
                }
            }
        }

        pub fn terminate(&mut self) -> Result<()> {
            // SAFETY: kill 向进程组(负 PID)发送信号；child_pid 通过 setsid 成为
            // 会话/进程组 leader。即使进程已退出，kill 也只是返回错误、无 UB。
            let pgid = -self.child_pid;
            unsafe {
                let _ = libc::kill(pgid, libc::SIGHUP);
                let _ = libc::kill(self.child_pid, libc::SIGTERM);
            }

            // Non-blocking reap: if the child already exited, we're done.
            if !self.is_alive() {
                return Ok(());
            }

            // Otherwise escalate on a detached thread instead of sleeping here —
            // terminate() runs from Drop, often on the UI thread, which must not
            // block for the grace period. The thread only captures the pid (in
            // its own process group), so there's no aliasing with `self`.
            let child_pid = self.child_pid;
            std::thread::spawn(move || unsafe {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let mut status = 0;
                if libc::waitpid(child_pid, &mut status, libc::WNOHANG) == 0 {
                    // Still alive after the grace period: force kill, then reap.
                    let _ = libc::kill(-child_pid, libc::SIGKILL);
                    let _ = libc::kill(child_pid, libc::SIGKILL);
                    let _ = libc::waitpid(child_pid, &mut status, 0);
                }
            });
            // The child is being torn down; record that so is_alive() stops
            // polling. The detached thread owns the actual reaping.
            self.exit_code_cached = Some(-15);
            Ok(())
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            if self.is_alive() {
                let _ = self.terminate();
            }
            // SAFETY: close 关闭文件描述符。master 是有效的 fd，
            // 关闭后不会再使用（因为这是 Drop 实现）。
            unsafe {
                let _ = libc::close(self.master);
            }
        }
    }
}

#[cfg(windows)]
mod windows_pty {
    use super::*;

    pub struct Pty;

    impl Pty {
        pub fn new(_cols: usize, _rows: usize) -> Result<Self> {
            Err(anyhow!("PTY support not yet implemented on Windows"))
        }

        pub fn write(&mut self, _data: &[u8]) -> Result<usize> {
            Err(anyhow!("PTY not available"))
        }

        pub fn read(&mut self, _buf: &mut [u8]) -> Result<Option<usize>> {
            Err(anyhow!("PTY not available"))
        }

        pub fn resize(&mut self, _cols: usize, _rows: usize) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }

        pub fn is_alive(&self) -> bool {
            false
        }

        pub fn wait_timeout(&mut self, _timeout_ms: u64) -> Result<i32> {
            Err(anyhow!("PTY not available"))
        }

        pub fn terminate(&mut self) -> Result<()> {
            Err(anyhow!("PTY not available"))
        }
    }
}

#[cfg(unix)]
pub use unix_pty::{Pty, ReaderPoll};

#[cfg(windows)]
pub use windows_pty::Pty;
