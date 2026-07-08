# Whisper-транскрипция (замена Vosk) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Заменить движок транскрипции Vosk на faster-whisper (large-v3 на GPU), чтобы русская речь и особенно IT-англицизмы распознавались заметно точнее.

**Architecture:** Тот же приём, что у Vosk — Rust шеллит Python-хелпер, обмен по stdin (сырой s16le@16000) / stdout (построчный JSON `{"final": …}`). Новый хелпер `scripts/whisper_stream.py` делает энергетическое эндпоинтинг-VAD, гоняет faster-whisper на CUDA, применяет два рычага под IT-термины (hotwords/initial_prompt при распознавании + пользовательский словарь пост-коррекции). Rust-сторона (`Feeder`, reader-поток, БД) почти не меняется — только пути/имена переменных указывают на whisper-стек.

**Tech Stack:** Rust (eframe/egui), Python 3.12 в отдельном venv, faster-whisper (CTranslate2), numpy, CUDA-либы pip-колёсами (nvidia-cublas-cu12, nvidia-cudnn-cu12), uv для создания venv.

## Global Constraints

- Whisper-стек ставится в **отдельный venv на Python 3.12**: `~/.local/share/health-widget/venv-whisper/` (существующий `venv/` на Python 3.14 не трогаем).
- Системного CUDA toolkit нет — CUDA-либы только pip-колёсами в venv.
- Хелпер сохраняет контракт: читает сырой mono s16le @16000 из stdin, печатает построчный JSON в stdout. Печатает **только** `{"final": <str>}` (никаких `partial`).
- Язык распознавания фиксирован: `language="ru"`.
- Модель по умолчанию: `large-v3`; устройство `cuda`; compute_type `float16`.
- `HEALTH_TRANSCRIBE=0` по-прежнему полностью выключает транскрипцию.
- Фолбэк: нет venv-python или скрипта → `Transcriber::start()` возвращает `None`, канал работает без текста.
- НЕ ломать: захват звука (`src/audio.rs`), схему БД (`transcript`/`calls`/`tracks`), CLI-флаги `--transcript`/`--calls`/`--export`, запись WAV-дорожек.
- Константа `STT_RATE = 16000.0` в `src/transcribe.rs` остаётся.
- Термин-файлы (`it_hotwords.txt`, `it_corrections.tsv`) живут в data-dir рядом с распакованным скриптом; редактируются пользователем; установщик копирует их только если отсутствуют.

## File Structure

- Create `scripts/whisper_smoke.py` — одноразовый smoke-тест GPU-инференса (Task 1).
- Create `scripts/whisper_stream.py` — новый стриминг-хелпер (Tasks 2–3).
- Create `scripts/test_whisper_stream.py` — unittest на чистые функции хелпера (Task 2).
- Create `scripts/it_hotwords.txt`, `scripts/it_corrections.tsv` — стартовые базы терминов (Task 5).
- Modify `src/transcribe.rs` — пути/переменные на whisper-стек (Task 4).
- Modify `install-transcribe.sh` — установка whisper-venv + предзагрузка модели + копирование баз (Task 5).
- Delete (в конце, опционально) `scripts/vosk_stream.py` и старый `venv/` (Task 6).

---

## Task 1: Провизия whisper-venv и доказательство GPU-инференса (блокирующий гейт)

Цель — до любых правок кода убедиться, что faster-whisper реально грузит модель и считает на RTX 5080 (Blackwell/sm_120). Если нет — чиним версию/compute здесь.

**Files:**
- Create: `scripts/whisper_smoke.py`

**Interfaces:**
- Produces: рабочий venv `~/.local/share/health-widget/venv-whisper/` с `faster-whisper`; подтверждённый факт, что `WhisperModel(..., device="cuda")` не падает и считает на GPU.

- [ ] **Step 1: Создать venv на Python 3.12 через uv**

```bash
BASE="$HOME/.local/share/health-widget"
uv venv "$BASE/venv-whisper" --python 3.12
```

Expected: создан каталог `venv-whisper` с `bin/python` (Python 3.12.x).

- [ ] **Step 2: Установить faster-whisper и CUDA-либы**

```bash
BASE="$HOME/.local/share/health-widget"
uv pip install --python "$BASE/venv-whisper/bin/python" \
  faster-whisper nvidia-cublas-cu12 nvidia-cudnn-cu12
```

Expected: установка без ошибок. Заметь версию faster-whisper (нужна ≥1.0.2 ради параметра `hotwords`).

- [ ] **Step 3: Написать smoke-скрипт**

`scripts/whisper_smoke.py`:

```python
#!/usr/bin/env python3
"""Одноразовая проверка: грузится ли faster-whisper на GPU (RTX 5080 / sm_120).

Запуск:
    venv-whisper/bin/python whisper_smoke.py [путь_к_wav]

Без аргумента прогоняет 1 секунду синтетического сигнала — доказывает, что модель
грузится и инференс идёт на CUDA (текст может быть пустым, это ок). С путём к WAV
печатает распознанный текст для глазами-проверки качества.
"""
import sys
import numpy as np
from faster_whisper import WhisperModel

def main() -> int:
    print("loading large-v3 on cuda/float16 ...", flush=True)
    model = WhisperModel("large-v3", device="cuda", compute_type="float16")
    print("model loaded OK", flush=True)

    if len(sys.argv) > 1:
        import wave
        with wave.open(sys.argv[1], "rb") as w:
            raw = w.readframes(w.getnframes())
        audio = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    else:
        audio = np.zeros(16000, dtype=np.float32)

    segments, info = model.transcribe(audio, language="ru", beam_size=5)
    text = " ".join(s.text.strip() for s in segments).strip()
    print(f"inference OK, text: {text!r}", flush=True)
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Прогнать smoke на синтетике и смотреть GPU**

```bash
BASE="$HOME/.local/share/health-widget"
"$BASE/venv-whisper/bin/python" scripts/whisper_smoke.py &
sleep 8 && nvidia-smi | grep -i python || true
wait
```

Expected: печатается `model loaded OK` и `inference OK`; в `nvidia-smi` во время загрузки виден python-процесс с занятой VRAM. **Если падает с ошибкой про sm_120 / no kernel image** — это гейт: обнови/понизь `faster-whisper`/`ctranslate2`, либо попробуй `compute_type="int8_float16"`, пока не пройдёт. Rust не трогаем.

- [ ] **Step 5: Прогнать smoke на реальной речи (если есть запись кола)**

```bash
BASE="$HOME/.local/share/health-widget"
WAV=$(ls "$BASE"/calls/*/mic.wav 2>/dev/null | head -1)
[ -n "$WAV" ] && "$BASE/venv-whisper/bin/python" scripts/whisper_smoke.py "$WAV"
```

Expected: осмысленный русский текст. (Если записей нет — пропусти, проверим e2e в Task 6.)

- [ ] **Step 6: Commit**

```bash
git add scripts/whisper_smoke.py
git commit -m "feat(transcribe): smoke-тест GPU-инференса faster-whisper"
```

---

## Task 2: Чистые функции хелпера — базы терминов (TDD)

Парсинг `it_hotwords.txt` и `it_corrections.tsv` и применение пост-коррекции — чистая логика, тестируется без GPU.

**Files:**
- Create: `scripts/whisper_stream.py` (пока только функции + импорты)
- Test: `scripts/test_whisper_stream.py`

**Interfaces:**
- Produces:
  - `parse_hotwords(lines: Iterable[str], limit: int = 80) -> str` — из строк файла делает строку `"term1, term2, ..."` (пропуская пустые и `#`-комментарии, беря первые `limit`).
  - `parse_corrections(lines: Iterable[str]) -> list[tuple[re.Pattern, str]]` — из строк `ослышка<TAB>правильно` компилирует список (регэксп по границам слова, IGNORECASE).
  - `apply_corrections(text: str, pairs: list) -> str` — применяет замены по порядку.
  - `load_hotwords(path) -> str`, `load_corrections(path) -> list` — обёртки, читают файл; при отсутствии файла возвращают `""` / `[]`.

- [ ] **Step 1: Написать падающие тесты**

`scripts/test_whisper_stream.py`:

```python
import unittest
from whisper_stream import parse_hotwords, parse_corrections, apply_corrections

class TestHotwords(unittest.TestCase):
    def test_joins_terms(self):
        self.assertEqual(parse_hotwords(["Kubernetes", "Docker"]), "Kubernetes, Docker")

    def test_skips_blank_and_comments(self):
        self.assertEqual(parse_hotwords(["Kubernetes", "", "# note", "Docker"]),
                         "Kubernetes, Docker")

    def test_respects_limit(self):
        self.assertEqual(parse_hotwords(["a", "b", "c"], limit=2), "a, b")

class TestCorrections(unittest.TestCase):
    def test_basic_replace(self):
        pairs = parse_corrections(["кубернетис\tKubernetes"])
        self.assertEqual(apply_corrections("запусти кубернетис сегодня", pairs),
                         "запусти Kubernetes сегодня")

    def test_case_insensitive(self):
        pairs = parse_corrections(["дэплой\tdeploy"])
        self.assertEqual(apply_corrections("Дэплой прошёл", pairs), "deploy прошёл")

    def test_word_boundary(self):
        # не трогаем часть более длинного слова
        pairs = parse_corrections(["код\tcode"])
        self.assertEqual(apply_corrections("кодовое слово", pairs), "кодовое слово")

    def test_multiword_phrase(self):
        pairs = parse_corrections(["пул реквест\tpull request"])
        self.assertEqual(apply_corrections("сделай пул реквест", pairs),
                         "сделай pull request")

    def test_skips_malformed_lines(self):
        pairs = parse_corrections(["нет таба", "", "# коммент", "a\tb"])
        self.assertEqual(len(pairs), 1)

if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Прогнать тесты — убедиться, что падают**

Run: `python3 -m unittest scripts.test_whisper_stream -v` (или из каталога `scripts/`: `python3 -m unittest test_whisper_stream -v`)
Expected: FAIL/ERROR — `whisper_stream` не содержит функций (ImportError).

- [ ] **Step 3: Реализовать функции в `scripts/whisper_stream.py`**

Начало файла `scripts/whisper_stream.py`:

```python
#!/usr/bin/env python3
"""Потоковое распознавание речи для health-widget через faster-whisper.

Читает СЫРОЙ моно-PCM s16le 16000 Гц из stdin (его пишет Rust-канал после ресемплинга)
и печатает построчный JSON в stdout — только законченные фразы:

    {"final": "Запусти Kubernetes"}

Endpointing по энергии (пауза => конец фразы), инференс на GPU (CUDA), два рычага под
IT-термины: hotwords/initial_prompt при распознавании + словарь пост-коррекции.
Никакой сети в рантайме (модель кэшируется локально при установке).

Аргументы:
    argv[1] — имя или путь модели faster-whisper (напр. large-v3)
Окружение:
    WHISPER_DEVICE (cuda), WHISPER_COMPUTE (float16)
Файлы баз (рядом со скриптом, опциональны):
    it_hotwords.txt, it_corrections.tsv
"""
import os
import re
import sys
import json
import time
import threading
from typing import Iterable


def parse_hotwords(lines: Iterable[str], limit: int = 80) -> str:
    terms = []
    for line in lines:
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        terms.append(s)
        if len(terms) >= limit:
            break
    return ", ".join(terms)


def parse_corrections(lines: Iterable[str]):
    pairs = []
    for line in lines:
        s = line.rstrip("\n")
        if not s or s.startswith("#") or "\t" not in s:
            continue
        wrong, right = s.split("\t", 1)
        wrong, right = wrong.strip(), right.strip()
        if not wrong or not right:
            continue
        rx = re.compile(r"(?<!\w)" + re.escape(wrong) + r"(?!\w)", re.IGNORECASE)
        pairs.append((rx, right))
    return pairs


def apply_corrections(text: str, pairs) -> str:
    for rx, right in pairs:
        text = rx.sub(right, text)
    return text


def load_hotwords(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            return parse_hotwords(f)
    except OSError:
        return ""


def load_corrections(path: str):
    try:
        with open(path, encoding="utf-8") as f:
            return parse_corrections(f)
    except OSError:
        return []
```

- [ ] **Step 4: Прогнать тесты — убедиться, что проходят**

Run (из каталога `scripts/`): `python3 -m unittest test_whisper_stream -v`
Expected: PASS (все 8 тестов).

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(transcribe): базы IT-терминов для whisper (hotwords + пост-коррекция)"
```

---

## Task 3: Стриминг-цикл хелпера (drain-поток + энергетический VAD + инференс)

**Files:**
- Modify: `scripts/whisper_stream.py` (добавить загрузку модели и главный цикл)

**Interfaces:**
- Consumes: `parse_*`/`load_*`/`apply_corrections` из Task 2; venv-whisper из Task 1.
- Produces: рабочий хелпер — на вход сырой s16@16000, на выход построчный `{"final": …}`.

- [ ] **Step 1: Написать интеграционный тест-скрипт (ручной прогон)**

Проверяем на реальной записи, скармливая её raw-PCM в stdin хелпера. WAV из колов — s16 mono @16000 (так пишет рекордер), но health-widget пишет заголовок WAV; для теста берём только PCM-данные. Команда:

```bash
BASE="$HOME/.local/share/health-widget"
PY="$BASE/venv-whisper/bin/python"
WAV=$(ls "$BASE"/calls/*/mic.wav 2>/dev/null | head -1)
# срезаем 44-байтный WAV-заголовок -> сырой s16le, в stdin хелпера
tail -c +45 "$WAV" | "$PY" scripts/whisper_stream.py large-v3
```

Expected (после Step 2): одна или несколько строк `{"final": "..."}` с осмысленным русским текстом. (Записей нет → отложить до Task 6.)

- [ ] **Step 2: Дописать главный цикл в `scripts/whisper_stream.py`**

Добавить в конец файла (после функций из Task 2):

```python
SAMPLE_RATE = 16000
FRAME = 320                 # 20 мс
FRAME_BYTES = FRAME * 2
SILENCE_RMS = 500.0         # порог RMS int16: тишина/речь
SILENCE_TAIL = 0.6          # сек тишины => конец фразы
MIN_SPEECH = 0.3            # короче — считаем шумом, не транскрибируем
MAX_SEGMENT = 15.0          # форс-флаш при непрерывной речи

_buf = bytearray()
_buf_lock = threading.Lock()
_stdin_open = True


def _drain_stdin():
    """Отдельный поток: непрерывно вычитываем stdin, чтобы Rust не упирался в
    заполненный пайп, пока грузится модель или идёт инференс."""
    global _stdin_open
    while True:
        b = sys.stdin.buffer.read(4096)
        if not b:
            _stdin_open = False
            return
        with _buf_lock:
            _buf.extend(b)


def _take(n: int) -> bytes:
    with _buf_lock:
        if not _buf:
            return b""
        m = min(n, len(_buf))
        out = bytes(_buf[:m])
        del _buf[:m]
        return out


def main() -> int:
    if len(sys.argv) < 2:
        sys.stderr.write("usage: whisper_stream.py <model>\n")
        return 2
    model_name = sys.argv[1]

    import numpy as np
    from faster_whisper import WhisperModel

    base = os.path.dirname(os.path.abspath(__file__))
    hotwords = load_hotwords(os.path.join(base, "it_hotwords.txt")) or None
    corrections = load_corrections(os.path.join(base, "it_corrections.tsv"))

    device = os.environ.get("WHISPER_DEVICE", "cuda")
    compute = os.environ.get("WHISPER_COMPUTE", "float16")

    threading.Thread(target=_drain_stdin, daemon=True).start()
    model = WhisperModel(model_name, device=device, compute_type=compute)

    def emit(raw: bytes):
        audio = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
        segments, _ = model.transcribe(
            audio, language="ru", beam_size=5, vad_filter=True,
            hotwords=hotwords, initial_prompt=hotwords,
        )
        text = " ".join(s.text.strip() for s in segments).strip()
        text = apply_corrections(text, corrections)
        if text:
            sys.stdout.write(json.dumps({"final": text}, ensure_ascii=False) + "\n")
            sys.stdout.flush()

    pending = bytearray()
    utter = bytearray()
    speaking = False
    silence_run = 0.0

    while True:
        chunk = _take(65536)
        if chunk:
            pending.extend(chunk)
        elif not _stdin_open:
            if len(utter) > MIN_SPEECH * SAMPLE_RATE * 2:
                emit(bytes(utter))
            break
        else:
            time.sleep(0.02)
            continue

        while len(pending) >= FRAME_BYTES:
            frame = bytes(pending[:FRAME_BYTES])
            del pending[:FRAME_BYTES]
            samples = np.frombuffer(frame, dtype=np.int16).astype(np.float32)
            rms = float(np.sqrt(np.mean(samples * samples)))
            if rms >= SILENCE_RMS:
                speaking = True
                utter.extend(frame)
                silence_run = 0.0
            elif speaking:
                utter.extend(frame)
                silence_run += FRAME / SAMPLE_RATE
                if silence_run >= SILENCE_TAIL:
                    if len(utter) > MIN_SPEECH * SAMPLE_RATE * 2:
                        emit(bytes(utter))
                    utter = bytearray()
                    speaking = False
                    silence_run = 0.0
            if speaking and len(utter) >= MAX_SEGMENT * SAMPLE_RATE * 2:
                emit(bytes(utter))
                utter = bytearray()
                speaking = False
                silence_run = 0.0
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 3: Убедиться, что unit-тесты Task 2 всё ещё зелёные**

Run (из `scripts/`): `python3 -m unittest test_whisper_stream -v`
Expected: PASS (импорт `main`/цикла не ломает чистые функции; `numpy`/`faster_whisper` импортируются лениво внутри `main`, поэтому тесты идут без GPU-зависимостей).

- [ ] **Step 4: Прогнать интеграционный тест из Step 1 (если есть запись)**

Expected: строки `{"final": "..."}` с осмысленным текстом; в `nvidia-smi` видно нагрузку. Если текст пустой на явно речевом WAV — подстрой `SILENCE_RMS`/`SILENCE_TAIL`.

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py
git commit -m "feat(transcribe): стриминг-цикл whisper (VAD-эндпоинтинг + GPU-инференс)"
```

---

## Task 4: Rust — переключить `transcribe.rs` на whisper-стек

**Files:**
- Modify: `src/transcribe.rs` (шапка-доккоммент; `python_path`, `model_path`→`model_spec`, `ensure_script`; вызов в `start`)

**Interfaces:**
- Consumes: `scripts/whisper_stream.py` (Task 2–3).
- Produces: `Transcriber::start` спавнит `venv-whisper/bin/python whisper_stream.py <model>`; поведение reader-потока и `Feeder` без изменений.

- [ ] **Step 1: Обновить `python_path` — новый venv и переменная**

В `src/transcribe.rs` заменить тело `python_path` (около `:187`):

```rust
/// Python из whisper-venv (`WHISPER_PYTHON` переопределяет). None — если не существует.
fn python_path() -> Option<PathBuf> {
    let p = match std::env::var_os("WHISPER_PYTHON") {
        Some(v) => PathBuf::from(v),
        None => data_dir()?.join("venv-whisper").join("bin").join("python"),
    };
    p.exists().then_some(p)
}
```

- [ ] **Step 2: Заменить `model_path` на `model_spec` (имя модели, без проверки существования)**

Whisper-модель — это имя (`large-v3`), которое faster-whisper скачивает/кэширует сам, а не каталог. Заменить `model_path` (около `:195`) на:

```rust
/// Имя/путь модели whisper (`WHISPER_MODEL` переопределяет; дефолт large-v3).
fn model_spec() -> String {
    std::env::var("WHISPER_MODEL").unwrap_or_else(|_| "large-v3".to_string())
}
```

- [ ] **Step 3: Переключить `ensure_script` на whisper-хелпер**

В `ensure_script` (около `:206`) заменить встраиваемый скрипт и имя файла:

```rust
    const SRC: &str = include_str!("../scripts/whisper_stream.py");
    let path = data_dir()?.join("whisper_stream.py");
```

(остальное тело `ensure_script` — запись `SRC` в `path` — без изменений.)

- [ ] **Step 4: Обновить вызов в `start`**

В `Transcriber::start` (около `:104-109`) заменить получение модели и передачу аргумента:

```rust
        let python = python_path()?;
        let model = model_spec();
        let script = ensure_script()?;

        let mut child = Command::new(&python)
            .arg(&script)
            .arg(&model)
```

(было `let model = model_path()?;` — `model_spec()` не возвращает Option, поэтому `?` убираем именно на этой строке. `python_path()?` и `ensure_script()?` остаются с `?` — это и есть гейт фолбэка.)

- [ ] **Step 5: Обновить шапку-доккоммент модуля**

Заменить строки 1–11 `src/transcribe.rs` на описание whisper-пайплайна:

```rust
//! Онлайн-транскрипция аудио-канала через faster-whisper — тем же приёмом, что и остальной
//! проект: не тянем библиотеку в бинарь, а шеллим Python-хелпер `whisper_stream.py` из
//! отдельного venv (`venv-whisper`, Python 3.12), где стоит `faster-whisper`.
//!
//! Поток данных: канал (`audio.rs`) декодирует f32 PCM @44100. Мы ресемплим его в s16 @16000
//! и пишем в stdin хелпера. Хелпер режет поток на фразы (VAD по энергии), гоняет модель на
//! GPU и печатает построчный JSON `{"final": …}`; фоновый поток читает stdout и копит текст.
//!
//! Partial'ов нет (Whisper выдаёт текст фразами) — под осциллографом появляется законченная
//! фраза с лагом ~1–3 c. Если venv/скрипт не найдены — `start()` возвращает None, и канал
//! работает без текста.
```

- [ ] **Step 6: Собрать проект**

Run: `cargo build`
Expected: сборка без ошибок (include_str! находит `scripts/whisper_stream.py`, созданный в Task 2–3).

- [ ] **Step 7: Commit**

```bash
git add src/transcribe.rs
git commit -m "feat(transcribe): Rust-сторона на whisper-стек (venv-whisper, whisper_stream.py)"
```

---

## Task 5: Установщик и стартовые базы терминов

**Files:**
- Create: `scripts/it_hotwords.txt`, `scripts/it_corrections.tsv`
- Modify: `install-transcribe.sh`

**Interfaces:**
- Consumes: `whisper_smoke.py` (для финальной проверки установки).
- Produces: воспроизводимая установка whisper-venv + предзагрузка large-v3 + копирование баз в data-dir.

- [ ] **Step 1: Стартовый `scripts/it_hotwords.txt`**

```
# IT-термины (топ-частотные) — в hotwords/initial_prompt whisper. Один термин на строку.
Kubernetes
Docker
deploy
backend
frontend
pull request
merge
commit
rebase
API
endpoint
frontend
Postgres
Redis
Kafka
Nginx
DevOps
CI/CD
pipeline
staging
production
rollback
namespace
container
cluster
microservice
webhook
payload
middleware
runtime
```

- [ ] **Step 2: Стартовый `scripts/it_corrections.tsv`**

```
# ослышка<TAB>правильно — регистронезависимая замена по границам слов на готовом тексте.
кубернетис	Kubernetes
кубернетес	Kubernetes
докер	Docker
дэплой	deploy
деплой	deploy
пул реквест	pull request
пулреквест	pull request
мёрдж	merge
мердж	merge
коммит	commit
ребейз	rebase
эндпоинт	endpoint
постгрес	Postgres
редис	Redis
кафка	Kafka
нгинкс	Nginx
пайплайн	pipeline
стейджинг	staging
продакшн	production
роллбэк	rollback
неймспейс	namespace
кластер	cluster
```

- [ ] **Step 3: Переписать `install-transcribe.sh`**

Заменить содержимое файла на:

```bash
#!/usr/bin/env bash
# Установка онлайн-транскрипции (faster-whisper) для health-widget.
#
# Ставит faster-whisper (модель Whisper large-v3, движок CTranslate2) в ОТДЕЛЬНЫЙ venv на
# Python 3.12 в data-каталоге виджета (~/.local/share/health-widget/venv-whisper). Инференс
# на GPU (CUDA). CUDA-либы ставятся pip-колёсами — системный CUDA toolkit не нужен.
# Идемпотентно — повторный запуск ничего не ломает.
#
# После установки запусти виджет: под каждым включённым осциллографом появится транскрипция
# фразами. Отключить — HEALTH_TRANSCRIBE=0.
set -euo pipefail

BASE="${XDG_DATA_HOME:-$HOME/.local/share}/health-widget"
VENV="$BASE/venv-whisper"
MODEL="${WHISPER_MODEL:-large-v3}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$BASE"

echo "==> venv (Python 3.12): $VENV"
if [ ! -x "$VENV/bin/python" ]; then
  uv venv "$VENV" --python 3.12
fi

echo "==> ставлю faster-whisper и CUDA-либы (колёса)"
uv pip install --python "$VENV/bin/python" \
  faster-whisper nvidia-cublas-cu12 nvidia-cudnn-cu12

echo "==> копирую хелпер и базы терминов в $BASE (базы — только если их ещё нет)"
cp "$SCRIPT_DIR/scripts/whisper_stream.py" "$BASE/whisper_stream.py"
[ -f "$BASE/it_hotwords.txt" ]    || cp "$SCRIPT_DIR/scripts/it_hotwords.txt" "$BASE/it_hotwords.txt"
[ -f "$BASE/it_corrections.tsv" ] || cp "$SCRIPT_DIR/scripts/it_corrections.tsv" "$BASE/it_corrections.tsv"

echo "==> предзагрузка модели $MODEL и проверка GPU-инференса"
"$VENV/bin/python" "$SCRIPT_DIR/scripts/whisper_smoke.py"

echo
echo "Готово. Whisper-транскрипция установлена в $BASE"
echo "Запусти виджет и включи каналы 🎤 / 🔊 — текст пойдёт под осциллографами."
echo "Переопределения: WHISPER_MODEL, WHISPER_PYTHON, WHISPER_DEVICE, WHISPER_COMPUTE; HEALTH_TRANSCRIBE=0 (выкл)."
echo "Базы IT-терминов: $BASE/it_hotwords.txt (hotwords), $BASE/it_corrections.tsv (пост-коррекция) — правь под себя."
```

- [ ] **Step 4: Прогнать установщик (идемпотентность)**

Run: `bash install-transcribe.sh`
Expected: доходит до `model loaded OK` / `inference OK` и печатает `Готово`. Повторный запуск не падает.

- [ ] **Step 5: Commit**

```bash
git add install-transcribe.sh scripts/it_hotwords.txt scripts/it_corrections.tsv
git commit -m "feat(transcribe): установщик whisper-venv + стартовые базы IT-терминов"
```

---

## Task 6: End-to-end проверка и очистка Vosk

**Files:**
- Delete (опционально): `scripts/vosk_stream.py`

**Interfaces:**
- Consumes: всё предыдущее.

- [ ] **Step 1: Запустить виджет и снять кол**

```bash
cargo run --release
```

Включи канал 🎤, нажми «Новый кол», проговори несколько фраз с IT-англицизмами («задеплоил на стейджинг», «смёржил пул реквест», «поднял кластер кубернетис»), заверши кол.

Expected: под осциллографом появляются фразы с лагом ~1–3 c; англицизмы латиницей (`deploy`, `pull request`, `Kubernetes`).

- [ ] **Step 2: Проверить, что текст лёг в БД**

Run: `cargo run --release -- --transcript-today`
Expected: строки `[ts] 🎤 я: ...` с распознанным текстом и корректными терминами.

- [ ] **Step 3: (Опционально) убрать старый Vosk**

Только после того как whisper устраивает по качеству:

```bash
git rm scripts/vosk_stream.py
rm -rf "$HOME/.local/share/health-widget/venv" \
       "$HOME/.local/share/health-widget/vosk-model-small-ru-0.22" \
       "$HOME/.local/share/health-widget/vosk_stream.py"
```

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore(transcribe): удалить Vosk после перехода на whisper"
```

---

## Self-Review

**Spec coverage:**
- Замена движка Vosk→faster-whisper — Tasks 1,3,4. ✓
- GPU/CUDA через pip-колёса, отдельный venv 3.12 — Tasks 1,5. ✓
- Хелпер с VAD-эндпоинтингом, только `{"final"}`, `language="ru"` — Task 3. ✓
- Оба рычага (hotwords/initial_prompt + пост-коррекция) — Tasks 2,3; базы — Task 5. ✓
- Rust: пути/переменные, фолбэк None, STT_RATE, partials убраны — Task 4. ✓
- Гейт проверки Blackwell до Rust — Task 1. ✓
- Конфиг-переменные (WHISPER_*), HEALTH_TRANSCRIBE=0 — Tasks 3,4,5. ✓
- Не ломаем аудио-захват/БД/CLI/WAV — подтверждается в Task 6. ✓
- Установщик переписан, стартовые базы — Task 5. ✓

**Placeholder scan:** плейсхолдеров/«TBD» нет; код приведён целиком в каждом шаге.

**Type consistency:** `parse_hotwords`/`parse_corrections`/`apply_corrections`/`load_hotwords`/`load_corrections` — имена совпадают между Task 2 и Task 3. Rust: `python_path`/`model_spec`/`ensure_script` согласованы между Task 4 шагами; `model_spec()` возвращает `String` (без `?`), `python_path()`/`ensure_script()` — `Option` (с `?`). Хелпер зовётся `whisper_stream.py` везде (Tasks 2–5). Термин-файлы `it_hotwords.txt`/`it_corrections.tsv` — единые имена в Tasks 3,5.

Отмеченный риск: поддержка sm_120 в поставляемой сборке ctranslate2 — изолирована в блокирующий гейт Task 1; при провале чинится там до правок Rust.
