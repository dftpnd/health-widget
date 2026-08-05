use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LOG_CAP: usize = 40;
const STOP_GRACE: Duration = Duration::from_secs(3);

struct Log {
    lines: VecDeque<String>,
    total: u64,
}

pub struct WinAgent {
    child: Child,
    stdin: Option<ChildStdin>,
    log: Arc<Mutex<Log>>,
    linked: Arc<AtomicBool>,
    peer: Arc<Mutex<String>>,
    busy: Arc<AtomicBool>,
}

impl WinAgent {
    pub fn start(dir: &Path, python: &Path) -> Option<Self> {
        let mut cmd = Command::new(python);
        cmd.args(["-u", "server/orchestrator.py"])
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.process_group(0);

        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take();

        let log = Arc::new(Mutex::new(Log {
            lines: VecDeque::with_capacity(LOG_CAP),
            total: 0,
        }));
        let linked = Arc::new(AtomicBool::new(false));
        let peer = Arc::new(Mutex::new(String::new()));
        let busy = Arc::new(AtomicBool::new(false));

        if let Some(out) = child.stdout.take() {
            pump(out, log.clone(), linked.clone(), peer.clone(), busy.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, log.clone(), linked.clone(), peer.clone(), busy.clone());
        }

        Some(Self { child, stdin, log, linked, peer, busy })
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn linked(&self) -> bool {
        self.linked.load(Ordering::Relaxed)
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    pub fn peer(&self) -> String {
        self.peer.lock().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn send(&mut self, task: &str) -> bool {
        let task = task.trim();
        if task.is_empty() || !self.linked() {
            return false;
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        if writeln!(stdin, "{task}").is_err() || stdin.flush().is_err() {
            return false;
        }
        self.busy.store(true, Ordering::Relaxed);
        push(&self.log, format!("▸ {task}"));
        true
    }

    pub fn since(&self, cursor: u64) -> (Vec<String>, u64) {
        let Ok(g) = self.log.lock() else {
            return (Vec::new(), cursor);
        };
        let dropped = g.total - g.lines.len() as u64;
        let from = cursor.max(dropped);
        let fresh = g
            .lines
            .iter()
            .skip((from - dropped) as usize)
            .cloned()
            .collect();
        (fresh, g.total)
    }

    fn stop(&mut self) {
        self.stdin.take();
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let pid = self.child.id();
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();

        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = Command::new("kill").args(["-KILL", &format!("-{pid}")]).status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for WinAgent {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pump(
    stream: impl std::io::Read + Send + 'static,
    log: Arc<Mutex<Log>>,
    linked: Arc<AtomicBool>,
    peer: Arc<Mutex<String>>,
    busy: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            let text = tidy(&line);
            if text.is_empty() {
                continue;
            }
            if let Some(host) = text.strip_suffix(" на связи").map(str::to_string) {
                linked.store(true, Ordering::Relaxed);
                if let Ok(mut g) = peer.lock() {
                    *g = host;
                }
            } else if text.contains("на связи, инструментов") {
                linked.store(true, Ordering::Relaxed);
                if let Ok(mut g) = peer.lock() {
                    *g = text.split(' ').next().unwrap_or_default().to_string();
                }
            } else if text.ends_with("отключился") {
                linked.store(false, Ordering::Relaxed);
                busy.store(false, Ordering::Relaxed);
            }
            if !text.starts_with('[') && !text.starts_with("  ") {
                busy.store(false, Ordering::Relaxed);
            }
            push(&log, text);
        }
        linked.store(false, Ordering::Relaxed);
        busy.store(false, Ordering::Relaxed);
    });
}

fn push(log: &Arc<Mutex<Log>>, line: String) {
    if let Ok(mut g) = log.lock() {
        if g.lines.len() >= LOG_CAP {
            g.lines.pop_front();
        }
        g.lines.push_back(line);
        g.total += 1;
    }
}

fn tidy(line: &str) -> String {
    let t = line.trim_end();
    let t = t.strip_prefix("> ").unwrap_or(t);
    t.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn fake_python(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("fake-winagent-{name}"));
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn captures_log_and_detects_link() {
        let bin = fake_python("hello", "echo 'fake-win на связи, инструментов: 13'; sleep 5");
        let mut a = WinAgent::start(&std::env::temp_dir(), &bin).expect("должен стартовать");
        assert!(wait_for(|| a.linked()), "должен увидеть подключение ноута");
        assert_eq!(a.peer(), "fake-win");
        let (lines, _) = a.since(0);
        assert!(lines.iter().any(|l| l.contains("инструментов")));
        assert!(a.alive());
    }

    #[test]
    fn task_goes_to_stdin_only_when_linked() {
        let bin = fake_python("echo-stdin", "echo 'fake-win на связи, инструментов: 3'; while read l; do echo \"got:$l\"; done");
        let mut a = WinAgent::start(&std::env::temp_dir(), &bin).expect("должен стартовать");
        assert!(wait_for(|| a.linked()));
        assert!(a.send("открой блокнот"));
        assert!(wait_for(|| a.since(0).0.iter().any(|l| l.contains("got:открой блокнот"))));
    }

    #[test]
    fn task_rejected_without_link() {
        let bin = fake_python("silent", "sleep 5");
        let mut a = WinAgent::start(&std::env::temp_dir(), &bin).expect("должен стартовать");
        assert!(!a.send("что-нибудь"), "без ноута задача не должна уходить");
    }
}
