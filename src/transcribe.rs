//! Онлайн-транскрипция аудио-канала через Vosk — тем же приёмом, что и остальной проект:
//! не тянем dev-библиотеку (libvosk) в бинарь, а шеллим готовый инструмент. Здесь это
//! маленький Python-хелпер `vosk_stream.py` из venv, куда поставлен пакет `vosk`.
//!
//! Поток данных: канал (`audio.rs`) уже декодирует f32 PCM @44100. Мы ресемплим его в
//! s16 @16000 (частота, на которой обучены small-модели Vosk) и пишем в stdin хелпера.
//! Фоновый поток читает stdout хелпера — построчный JSON с `partial`/`final` — и копит
//! текст в общее состояние, которое UI показывает под осциллографом.
//!
//! Если venv/модель/скрипт не найдены — `start()` возвращает None, и канал просто работает
//! без текста (как `AudioMonitor::start`, когда нет `pw-record`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Частота, которую ждёт Vosk small-модель.
const STT_RATE: f64 = 16000.0;
/// Сколько последних символов финального текста держим (бегущая строка под осциллографом).
const MAX_FINALS: usize = 280;

/// Разделяемое между потоком-читателем и UI состояние транскрипции одного канала.
#[derive(Default)]
struct Transcript {
    /// Накопленный распознанный текст (обрезается с головы до MAX_FINALS).
    finals: String,
    /// Текущая незавершённая гипотеза.
    partial: String,
}

/// Транскрайбер канала: держит дочерний python-процесс и читающий поток.
/// UI дёргает `text()`; аудио-поток кормит сэмплами через `Feeder`.
pub struct Transcriber {
    state: Arc<Mutex<Transcript>>,
    child: Child,
}

/// Кормящая половина: живёт в аудио-потоке (`audio.rs`), пишет ресемплённый PCM в хелпер.
/// Хранит состояние ресемплера между батчами, чтобы стык окон был бесшовным.
pub struct Feeder {
    stdin: ChildStdin,
    /// src_rate / STT_RATE — на сколько исходных сэмплов сдвигаемся за один выходной.
    ratio: f64,
    /// Дробная позиция чтения внутри текущего батча (может стартовать с [-1,0) — см. `prev`).
    pos: f64,
    /// Последний сэмпл прошлого батча — виртуальный индекс -1 для интерполяции через границу.
    prev: f32,
    /// Переиспользуемый буфер выходных s16-байт (без аллокаций на батч).
    out: Vec<u8>,
    /// Пайп сломан (хелпер умер) — перестаём писать.
    dead: bool,
}

impl Feeder {
    /// Скормить батч исходных f32-сэмплов: ресемплим в s16 @16000 и пишем в stdin хелпера.
    pub fn feed(&mut self, s: &[f32]) {
        if self.dead || s.is_empty() {
            return;
        }
        self.out.clear();
        let len = s.len() as f64;
        // Виртуальный массив: индекс -1 == prev, 0.. == s. Идём с шагом ratio, линейно
        // интерполируя, пока следующая точка (i+1) ещё внутри батча.
        while self.pos < len - 1.0 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let ii = i as isize;
            let a = if ii < 0 { self.prev } else { s[ii as usize] };
            let b = s[(ii + 1) as usize];
            let v = (a + (b - a) * frac).clamp(-1.0, 1.0);
            let q = (v * 32767.0) as i16;
            self.out.extend_from_slice(&q.to_le_bytes());
            self.pos += self.ratio;
        }
        // Сдвигаем начало координат на следующий батч: его индекс 0 == текущий индекс len.
        self.pos -= len;
        self.prev = s[s.len() - 1];

        if !self.out.is_empty() && self.stdin.write_all(&self.out).is_err() {
            self.dead = true; // EPIPE — хелпер закрылся
        }
    }
}

impl Transcriber {
    /// Запустить хелпер для канала с исходной частотой `src_rate`.
    /// None — если транскрипция выключена или окружение (python/скрипт/модель) не готово.
    /// При успехе возвращает читающую половину (для UI) и кормящую (в аудио-поток).
    pub fn start(src_rate: u32) -> Option<(Transcriber, Feeder)> {
        if std::env::var("HEALTH_TRANSCRIBE").as_deref() == Ok("0") {
            return None;
        }
        let python = python_path()?;
        let model = model_path()?;
        let script = ensure_script()?;

        let mut child = Command::new(&python)
            .arg(&script)
            .arg(&model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let state = Arc::new(Mutex::new(Transcript::default()));

        // Поток-читатель: построчный JSON из хелпера → общее состояние.
        {
            let state = state.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(_) => break, // stdout закрылся — хелпер умер
                    };
                    let v: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let Ok(mut g) = state.lock() else { break };
                    if let Some(p) = v.get("partial").and_then(|p| p.as_str()) {
                        g.partial = p.to_string();
                    } else if let Some(t) = v.get("final").and_then(|t| t.as_str()) {
                        if !g.finals.is_empty() {
                            g.finals.push(' ');
                        }
                        g.finals.push_str(t);
                        trim_head(&mut g.finals, MAX_FINALS);
                        g.partial.clear();
                    }
                }
            });
        }

        let feeder = Feeder {
            stdin,
            ratio: src_rate as f64 / STT_RATE,
            pos: 0.0,
            prev: 0.0,
            out: Vec::with_capacity(4096),
            dead: false,
        };
        Some((Transcriber { state, child }, feeder))
    }

    /// Текст для показа под осциллографом: (накопленный финальный, текущая гипотеза).
    pub fn text(&self) -> (String, String) {
        match self.state.lock() {
            Ok(g) => (g.finals.clone(), g.partial.clone()),
            Err(_) => (String::new(), String::new()),
        }
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Каталог данных виджета (`~/.local/share/health-widget`) — сюда ставятся venv и модель.
fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget"))
}

/// Python из venv (`VOSK_PYTHON` переопределяет). None — если не существует.
fn python_path() -> Option<PathBuf> {
    let p = match std::env::var_os("VOSK_PYTHON") {
        Some(v) => PathBuf::from(v),
        None => data_dir()?.join("venv/bin/python"),
    };
    p.exists().then_some(p)
}

/// Каталог модели Vosk (`VOSK_MODEL` переопределяет). None — если не существует.
fn model_path() -> Option<PathBuf> {
    let p = match std::env::var_os("VOSK_MODEL") {
        Some(v) => PathBuf::from(v),
        None => data_dir()?.join("vosk-model-small-ru-0.22"),
    };
    p.is_dir().then_some(p)
}

/// Скрипт-хелпер зашит в бинарь и распаковывается в data-dir при первом запуске,
/// чтобы не зависеть от рабочего каталога/расположения репозитория.
fn ensure_script() -> Option<PathBuf> {
    const SRC: &str = include_str!("../scripts/vosk_stream.py");
    let path = data_dir()?.join("vosk_stream.py");
    // Перезаписываем, только если содержимое отличается (обновление бинаря).
    let need_write = std::fs::read_to_string(&path).map(|c| c != SRC).unwrap_or(true);
    if need_write {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, SRC).ok()?;
    }
    Some(path)
}

/// Обрезать строку с головы до `max` символов по границе слова (для бегущей строки).
fn trim_head(s: &mut String, max: usize) {
    let n = s.chars().count();
    if n <= max {
        return;
    }
    let skip = n - max;
    // Индекс байта после skip символов…
    let mut cut = s.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(0);
    // …и дальше до ближайшего пробела, чтобы не рвать слово.
    if let Some(rel) = s[cut..].find(' ') {
        cut += rel + 1;
    }
    s.replace_range(..cut, "");
}
