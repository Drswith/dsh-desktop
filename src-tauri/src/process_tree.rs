//! 启动并清理 dsh 及其普通子进程。
//!
//! Unix 上让 dsh 成为独立进程组组长，向负的进程组 ID 发信号即可覆盖
//! shell、脚本和 worker。Windows 上使用 Job Object；Job 句柄关闭时，
//! Windows 会把 Job 中的进程一起终止，覆盖启动器崩溃的场景。

use std::io;
use std::process::{Child, ChildStdout, Command};

pub struct ManagedChild {
    child: Child,
    control: platform::Control,
}

impl ManagedChild {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        platform::prepare(command);
        let child = command.spawn()?;
        let control = platform::attach(&child)?;
        Ok(Self { child, control })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Unix 返回进程组 ID；Windows 由 Job Object 管理，不使用数值组 ID。
    pub fn process_group_id(&self) -> Option<u32> {
        platform::group_id(self.id())
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// 先优雅终止，超时后强制终止整个进程树。
    pub fn terminate(&mut self) {
        platform::terminate(&mut self.child, &mut self.control);
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.terminate();
        }
    }
}

/// 清理启动器上次留下的进程组。旧记录没有组 ID 时由调用方保留旧的单进程
/// fallback，避免把一个没有被本启动器创建的进程组当成自己的目标。
pub fn terminate_record(pid: u32, group_id: Option<u32>) {
    platform::terminate_record(pid, group_id);
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::thread;
    use std::time::{Duration, Instant};

    pub struct Control {
        group_id: u32,
    }

    pub fn prepare(command: &mut Command) {
        // `pre_exec` 在 fork 后、exec 前运行；不能在父进程 spawn 返回后再
        // setpgid，否则 dsh 可能已经创建了子进程并继承了启动器的进程组。
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }

    pub fn attach(child: &Child) -> io::Result<Control> {
        Ok(Control { group_id: child.id() })
    }

    pub fn group_id(pid: u32) -> Option<u32> {
        Some(pid)
    }

    pub fn terminate(child: &mut Child, control: &mut Control) {
        terminate_group(control.group_id);
        let _ = child.wait();
    }

    pub fn terminate_record(pid: u32, group_id: Option<u32>) {
        if let Some(group_id) = group_id {
            terminate_group(group_id);
        } else {
            terminate_pid(pid);
        }
    }

    fn terminate_group(group_id: u32) {
        if group_id == 0 {
            return;
        }

        let _ = signal_group(group_id, libc::SIGTERM);
        wait_for_group(group_id, Duration::from_secs(3));
        if group_exists(group_id) {
            let _ = signal_group(group_id, libc::SIGKILL);
            wait_for_group(group_id, Duration::from_secs(1));
        }
    }

    fn terminate_pid(pid: u32) {
        if pid == 0 {
            return;
        }

        let pid = pid as libc::pid_t;
        unsafe {
            let _ = libc::kill(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while pid_exists(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        if pid_exists(pid) {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    fn signal_group(group_id: u32, signal: libc::c_int) -> bool {
        let result = unsafe { libc::kill(-(group_id as libc::pid_t), signal) };
        result == 0
    }

    fn group_exists(group_id: u32) -> bool {
        signal_group(group_id, 0)
    }

    fn pid_exists(pid: libc::pid_t) -> bool {
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn wait_for_group(group_id: u32, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while group_exists(group_id) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::Stdio;

        #[test]
        fn child_runs_in_its_own_process_group_and_group_terminates() {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", "sleep 30"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            let mut child = ManagedChild::spawn(&mut command).unwrap();
            let pid = child.id();
            assert_eq!(group_id(pid), Some(pid));
            assert!(group_exists(pid));

            child.terminate();
            assert!(!group_exists(pid));
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Control {
        job: Option<HANDLE>,
    }

    pub fn prepare(_command: &mut Command) {}

    pub fn attach(child: &Child) -> io::Result<Control> {
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Ok(Control { job: None });
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
        };
        let assigned =
            configured && unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) != 0 };

        if assigned {
            Ok(Control { job: Some(job) })
        } else {
            unsafe { CloseHandle(job) };
            Ok(Control { job: None })
        }
    }

    pub fn group_id(_pid: u32) -> Option<u32> {
        None
    }

    pub fn terminate(child: &mut Child, control: &mut Control) {
        // Windows 没有与 Unix SIGTERM 等价的通用子进程信号；Job Object
        // 提供可靠的整棵进程树强制终止，且句柄关闭时也会自动清理。
        if let Some(job) = control.job {
            unsafe {
                let _ = TerminateJobObject(job, 1);
            }
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }

    pub fn terminate_record(_pid: u32, _group_id: Option<u32>) {
        // 成功加入 Job Object 的进程会在启动器崩溃、Job 句柄关闭时自动终止。
        // 旧记录或 Job 创建失败的 fallback 仍由 sysinfo 的直接 kill 处理。
    }

    impl Drop for Control {
        fn drop(&mut self) {
            if let Some(job) = self.job.take() {
                unsafe { CloseHandle(job) };
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;

    pub struct Control;

    pub fn prepare(_command: &mut Command) {}
    pub fn attach(_child: &Child) -> io::Result<Control> {
        Ok(Control)
    }
    pub fn group_id(_pid: u32) -> Option<u32> {
        None
    }
    pub fn terminate(child: &mut Child, _control: &mut Control) {
        let _ = child.kill();
        let _ = child.wait();
    }
    pub fn terminate_record(_pid: u32, _group_id: Option<u32>) {}
}
