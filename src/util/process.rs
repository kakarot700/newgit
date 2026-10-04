//! Child-process management for bounded external Git work.
//!
//! When a deadline is present, a watchdog kills the child process group (and
//! descendants) rather than only terminating the immediate Git process.

use std::io;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// A spawned process plus optional wall-clock deadline and reaping behavior.
#[derive(Debug)]
pub struct ManagedChild {
    child: Option<Child>,
    deadline: Option<ChildDeadline>,
    completed: bool,
}

impl ManagedChild {
    /// Spawn `command` in a fresh process group with an optional total runtime
    /// deadline. If watchdog setup fails, the newly spawned child is killed
    /// and reaped before returning the error.
    pub fn spawn(command: &mut Command, timeout: Option<Duration>) -> io::Result<Self> {
        configure_process_group(command);
        let mut child = command.spawn()?;
        let deadline = match timeout {
            Some(timeout) => match ChildDeadline::start(child.id(), timeout) {
                Ok(deadline) => Some(deadline),
                Err(error) => {
                    kill_process_tree(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            },
            None => None,
        };
        Ok(Self {
            child: Some(child),
            deadline,
            completed: false,
        })
    }

    pub fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("managed child is present until consumed")
    }

    /// Wait for the child. Returns whether the watchdog deadline fired.
    pub fn wait(&mut self) -> io::Result<(ExitStatus, bool)> {
        let status = self.child_mut().wait()?;
        let timed_out = self.deadline.take().is_some_and(ChildDeadline::finish);
        self.completed = true;
        Ok((status, timed_out))
    }

    /// Collect stdout/stderr and wait. The watchdog remains active while the
    /// process is running or its output pipes are being drained.
    pub fn wait_with_output(mut self) -> io::Result<(Output, bool)> {
        let child = self
            .child
            .take()
            .expect("managed child is present until consumed");
        let output = child.wait_with_output();
        let timed_out = self.deadline.take().is_some_and(ChildDeadline::finish);
        self.completed = true;
        output.map(|output| (output, timed_out))
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Some(pid) = self
            .child
            .as_ref()
            .map(Child::id)
            .or_else(|| self.deadline.as_ref().map(|deadline| deadline.pid))
        {
            kill_process_tree(pid);
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(deadline) = self.deadline.take() {
            let _ = deadline.finish();
        }
    }
}

#[derive(Debug)]
struct ChildDeadline {
    pid: u32,
    stop: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
    expired: Arc<AtomicBool>,
    completed: bool,
}

impl ChildDeadline {
    fn start(pid: u32, timeout: Duration) -> io::Result<Self> {
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::clone(&expired);
        let worker = thread::Builder::new()
            .name("newgit-git-deadline".into())
            .spawn(move || {
                if matches!(
                    stop_rx.recv_timeout(timeout),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    timed_out.store(true, Ordering::Release);
                    kill_process_tree(pid);
                }
            })?;
        Ok(Self {
            pid,
            stop: Some(stop_tx),
            worker: Some(worker),
            expired,
            completed: false,
        })
    }

    fn finish(mut self) -> bool {
        self.stop_and_join();
        self.completed = true;
        self.expired.load(Ordering::Acquire)
    }

    fn stop_and_join(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ChildDeadline {
    fn drop(&mut self) {
        if !self.completed {
            kill_process_tree(self.pid);
            self.stop_and_join();
        }
    }
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Best-effort force termination of the direct child and all descendants.
/// Git subprocesses are started in their own process group on Unix; Windows
/// uses taskkill's process-tree mode. The caller still waits/reaps its child.
fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        let group = format!("-{pid}");
        let group_kill = Command::new("/bin/kill")
            .args(["-KILL", "--", group.as_str()])
            .output();
        if !group_kill.is_ok_and(|output| output.status.success()) {
            let direct = pid.to_string();
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", direct.as_str()])
                .output();
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
}

pub(crate) fn terminate_process_tree(pid: u32) {
    kill_process_tree(pid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::process::Stdio;
    use std::time::Instant;

    #[cfg(unix)]
    #[test]
    fn deadline_kills_and_reaps_child_process_group() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let started = Instant::now();
        let mut managed =
            ManagedChild::spawn(&mut command, Some(Duration::from_millis(75))).unwrap();
        let mut child_pid = String::new();
        std::io::BufReader::new(managed.child_mut().stdout.take().unwrap())
            .read_line(&mut child_pid)
            .unwrap();
        let (status, timed_out) = managed.wait().unwrap();
        assert!(timed_out, "watchdog did not report the expired deadline");
        assert!(!status.success(), "timed-out process exited successfully");
        assert!(started.elapsed() < Duration::from_secs(5));

        let child_pid = child_pid.trim();
        assert!(!child_pid.is_empty());
        let mut gone = false;
        for _ in 0..50 {
            let status = Command::new("/bin/kill")
                .args(["-0", child_pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            if !status.success() {
                gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            gone,
            "descendant process {child_pid} survived timeout cleanup"
        );
    }
}
