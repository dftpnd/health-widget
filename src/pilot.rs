//! Управление процессом автопилота (`work-autopilot`) прямо из виджета.
//!
//! Тот же приём, что и в `audio.rs`: шеллим готовый бинарь (`autopilot run …`) и держим
//! его дочерним процессом. Фоновый поток читает stderr автопилота (loguru пишет туда) в
//! кольцо последних строк — под кнопками показываем последнюю как краткий статус.
//!
//! Автопилот работает через ОДИН persistent-профиль браузера (Chromium его лочит) и одно
//! окно; фазы (чат/отклики) не совмещаются. Поэтому живёт ровно один процесс ровно в одной
//! фазе. Две кнопки виджета взаимоисключающи: выбор фазы = (пере)запуск процесса.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Сколько последних строк лога автопилота держим для показа статуса.
const LOG_CAP: usize = 6;
/// Сколько ждём мягкого завершения (SIGTERM → закрытие браузера) перед SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Фаза автопилота — ровно одна за раз (одно окно браузера).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Чат с работодателями (`autopilot run --chat`).
    Chat,
    /// Отклики из очереди скана (`autopilot run --apply`).
    Apply,
    /// Разовый скан одной группы поиска в очередь (`autopilot run --scan --group N`).
    /// Строка — имя группы (из listings_urls конфига автопилота).
    Scan(String),
    /// Разовый скан ВСЕХ групп подряд (`autopilot run --scan` без группы).
    ScanAll,
    /// Разовое обогащение пула (`autopilot run --enrich`): открыть необогащённые
    /// вакансии, дописать полное описание + дату публикации + вектор, и выйти.
    Enrich,
}

impl Phase {
    /// Аргументы к `autopilot run …` для этой фазы.
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
    /// Фаза, в которой запущен этот процесс.
    phase: Phase,
    /// Профиль (аккаунт), под которым запущен процесс. None — базовый (fullstack).
    /// Смена профиля, как и смена фазы, требует перезапуска (другой браузер/аккаунт).
    profile: Option<String>,
    /// Порог релевантности (MIN_SIMILARITY), с которым запущен процесс. Читается один
    /// раз при старте, поэтому смена строгости на ходу требует перезапуска фазы откликов.
    min_sim: f32,
    /// Последние строки stderr автопилота (для краткого статуса под кнопками).
    log: Arc<Mutex<VecDeque<String>>>,
    /// Стоит ли процесс на паузе (SIGSTOP) — снимается `resume()`.
    paused: bool,
}

impl Pilot {
    /// Запустить `autopilot run [--profile <name>] --chat|--apply|--scan|--enrich` в каталоге
    /// `dir` (там `.env` и `config/`). `profile` выбирает аккаунт/резюме (None —
    /// базовый fullstack). `min_similarity` — порог релевантности откликов (строгость):
    /// прокидываем в окружение как MIN_SIMILARITY (Settings его читает; актуально для
    /// фазы отклика, остальные фазы игнорируют). None — если бинарь не стартовал.
    pub fn start(
        dir: &Path,
        bin: &Path,
        phase: Phase,
        profile: Option<&str>,
        min_similarity: Option<f32>,
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
        cmd.current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // Своя process-group: при жёсткой остановке добьём и осиротевший Chromium (kill -group).
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
            log,
            paused: false,
        })
    }

    /// Порог релевантности, с которым запущен процесс (для сверки при смене строгости).
    pub fn min_sim(&self) -> f32 {
        self.min_sim
    }

    /// Жив ли процесс. Завершился (сам или упал) — false, кнопки должны погаснуть.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// В какой фазе запущен процесс — чтобы решать, нужен ли перезапуск.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Под каким профилем (аккаунтом) запущен процесс. None — базовый (fullstack).
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// Последняя строка лога автопилота — краткий статус под кнопками (None — пока молчит).
    pub fn last_line(&self) -> Option<String> {
        self.log.lock().ok().and_then(|g| g.back().cloned())
    }

    /// Запрошена ли пауза.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Мягкая пауза: SIGUSR1 автопилоту. Процесс НЕ замораживается — он доделывает
    /// текущую вакансию/чат и встаёт перед следующим (браузер и профиль заняты, без
    /// перезапуска). Продолжить — `resume()`. Повторный вызов — no-op.
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

    /// Снять паузу: SIGUSR2 — автопилот продолжает со следующего шага. Повторный вызов — no-op.
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

    /// Мягко остановить: SIGTERM (Python закроет браузер, освободит профиль), подождать,
    /// при упорстве — SIGKILL по всей группе (добить осиротевший Chromium).
    fn stop(&mut self) {
        // Уже завершился — ничего не делаем.
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let pid = self.child.id();
        // Мягкая пауза (SIGUSR1) не морозит процесс — SIGTERM доставляется штатно.
        // std не умеет слать SIGTERM — шеллим kill (в идиоме проекта).
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

        // Не закрылся за отведённое время — жёстко гасим всю группу (pid == pgid).
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

/// Обновить `data/scan.json` (список групп + счётчики очереди) без браузера:
/// шеллим `autopilot scan-status`. Нужно, чтобы кнопки групп появились в виджете
/// до первого скана. Быстро (только конфиг + БД), ошибки безопасно игнорируем.
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

    // Тесты порождают процессы. Cargo гоняет их параллельно в одном процессе, и fork()
    // одного теста может унаследовать открытый на запись fd фейкового бинаря другого →
    // ETXTBSY при exec (артефакт теста, не кода). Сериализуем тела тестов этим локом.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Записать исполняемый фейковый «autopilot» с заданным телом (после shebang).
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
        // Фейк повторяет свои аргументы в stderr — так проверяем переданный флаг фазы.
        let bin = fake_bin("echo-args", "while true; do echo \"args: $*\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let mut p = Pilot::start(dir.as_path(), &bin, Phase::Chat, None, None).expect("должен стартовать");
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
        let p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, None).expect("должен стартовать");
        assert_eq!(p.phase(), &Phase::Apply);
        std::thread::sleep(Duration::from_millis(200));
        let line = p.last_line().unwrap();
        assert!(line.contains("--apply") && !line.contains("--chat"), "лог: {line}");
    }

    #[test]
    fn profile_passes_profile_flag() {
        let _serial = SERIAL.lock().unwrap();
        // Профиль пробрасывается как `--profile <name>` перед флагом фазы.
        let bin = fake_bin("echo-profile", "while true; do echo \"args: $*\" >&2; sleep 0.05; done");
        let dir = std::env::temp_dir();
        let p = Pilot::start(dir.as_path(), &bin, Phase::Apply, Some("back"), None).unwrap();
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
        let p = Pilot::start(dir.as_path(), &bin, Phase::Chat, None, None).unwrap();
        let pid = p.pid();
        assert!(alive_pid(pid), "процесс должен быть жив до drop");
        drop(p); // stop(): SIGTERM → браузер закрылся бы, профиль освобождён
        std::thread::sleep(Duration::from_millis(300));
        assert!(!alive_pid(pid), "процесс должен быть убит после drop");
    }

    #[test]
    fn pause_sends_usr1_resume_sends_usr2() {
        let _serial = SERIAL.lock().unwrap();
        // Мягкая пауза не морозит процесс: он ловит SIGUSR1/USR2 и продолжает жить.
        // Фейк ловит сигналы трапами и печатает метку в stderr — проверяем доставку.
        let bin = fake_bin(
            "signal-trap",
            "trap 'echo GOT-USR1 >&2' USR1; trap 'echo GOT-USR2 >&2' USR2; \
             while true; do sleep 0.05; done",
        );
        let dir = std::env::temp_dir();
        let mut p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, None).unwrap();
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
        // Автопилот завершился сам → alive() должен вернуть false (кнопки погаснут).
        let bin = fake_bin("quickexit", "exit 0");
        let dir = std::env::temp_dir();
        let mut p = Pilot::start(dir.as_path(), &bin, Phase::Apply, None, None).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!p.alive(), "завершившийся процесс не должен считаться живым");
    }
}
