//! Supervised child processes. Captured tool output is bounded; user programs
//! inherit standard streams and have no implicit timeout.
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static TIMED_OUT: AtomicBool = AtomicBool::new(false);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

pub fn install_handlers() -> Result<(), String> {
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT.get_or_init(platform_handlers).clone()
}

#[cfg(unix)]
fn platform_handlers() -> Result<(), String> {
    extern "C" fn signal(_signal: libc::c_int) {
        // AtomicBool is lock-free; the handler does no allocation or I/O.
        CANCELLED.store(true, Ordering::SeqCst);
    }
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = signal as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    for number in [libc::SIGINT, libc::SIGTERM] {
        if unsafe { libc::sigaction(number, &action, std::ptr::null_mut()) } < 0 {
            return Err(format!(
                "cannot install cancellation handler: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}
#[cfg(windows)]
fn platform_handlers() -> Result<(), String> {
    unsafe extern "system" fn handler(event: u32) -> i32 {
        if event == 0 || event == 1 {
            // CTRL_C_EVENT / CTRL_BREAK_EVENT
            CANCELLED.store(true, Ordering::SeqCst);
            1
        } else {
            0
        }
    }
    if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1) } == 0
    {
        return Err(format!(
            "cannot install cancellation handler: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}
pub fn failure_code() -> i32 {
    if cancelled() {
        130
    } else if TIMED_OUT.load(Ordering::SeqCst) {
        124
    } else {
        1
    }
}
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(1)
    }
    #[cfg(windows)]
    {
        1
    }
}

pub fn output(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    captured_output(command, Some(timeout))
}

pub fn output_without_deadline(command: &mut Command) -> Result<Output, String> {
    captured_output(command, None)
}

fn captured_output(command: &mut Command, timeout: Option<Duration>) -> Result<Output, String> {
    install_handlers()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, group) = spawn(command, false)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let reader = |stream: Box<dyn Read + Send>, overflow: Arc<AtomicBool>| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = (|| {
                let mut bytes = Vec::new();
                let mut stream = stream;
                let mut buffer = [0u8; 8192];
                loop {
                    let size = stream.read(&mut buffer)?;
                    if size == 0 {
                        break;
                    }
                    if bytes.len() + size > OUTPUT_LIMIT {
                        overflow.store(true, Ordering::SeqCst);
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..size]);
                }
                Ok::<_, std::io::Error>(bytes)
            })();
            let _ = tx.send(result);
        });
        rx
    };
    let stdout = reader(Box::new(child.stdout.take().unwrap()), overflow.clone());
    let stderr = reader(Box::new(child.stderr.take().unwrap()), overflow.clone());
    let status = wait(&mut child, &group, timeout, &overflow);
    // Reap helpers still holding captured pipes even when their Git parent exited.
    group.terminate(true);
    let stdout = stdout
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "stdout pipe did not close after child termination")?
        .map_err(|e| e.to_string())?;
    let stderr = stderr
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "stderr pipe did not close after child termination")?
        .map_err(|e| e.to_string())?;
    let status = status?;
    if overflow.load(Ordering::SeqCst) {
        return Err("child output exceeds 8 MiB".into());
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

pub fn status(
    command: &mut Command,
    timeout: Option<Duration>,
    interactive: bool,
) -> Result<ExitStatus, String> {
    install_handlers()?;
    let (mut child, group) = spawn(command, interactive)?;
    let result = wait(&mut child, &group, timeout, &AtomicBool::new(false));
    #[cfg(windows)]
    if interactive && result.is_ok() {
        if let Err(error) = group.preserve_descendants() {
            use std::io::Write;
            // The program has completed: cleanup must not replace its exit code.
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: cannot preserve background processes after program exit: {error}"
            );
        }
    }
    result
}

fn wait(
    child: &mut Child,
    group: &Group,
    timeout: Option<Duration>,
    overflow: &AtomicBool,
) -> Result<ExitStatus, String> {
    let started = Instant::now();
    let result = loop {
        if cancelled() {
            break Err("operation cancelled".to_string());
        }
        if timeout.is_some_and(|limit| started.elapsed() >= limit) {
            TIMED_OUT.store(true, Ordering::SeqCst);
            break Err("child process timed out".to_string());
        }
        if overflow.load(Ordering::SeqCst) {
            break Err("child output exceeds 8 MiB".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(e.to_string()),
        }
    };
    if result.is_err() {
        group.terminate(false);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        group.terminate(true);
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(unix)]
struct Group {
    pid: i32,
    foreground: Option<i32>,
}
#[cfg(unix)]
fn spawn(command: &mut Command, interactive: bool) -> Result<(Child, Group), String> {
    use std::os::unix::process::CommandExt;
    let program = std::path::Path::new(command.get_program());
    if program.is_absolute() {
        std::fs::metadata(program).map_err(|error| error.to_string())?;
    }
    command.process_group(0);
    let foreground = if interactive {
        let previous = unsafe { libc::tcgetpgrp(0) };
        (previous >= 0 && unsafe { libc::getpgrp() } == previous).then_some(previous)
    } else {
        None
    };
    if foreground.is_some() {
        // Only async-signal-safe libc operations run between fork and exec.
        // Hand over the terminal before the program can attempt its first read.
        unsafe {
            command.pre_exec(|| {
                let signal = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                let result = libc::tcsetpgrp(0, libc::getpid());
                libc::signal(libc::SIGTTOU, signal);
                if result < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let mut group = Group { pid: 0, foreground };
    let child = command.spawn().map_err(|e| e.to_string())?;
    group.pid = child.id() as i32;
    Ok((child, group))
}
#[cfg(unix)]
impl Group {
    fn terminate(&self, force: bool) {
        if self.pid == 0 {
            return;
        }
        unsafe {
            libc::kill(-self.pid, if force { libc::SIGKILL } else { libc::SIGINT });
        }
    }
}
#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        if let Some(previous) = self.foreground {
            let signal = unsafe { libc::signal(libc::SIGTTOU, libc::SIG_IGN) };
            unsafe {
                libc::tcsetpgrp(0, previous);
                libc::signal(libc::SIGTTOU, signal);
            }
        }
    }
}

#[cfg(windows)]
struct Group {
    job: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
}
#[cfg(windows)]
fn spawn(command: &mut Command, _interactive: bool) -> Result<(Child, Group), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::*;
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut group = Group { job, pid: 0 };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED};
    command.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    if unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
        let error = std::io::Error::last_os_error().to_string();
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("cannot supervise child process tree: {error}"));
    }
    group.pid = child.id();
    if let Err(error) = resume_initial_thread(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok((child, group))
}

#[cfg(windows)]
fn resume_initial_thread(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    // The child is suspended and assigned to its job before its sole initial
    // thread can create descendants. Do not permit an unsupervised fallback.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let result = (|| {
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut present = unsafe { Thread32First(snapshot, &mut entry) };
        while present != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let resumed = unsafe { ResumeThread(thread) };
                let error = std::io::Error::last_os_error();
                unsafe {
                    CloseHandle(thread);
                }
                return if resumed == u32::MAX {
                    Err(error.to_string())
                } else {
                    Ok(())
                };
            }
            present = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        Err("suspended child initial thread not found".into())
    })();
    unsafe {
        CloseHandle(snapshot);
    }
    result
}
#[cfg(windows)]
impl Group {
    fn terminate(&self, force: bool) {
        if force {
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job, 130);
            }
        } else if self.pid != 0 {
            unsafe {
                windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(1, self.pid);
            }
        }
    }
    fn preserve_descendants(&self) -> Result<(), String> {
        use windows_sys::Win32::System::JobObjects::*;
        // A normally completed user program may intentionally leave background
        // children. Cancellation and parent death still terminate the job.
        let limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            SetInformationJobObject(
                self.job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "subprocess fixture, launched by supervision tests"]
    fn child_fixture() {
        match std::env::var("VEX_PROCESS_FIXTURE").unwrap().as_str() {
            "wait" => std::thread::sleep(Duration::from_secs(20)),
            "overflow" => {
                use std::io::Write;
                std::io::stdout()
                    .write_all(&vec![b'x'; OUTPUT_LIMIT + 65536])
                    .unwrap();
            }
            "exit" => std::process::exit(42),
            _ => panic!("unknown fixture"),
        }
    }
    fn command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("VEX_PROCESS_FIXTURE", mode);
        command
    }
    #[test]
    fn output_limit_and_timeout_terminate_children() {
        let start = Instant::now();
        assert!(output(&mut command("wait"), Duration::from_millis(100))
            .unwrap_err()
            .contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(6));
        assert!(output(&mut command("overflow"), Duration::from_secs(10))
            .unwrap_err()
            .contains("8 MiB"));
        let result = output(&mut command("exit"), Duration::from_secs(10)).unwrap();
        assert_eq!(exit_code(result.status), 42);
    }
}
