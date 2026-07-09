
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LOG_CAP: usize = 6;
const STOP_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Phase {
    Chat,
    Apply,
    Scan(String),
    ScanAll,
    Enrich,
}

impl Phase {
    fn args(&self) -> Vec<String> {
        match self {
            Phase::Chat => vec!["--chat".into()],
            Phase::Apply => vec!["--apply".into()],
            Phase::Scan(group) => vec!["--scan".into(), "--group".into(), group.clone()],
            Phase::ScanAll => vec!["--scan".into()],
            Phase::Enrich => vec!["--enrich".into()],
        }
    }
}

pub struct Pilot {
    child: Child,
    phase: Phase,
    profile: Option<String>,
    min_sim: f32,
    apply_fresh: bool,
    log: Arc<Mutex<VecDeque<String>>>,
    paused: bool,
}

impl Pilot {
    pub fn start(
        dir: &Path,
        bin: &Path,
        phase: Phase,
        profile: Option<&str>,
        min_similarity: Option<f32>,
        apply_fresh: bool,
    ) -> Option<Self> {
        let mut cmd = Command::new(bin);
        cmd.arg("run");
        if let Some(p) = profile {
            cmd.args(["--profile", p]);
        }
        cmd.args(phase.args());
        if let Some(sim) = min_similarity {
            cmd.env("MIN_SIMILARITY", format!("{sim}"));
        }
        if apply_fresh {
            cmd.env("APPLY_ORDER", "fresh");
        }
        cmd.current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        cmd.process_group(0);

        let mut child = cmd.spawn().ok()?;

        let log = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP)));
        if let Some(stderr) = child.stderr.take() {
            let sink = log.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut g) = sink.lock() {
                        if g.len() >= LOG_CAP {
                            g.pop_front();
                        }
                        g.push_back(line);
                    }
                }
            });
        }

        Some(Self {
            child,
            phase,
            profile: profile.map(str::to_string),
            min_sim: min_similarity.unwrap_or(0.0),
            apply_fresh,
            log,
            paused: false,
        })
    }

    pub fn min_sim(&self) -> f32 {
        self.min_sim
    }

    pub fn apply_fresh(&self) -> bool {
        self.apply_fresh
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    pub fn last_line(&self) -> Option<String> {
        self.log.lock().ok().and_then(|g| g.back().cloned())
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        let pid = self.child.id();
        let _ = Command::new("kill")
            .args(["-USR1", &pid.to_string()])
            .status();
        self.paused = true;
    }

    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        let pid = self.child.id();
        let _ = Command::new("kill")
            .args(["-USR2", &pid.to_string()])
            .status();
        self.paused = false;
    }

    fn stop(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let pid = self.child.id();
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();

        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Pilot {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn refresh_scan_status(dir: &Path, bin: &Path, profile: &str) {
    let _ = Command::new(bin)
        .arg("scan-status")
        .args(["--profile", profile])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
impl Pilot {
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    static SERIAL: Mutex<()> = Mutex::new(());

    fn fake_bin(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("fake-autopilot-{name}"));
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn alive_pid(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn chat_passes_chat_flag_and_captures_log() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("echo-args", "while true; do echo \"args: $*\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let mut p = Pilot::start(dir.as_path(), &bin, Phase::Chat, None, None, false).expect("должен стартовать");
        assert_eq!(p.phase(), &Phase::Chat);
        std::thread::sleep(Duration::from_millis(200));
        assert!(p.alive(), "процесс должен быть жив");
        let line = p.last_line().expect("должна быть строка лога");
        assert!(line.contains("run"), "лог: {line}");
        assert!(line.contains("--chat"), "флаг фазы не передан: {line}");
        assert!(!line.contains("--apply"), "лишний флаг: {line}");
    }

    #[test]
    fn apply_passes_apply_flag() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("echo-apply", "while true; do echo \"args: $*\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, None, false).expect("должен стартовать");
        assert_eq!(p.phase(), &Phase::Apply);
        std::thread::sleep(Duration::from_millis(200));
        let line = p.last_line().unwrap();
        assert!(line.contains("--apply") && !line.contains("--chat"), "лог: {line}");
    }

    #[test]
    fn apply_fresh_sets_order_env() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("echo-order", "while true; do echo \"order: $APPLY_ORDER\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, Some(0.0), true).unwrap();
        assert!(p.apply_fresh());
        std::thread::sleep(Duration::from_millis(200));
        let line = p.last_line().unwrap();
        assert!(line.contains("order: fresh"), "APPLY_ORDER не передан: {line}");
    }

    #[test]
    fn no_fresh_leaves_order_env_empty() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("echo-order-off", "while true; do echo \"order: $APPLY_ORDER\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, Some(0.5), false).unwrap();
        assert!(!p.apply_fresh());
        std::thread::sleep(Duration::from_millis(200));
        let line = p.last_line().unwrap();
        assert!(line.contains("order:") && !line.contains("fresh"), "лишний APPLY_ORDER: {line}");
    }

    #[test]
    fn profile_passes_profile_flag() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("echo-profile", "while true; do echo \"args: $*\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let p = Pilot::start(dir.as_path(), &bin, Phase::Apply, Some("back"), None, false).unwrap();
        assert_eq!(p.profile(), Some("back"));
        std::thread::sleep(Duration::from_millis(200));
        let line = p.last_line().unwrap();
        assert!(line.contains("--profile back"), "флаг профиля не передан: {line}");
        assert!(line.contains("--apply"), "флаг фазы потерян: {line}");
    }

    #[test]
    fn drop_stops_process() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("longsleep", "sleep 60");
        let dir = std::env::temp_dir();
        let p = Pilot::start(dir.as_path(), &bin, Phase::Chat, None, None, false).unwrap();
        let pid = p.pid();
        assert!(alive_pid(pid), "процесс должен быть жив до drop");
        drop(p);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!alive_pid(pid), "процесс должен быть убит после drop");
    }

    #[test]
    fn pause_sends_usr1_resume_sends_usr2() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin(
            "signal-trap",
            "trap 'echo GOT-USR1 >&2' USR1; trap 'echo GOT-USR2 >&2' USR2; \
             while true; do sleep 0.05; done",
        );
        let dir = std::env::temp_dir();
        let mut p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, None, false).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(!p.is_paused());

        p.pause();
        assert!(p.is_paused(), "флаг паузы должен взвестись");
        std::thread::sleep(Duration::from_millis(150));
        assert!(p.alive(), "мягкая пауза НЕ должна убивать/морозить процесс");
        assert_eq!(p.last_line().as_deref(), Some("GOT-USR1"), "SIGUSR1 не доставлен");

        p.resume();
        assert!(!p.is_paused());
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(p.last_line().as_deref(), Some("GOT-USR2"), "SIGUSR2 не доставлен");
        assert!(p.alive(), "после resume процесс должен быть жив");
    }

    #[test]
    fn alive_false_after_self_exit() {
        let _serial = SERIAL.lock().unwrap();
        let bin = fake_bin("quickexit", "exit 0");
        let dir = std::env::temp_dir();
        let mut p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, None, false).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!p.alive(), "завершившийся процесс не должен считаться живым");
    }
}
