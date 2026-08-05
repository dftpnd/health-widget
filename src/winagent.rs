use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LOG_CAP: usize = 40;
const DONE: &str = "— готово —";
const STOP_GRACE: Duration = Duration::from_secs(3);
const SETTLE: Duration = Duration::from_millis(600);
const ARGS: [&str; 2] = ["-u", "server/orchestrator.py"];

struct Log {
    lines: VecDeque<String>,
    total: u64,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Alert {
    pub id: String,
    pub text: String,
    pub when: String,
}

pub fn parse_alert(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix("⚠ проверка ")?;
    let (id, text) = rest.split_once(':')?;
    let id = id.trim();
    let text = text.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) || text.is_empty() {
        return None;
    }
    Some((id.to_string(), text.to_string()))
}

#[derive(Clone)]
struct Watch {
    log: Arc<Mutex<Log>>,
    linked: Arc<AtomicBool>,
    peer: Arc<Mutex<String>>,
    busy: Arc<AtomicBool>,
    alerts: Arc<Mutex<Vec<Alert>>>,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            log: Arc::new(Mutex::new(Log {
                lines: VecDeque::with_capacity(LOG_CAP),
                total: 0,
            })),
            linked: Arc::new(AtomicBool::new(false)),
            peer: Arc::new(Mutex::new(String::new())),
            busy: Arc::new(AtomicBool::new(false)),
            alerts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

pub struct WinAgent {
    child: Child,
    stdin: Option<ChildStdin>,
    watch: Watch,
}

impl WinAgent {
    pub fn start(dir: &Path, python: &Path) -> Result<Self, String> {
        kill_strays(python);

        let mut cmd = Command::new(python);
        cmd.args(ARGS)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|e| format!("не запускается: {e}"))?;
        let stdin = child.stdin.take();

        let watch = Watch::default();
        if let Some(out) = child.stdout.take() {
            pump(out, watch.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, watch.clone());
        }

        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                std::thread::sleep(Duration::from_millis(50));
                return Err(last_words(&watch.log));
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Ok(Self { child, stdin, watch })
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn linked(&self) -> bool {
        self.watch.linked.load(Ordering::Relaxed)
    }

    pub fn busy(&self) -> bool {
        self.watch.busy.load(Ordering::Relaxed)
    }

    pub fn peer(&self) -> String {
        self.watch.peer.lock().map(|p| p.clone()).unwrap_or_default()
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
        self.watch.busy.store(true, Ordering::Relaxed);
        push(&self.watch.log, format!("▸ {task}"));
        true
    }

    pub fn stop_task(&mut self) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        if writeln!(stdin, "/stop").is_err() || stdin.flush().is_err() {
            return false;
        }
        push(&self.watch.log, "▪ стоп".into());
        true
    }

    pub fn take_alerts(&self) -> Vec<Alert> {
        match self.watch.alerts.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(_) => Vec::new(),
        }
    }

    pub fn since(&self, cursor: u64) -> (Vec<String>, u64) {
        let Ok(g) = self.watch.log.lock() else {
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

fn pump(stream: impl std::io::Read + Send + 'static, watch: Watch) {
    let Watch { log, linked, peer, busy, alerts } = watch;
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
            if text.contains(DONE) {
                busy.store(false, Ordering::Relaxed);
                continue;
            }
            if let Some((id, what)) = parse_alert(&text) {
                if let Ok(mut g) = alerts.lock() {
                    g.push(Alert { id, text: what, when: stamp() });
                }
            }
            push(&log, text);
        }
        linked.store(false, Ordering::Relaxed);
        busy.store(false, Ordering::Relaxed);
    });
}

pub fn kill_strays(python: &Path) {
    let pattern = format!("{} {}", python.display(), ARGS.join(" "));
    let killed = Command::new("pkill")
        .args(["-TERM", "-f", &pattern])
        .status()
        .is_ok_and(|s| s.success());
    if killed {
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn last_words(log: &Arc<Mutex<Log>>) -> String {
    let Ok(g) = log.lock() else {
        return "оркестратор сразу умер".into();
    };
    let tail: Vec<&str> = g
        .lines
        .iter()
        .rev()
        .take(2)
        .map(String::as_str)
        .filter(|l| !l.is_empty())
        .collect();
    if tail.is_empty() {
        return "оркестратор сразу умер".into();
    }
    tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
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

fn stamp() -> String {
    Command::new("date")
        .arg("+%H:%M")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
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
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir()
            .join(format!("fake-winagent-{name}-{}-{stamp}", std::process::id()));
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn spawn_retry(bin: &Path) -> Child {
        for _ in 0..20 {
            let spawned = Command::new(bin)
                .args(ARGS)
                .current_dir(std::env::temp_dir())
                .spawn();
            if let Ok(child) = spawned {
                return child;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("фейковый оркестратор должен стартовать");
    }

    fn start_retry(dir: &Path, bin: &Path) -> WinAgent {
        for _ in 0..20 {
            if let Ok(a) = WinAgent::start(dir, bin) {
                return a;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("должен стартовать");
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
        let mut a = start_retry(&std::env::temp_dir(), &bin);
        assert!(wait_for(|| a.linked()), "должен увидеть подключение ноута");
        assert_eq!(a.peer(), "fake-win");
        let (lines, _) = a.since(0);
        assert!(lines.iter().any(|l| l.contains("инструментов")));
        assert!(a.alive());
    }

    #[test]
    fn task_goes_to_stdin_only_when_linked() {
        let bin = fake_python("echo-stdin", "echo 'fake-win на связи, инструментов: 3'; while read l; do echo \"got:$l\"; done");
        let mut a = start_retry(&std::env::temp_dir(), &bin);
        assert!(wait_for(|| a.linked()));
        assert!(a.send("открой блокнот"));
        assert!(wait_for(|| a.since(0).0.iter().any(|l| l.contains("got:открой блокнот"))));
    }

    #[test]
    fn done_marker_clears_busy() {
        let bin = fake_python(
            "done",
            "echo 'fake-win на связи, инструментов: 3'; while read l; do echo \"[1] uia.tree\"; echo '— готово —'; done",
        );
        let mut a = start_retry(&std::env::temp_dir(), &bin);
        assert!(wait_for(|| a.linked()));
        assert!(a.send("задача"));
        assert!(a.busy(), "сразу после отправки агент занят");
        assert!(wait_for(|| !a.busy()), "маркер конца должен снять «занят»");
        let (lines, _) = a.since(0);
        assert!(!lines.iter().any(|l| l.contains("готово")), "маркер не показываем в ленте");
    }

    #[test]
    fn alert_line_becomes_notification() {
        let (id, text) = parse_alert("⚠ проверка a1b2c3: файл не сохранился, окно «Ошибка»")
            .expect("строка проверки разбирается");
        assert_eq!(id, "a1b2c3");
        assert_eq!(text, "файл не сохранился, окно «Ошибка»");

        assert!(parse_alert("✓ проверка a1b2c3: чисто").is_none(), "чистый итог не уведомление");
        assert!(parse_alert("  [1/25] uia.tree {}").is_none(), "обычный шаг не уведомление");
        assert!(parse_alert("⚠ проверка a1b2c3:").is_none(), "без текста уведомления нет");
    }

    #[test]
    fn alerts_collected_from_stream_once() {
        let bin = fake_python(
            "alerts",
            "echo 'fake-win на связи, инструментов: 3'; \
             echo '⚠ проверка ff01aa: не то окно'; echo '✓ проверка ff02bb: чисто'; sleep 5",
        );
        let a = start_retry(&std::env::temp_dir(), &bin);
        let mut got = Vec::new();
        assert!(wait_for(|| {
            got = a.take_alerts();
            !got.is_empty()
        }));
        assert_eq!(got.len(), 1, "в список идут только проблемы");
        assert_eq!(got[0].id, "ff01aa");
        assert!(a.take_alerts().is_empty(), "повторно то же уведомление не выдаётся");
    }

    #[test]
    fn stop_reaches_orchestrator_while_busy() {
        let bin = fake_python(
            "stop",
            "echo 'fake-win на связи, инструментов: 3'; while read l; do echo \"got:$l\"; done",
        );
        let mut a = start_retry(&std::env::temp_dir(), &bin);
        assert!(wait_for(|| a.linked()));
        assert!(a.send("долгая задача"));
        assert!(a.busy());
        assert!(a.stop_task(), "стоп уходит и когда агент занят");
        assert!(wait_for(|| a.since(0).0.iter().any(|l| l.contains("got:/stop"))));
    }

    #[test]
    fn dead_on_arrival_reports_error_instead_of_agent() {
        let bin = fake_python("bind-fail", "echo 'OSError: address already in use' >&2; exit 1");
        let err = WinAgent::start(&std::env::temp_dir(), &bin).err();
        assert!(err.is_some(), "мгновенно умерший оркестратор — не агент");
        assert!(
            err.unwrap().contains("address already in use"),
            "ошибка должна доехать до виджета"
        );
    }

    #[test]
    fn start_kills_stray_orchestrator() {
        let bin = fake_python("stray", "sleep 30");
        let mut stray = spawn_retry(&bin);

        let _agent = start_retry(&std::env::temp_dir(), &bin);

        assert!(
            wait_for(|| matches!(stray.try_wait(), Ok(Some(_)))),
            "старый оркестратор должен быть убит перед стартом"
        );
    }

    #[test]
    fn task_rejected_without_link() {
        let bin = fake_python("silent", "sleep 5");
        let mut a = start_retry(&std::env::temp_dir(), &bin);
        assert!(!a.send("что-нибудь"), "без ноута задача не должна уходить");
    }
}
