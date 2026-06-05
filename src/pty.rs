use anyhow::{anyhow, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

const TERM_PROGRAM_NAME: &str = "jterm3";
const TERM_PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
const VTE_VERSION: &str = "7802";

// 声明全局环境变量指针
extern "C" {
    static environ: *const *const libc::c_char;
}

#[cfg(unix)]
mod unix_pty {
    use super::*;
    use std::path::Path;

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

    fn choose_shell(configured_shell: Option<&str>) -> String {
        // Priority 1: explicit config / env var (needed when PATH is stripped by launchers like wofi)
        if let Some(path) = configured_shell {
            if is_executable(Path::new(path)) {
                return path.to_string();
            }
            eprintln!("[PTY] Configured shell '{}' is not executable, falling back", path);
        }

        // Priority 2: rsh (preferred shell with advanced features)
        if let Some(rsh_path) = find_executable_in_path("rsh") {
            return rsh_path;
        }

        // Priority 3: bash (fallback)
        if let Some(bash_path) = find_executable_in_path("bash") {
            return bash_path;
        }

        // Priority 4: sh (last resort)
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

                // 3. Fork 子进程
                let fork_result = libc::fork();

                if fork_result < 0 {
                    libc::close(master);
                    libc::close(slave);
                    return Err(anyhow!("Failed to fork"));
                }

                if fork_result == 0 {
                    // 子进程分支
                    // 关闭 master
                    libc::close(master);

                    // 【关键】设置父进程死亡信号：当父进程(jterm2)死亡时，此进程会收到SIGTERM
                    // 这是最后一道防线，确保即使jterm2被SIGKILL强制杀死或panic崩溃，
                    // rsh进程也会收到退出信号，不会变成孤儿进程继续运行。
                    //
                    // 配合其他清理机制：
                    // - 正常退出：ShellSession::Drop 清理进程组
                    // - SIGINT/SIGTERM：信号处理器触发正常退出
                    // - SIGKILL/panic：PR_SET_PDEATHSIG 确保子进程退出
                    #[cfg(target_os = "linux")]
                    {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    }

                    // 创建新的会话和进程组（将此进程设为会话leader）
                    // 这允许我们通过负PID向整个进程组发送信号，杀死shell的所有子进程
                    libc::setsid();

                    // 如果指定了工作目录，在执行 shell 前改变目录
                    if let Some(dir) = cwd {
                        let dir_cstr = match CString::new(dir) {
                            Ok(s) => s,
                            Err(_) => {
                                libc::perror(b"Invalid working directory\0".as_ptr() as *const i8);
                                libc::exit(127);
                            }
                        };
                        if libc::chdir(dir_cstr.as_ptr()) != 0 {
                            libc::perror(b"chdir failed\0".as_ptr() as *const i8);
                            libc::exit(127);
                        }
                    }

                    // 设置 slave 为控制终端
                    if libc::ioctl(slave, libc::TIOCSCTTY, 0) != 0 {
                        libc::perror(b"ioctl TIOCSCTTY failed\0".as_ptr() as *const i8);
                    }

                    // 重定向 stdin/stdout/stderr 到 PTY slave
                    libc::dup2(slave, libc::STDIN_FILENO);
                    libc::dup2(slave, libc::STDOUT_FILENO);
                    libc::dup2(slave, libc::STDERR_FILENO);

                    // 关闭原始 slave fd（因为已经重定向了）
                    if slave > libc::STDERR_FILENO {
                        libc::close(slave);
                    }

                    // 选择 shell：优先 rsh，fallback bash，最后 sh
                    let shell_path = choose_shell(configured_shell);

                    let term_name = CString::new("TERM").unwrap();
                    let term_value = CString::new("xterm-256color").unwrap();
                    libc::setenv(term_name.as_ptr(), term_value.as_ptr(), 1);

                    let color_term_name = CString::new("COLORTERM").unwrap();
                    let color_term_value = CString::new("truecolor").unwrap();
                    libc::setenv(color_term_name.as_ptr(), color_term_value.as_ptr(), 1);

                    let term_program_name = CString::new("TERM_PROGRAM").unwrap();
                    let term_program_value = CString::new(TERM_PROGRAM_NAME).unwrap();
                    libc::setenv(term_program_name.as_ptr(), term_program_value.as_ptr(), 1);

                    let term_program_version_name = CString::new("TERM_PROGRAM_VERSION").unwrap();
                    let term_program_version_value = CString::new(TERM_PROGRAM_VERSION).unwrap();
                    libc::setenv(
                        term_program_version_name.as_ptr(),
                        term_program_version_value.as_ptr(),
                        1,
                    );

                    let vte_version_name = CString::new("VTE_VERSION").unwrap();
                    let vte_version_value = CString::new(VTE_VERSION).unwrap();
                    libc::setenv(vte_version_name.as_ptr(), vte_version_value.as_ptr(), 1);

                    // Set LESS=FR (without -X) so that programs like git use
                    // the alternate screen properly. Git defaults to LESS=FRX
                    // where -X disables alternate screen, causing pager output
                    // to leak into scrollback. Only set if user hasn't already
                    // configured LESS.
                    let less_name = CString::new("LESS").unwrap();
                    let less_value = CString::new("FR").unwrap();
                    libc::setenv(less_name.as_ptr(), less_value.as_ptr(), 0); // 0 = don't overwrite

                    // 创建 C 字符串
                    let shell_cstr = match CString::new(shell_path.clone()) {
                        Ok(s) => s,
                        Err(_) => {
                            libc::perror(b"Invalid shell path\0".as_ptr() as *const i8);
                            libc::exit(127);
                        }
                    };

                    // 根据 shell 名称确定 argv[0]（带前缀 "-" 表示登录 shell）
                    let shell_name = std::path::Path::new(&shell_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("sh");

                    let dash_shell = format!("-{}", shell_name);
                    let dash_shell_cstr = match CString::new(dash_shell.clone()) {
                        Ok(s) => s,
                        Err(_) => {
                            libc::perror(b"Invalid shell name\0".as_ptr() as *const i8);
                            libc::exit(127);
                        }
                    };

                    // Build argv as a Vec for flexibility
                    let mut argv_ptrs: Vec<*const libc::c_char> = Vec::new();
                    argv_ptrs.push(dash_shell_cstr.as_ptr());

                    // 如果是 bash，添加 -l 参数
                    let login_arg = if shell_name == "bash" {
                        Some(CString::new("-l").unwrap())
                    } else {
                        None
                    };
                    if let Some(ref arg) = login_arg {
                        argv_ptrs.push(arg.as_ptr());
                    }

                    // 如果是 rsh 且有 session_id，添加 --session <id>
                    let session_flag = CString::new("--session").unwrap();
                    let session_id_cstr = session_id.and_then(|s| CString::new(s).ok());
                    if shell_name == "rsh" {
                        if let Some(ref sid) = session_id_cstr {
                            argv_ptrs.push(session_flag.as_ptr());
                            argv_ptrs.push(sid.as_ptr());
                        }
                    }

                    argv_ptrs.push(std::ptr::null());

                    // 执行 shell，继承当前环境
                    libc::execve(shell_cstr.as_ptr(), argv_ptrs.as_ptr(), environ);

                    // 如果 execve 返回，说明出错
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
            } else {
                Ok((poll_fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0)
            }
        }

        /// Single non-blocking write. Returns bytes written, or WouldBlock if buffer full.
        pub fn write(&mut self, data: &[u8]) -> Result<usize> {
            // SAFETY: self.master 是有效的文件描述符，data.as_ptr() 指向有效的内存，
            // data.len() 是正确的长度。write 系统调用不会超出缓冲区边界。
            unsafe {
                let n = libc::write(self.master, data.as_ptr() as *const _, data.len());
                if n < 0 {
                    Err(anyhow!(
                        "Failed to write to PTY: {}",
                        std::io::Error::last_os_error()
                    ))
                } else {
                    Ok(n as usize)
                }
            }
        }

        pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            // SAFETY: self.master 是有效的文件描述符，buf.as_mut_ptr() 指向有效的可变内存，
            // buf.len() 是正确的缓冲区大小。read 不会超出边界。
            unsafe {
                let n = libc::read(self.master, buf.as_mut_ptr() as *mut _, buf.len());
                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        Ok(0)
                    } else {
                        Err(anyhow!("Failed to read from PTY: {}", err))
                    }
                } else {
                    Ok(n as usize)
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

        pub fn is_alive(&self) -> bool {
            // If we already have a cached exit code, the process is not alive
            if self.exit_code_cached.is_some() {
                return false;
            }

            // SAFETY: waitpid 使用 WNOHANG 非阻塞检查子进程状态。
            // status 是有效的栈变量，child_pid 是有效的进程 ID。
            unsafe {
                let mut status = 0;
                let result = libc::waitpid(self.child_pid, &mut status, libc::WNOHANG);
                result == 0 // 0 表示子进程还活着
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
            // SAFETY: kill 系统调用发送信号到进程/进程组。
            // 负 PID 是向进程组发送信号的标准方式。child_pid 是有效的进程 ID。
            // 即使进程已经退出，kill 调用也是安全的（会返回错误但不会导致 UB）。
            unsafe {
                // 向整个进程组发送 SIGHUP（子进程通过 setsid() 创建了新会话）
                // 使用负 PID 向进程组发信号，确保 shell 的子进程也被杀死
                let pgid = -self.child_pid;
                let _ = libc::kill(pgid, libc::SIGHUP);
                let _ = libc::kill(self.child_pid, libc::SIGTERM);

                // 给进程时间优雅退出
                std::thread::sleep(std::time::Duration::from_millis(50));

                // 如果仍未退出，强制杀死
                if self.is_alive() {
                    let _ = libc::kill(pgid, libc::SIGKILL);
                    let _ = libc::kill(self.child_pid, libc::SIGKILL);
                    // 回收僵尸进程
                    let mut status = 0;
                    let _ = libc::waitpid(self.child_pid, &mut status, 0);
                    self.exit_code_cached = Some(-9);
                }
                Ok(())
            }
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

        pub fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
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
pub use unix_pty::Pty;

#[cfg(windows)]
pub use windows_pty::Pty;
