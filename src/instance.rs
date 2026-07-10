use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const LOCK_WAIT: Duration = Duration::from_secs(15);
const WHISPER_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(200);

fn lock_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("health-widget.lock")
}

fn try_flock(file: &File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn send_signal(pid: i32, sig: i32) {
    if pid > 1 {
        unsafe { libc::kill(pid, sig) };
    }
}

pub fn acquire_or_replace() {
    let path = lock_path();
    let Ok(mut file) = OpenOptions::new().read(true).write(true).create(true).open(&path)
    else {
        return;
    };

    if !try_flock(&file) {
        let mut pid_text = String::new();
        let _ = file.read_to_string(&mut pid_text);
        let old_pid: Option<i32> = pid_text.trim().parse().ok();

        if let Some(pid) = old_pid {
            send_signal(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + LOCK_WAIT;
        while !try_flock(&file) {
            if Instant::now() >= deadline {
                if let Some(pid) = old_pid {
                    send_signal(pid, libc::SIGKILL);
                }
                std::thread::sleep(Duration::from_millis(500));
                if !try_flock(&file) {
                    eprintln!("health-widget: не удалось перехватить замок у PID {old_pid:?}, выходим");
                    std::process::exit(1);
                }
                break;
            }
            std::thread::sleep(POLL);
        }
        crate::telemetry::event(
            "app.replaced_old",
            serde_json::json!({ "old_pid": old_pid }),
        );
    }

    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = write!(file, "{}", std::process::id());
    std::mem::forget(file);
}

pub fn wait_whisper_gone() {
    let Some(script) = crate::transcribe::script_path() else {
        return;
    };
    let pattern = script.to_string_lossy().into_owned();
    let deadline = Instant::now() + WHISPER_WAIT;
    let mut waited = false;
    while whisper_alive(&pattern) {
        waited = true;
        if Instant::now() >= deadline {
            let _ = Command::new("pkill").args(["-9", "-f", &pattern]).status();
            std::thread::sleep(Duration::from_millis(500));
            crate::telemetry::error("stt.orphan_killed", &pattern);
            return;
        }
        std::thread::sleep(POLL);
    }
    if waited {
        crate::telemetry::event("stt.orphan_waited", serde_json::json!({}));
    }
}

fn whisper_alive(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
