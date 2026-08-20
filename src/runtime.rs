use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt as WindowsCommandExt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunResult {
    Success,
    Failed,
    Cancelled,
}

pub fn prepare_cancellable_command(cmd: &mut Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

pub fn terminate_child_tree(child: &mut Child) {
    let pid = child.id();

    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }

    let _ = child.kill();
}

fn read_stream_lines<R: std::io::Read>(reader: R, on_line: Arc<dyn Fn(String) + Send + Sync>) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                on_line(line);
            }
            Err(_) => break,
        }
    }
}

pub fn run_command_with_cancel(
    mut cmd: Command,
    stop: Arc<AtomicBool>,
    on_line: Arc<dyn Fn(String) + Send + Sync>,
) -> std::io::Result<RunResult> {
    prepare_cancellable_command(&mut cmd);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = cmd.spawn()?;

    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let on_line = Arc::clone(&on_line);
        readers.push(std::thread::spawn(move || {
            read_stream_lines(stdout, on_line)
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let on_line = Arc::clone(&on_line);
        readers.push(std::thread::spawn(move || {
            read_stream_lines(stderr, on_line)
        }));
    }

    let mut cancelled = false;
    let status = loop {
        if stop.load(Ordering::SeqCst) {
            cancelled = true;
            terminate_child_tree(&mut child);
            break child.wait();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => break Err(e),
        }
    };

    for reader in readers {
        let _ = reader.join();
    }

    if cancelled {
        Ok(RunResult::Cancelled)
    } else if status.map(|s| s.success()).unwrap_or(false) {
        Ok(RunResult::Success)
    } else {
        Ok(RunResult::Failed)
    }
}
