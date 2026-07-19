# Стриминговая транскрипция (LocalAgreement-2) — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Переписать `scripts/whisper_stream.py` со схемы «декод по тишине» на скользящий буфер с коммитом по согласию двух декодов: `partial` через ~1 с, `final` течёт во время речи.

**Architecture:** Главный цикл копит s16le-аудио в скользящий буфер; каждые ≥1 с нового аудио буфер декодируется целиком (`word_timestamps=True`); совпавший префикс слов двух последовательных декодов коммитится и уходит финалами по границам предложений; хвост печатается как `{"partial": ...}`. Пауза ≥0.4 с — флаш с полным доверием; для коротких изолированных фраз при флаше остаётся второй декод с пертурбацией. Спека: `docs/superpowers/specs/2026-07-19-streaming-stt-design.md`.

**Tech Stack:** Python 3 (stdlib + numpy + faster-whisper), unittest. Rust не трогаем.

## Global Constraints

- Никаких комментариев и docstring'ов в новом коде (CLAUDE.md); правится только существующий модульный docstring, и только по содержанию.
- Ветка `master`, новых веток не создавать.
- Коммиты на русском в стиле проекта (`feat(stt): …`), в конце сообщения строка `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Тесты гоняются без модели и GPU: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`.
- Файлы затрагиваются только эти: `scripts/whisper_stream.py`, `scripts/test_whisper_stream.py` (+ пересборка Rust-бинаря в Task 7 без правок кода).

---

### Task 1: норма слова и согласный префикс

**Files:**
- Modify: `scripts/whisper_stream.py` (после блока констант `SAMPLE_RATE…`)
- Test: `scripts/test_whisper_stream.py`

**Interfaces:**
- Produces: `norm_word(w: str) -> str`; `common_prefix(a: list[tuple[str, float, float]], b: list[...]) -> int` — длина позиционно совпавшего префикса по нормализованной форме слова. Слово везде в плане — кортеж `(text, start_sec, end_sec)`.

- [ ] **Step 1: Написать падающие тесты**

В `scripts/test_whisper_stream.py` дописать импорт и классы:

```python
from whisper_stream import norm_word, common_prefix
```

```python
class TestNormWord(unittest.TestCase):
    def test_lower_and_strip_punct(self):
        self.assertEqual(norm_word("Привет,"), "привет")
        self.assertEqual(norm_word("мир."), "мир")

    def test_only_punct_is_empty(self):
        self.assertEqual(norm_word("—"), "")

class TestCommonPrefix(unittest.TestCase):
    def test_full_match_ignores_case_and_punct(self):
        a = [("Привет,", 0.0, 0.4), ("мир", 0.5, 0.9)]
        b = [("привет", 0.0, 0.4), ("мир.", 0.5, 0.9)]
        self.assertEqual(common_prefix(a, b), 2)

    def test_divergence_stops_prefix(self):
        a = [("раз", 0.0, 0.3), ("два", 0.4, 0.7), ("три", 0.8, 1.1)]
        b = [("раз", 0.0, 0.3), ("двадцать", 0.4, 0.7), ("три", 0.8, 1.1)]
        self.assertEqual(common_prefix(a, b), 1)

    def test_empty_side(self):
        self.assertEqual(common_prefix([], [("а", 0.0, 0.3)]), 0)
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: ImportError `cannot import name 'norm_word'`

- [ ] **Step 3: Реализация**

В `whisper_stream.py` после констант:

```python
def norm_word(w: str) -> str:
    return re.sub(r"[\W_]+", "", w.lower())

def common_prefix(a, b) -> int:
    n = 0
    m = min(len(a), len(b))
    while n < m and norm_word(a[n][0]) == norm_word(b[n][0]):
        n += 1
    return n
```

- [ ] **Step 4: Тесты зелёные**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: OK, все тесты проходят

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(stt): нормализация слов и согласный префикс двух гипотез"
```

---

### Task 2: продвижение коммита (advance)

**Files:**
- Modify: `scripts/whisper_stream.py` (после `common_prefix`)
- Test: `scripts/test_whisper_stream.py`

**Interfaces:**
- Consumes: `common_prefix` из Task 1.
- Produces: `advance(prev_words, committed: int, cur_words) -> tuple[int, list[str], str]` — новый счётчик закоммиченных слов, список свежезакоммиченных текстов слов, текст незакоммиченного хвоста.

- [ ] **Step 1: Написать падающие тесты**

Дописать импорт `advance` и класс:

```python
class TestAdvance(unittest.TestCase):
    def test_agreed_prefix_commits(self):
        prev = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0)]
        cur = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0), ("завтра", 1.1, 1.5)]
        committed, newly, partial = advance(prev, 0, cur)
        self.assertEqual(committed, 2)
        self.assertEqual(newly, ["запусти", "кластер"])
        self.assertEqual(partial, "завтра")

    def test_already_committed_not_repeated(self):
        prev = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0), ("завтра", 1.1, 1.5)]
        cur = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0), ("завтра", 1.1, 1.5)]
        committed, newly, partial = advance(prev, 2, cur)
        self.assertEqual(committed, 3)
        self.assertEqual(newly, ["завтра"])
        self.assertEqual(partial, "")

    def test_shifted_hypothesis_commits_nothing(self):
        prev = [("шум", 0.0, 0.5)]
        cur = [("совсем", 0.0, 0.4), ("другое", 0.5, 0.9)]
        committed, newly, partial = advance(prev, 0, cur)
        self.assertEqual(committed, 0)
        self.assertEqual(newly, [])
        self.assertEqual(partial, "совсем другое")

    def test_rewrite_before_committed_ignored(self):
        prev = [("раз", 0.0, 0.3), ("два", 0.4, 0.7)]
        cur = [("уже", 0.0, 0.3), ("другой", 0.4, 0.7), ("текст", 0.8, 1.1)]
        committed, newly, partial = advance(prev, 2, cur)
        self.assertEqual(committed, 2)
        self.assertEqual(newly, [])
        self.assertEqual(partial, "текст")
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: ImportError `cannot import name 'advance'`

- [ ] **Step 3: Реализация**

```python
def advance(prev_words, committed, cur_words):
    n = common_prefix(prev_words, cur_words)
    newly = [w for w, _s, _e in cur_words[committed:n]]
    committed = max(committed, n)
    partial = " ".join(w for w, _s, _e in cur_words[committed:])
    return committed, newly, partial
```

- [ ] **Step 4: Тесты зелёные**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: OK

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(stt): коммит слов по согласию двух последовательных декодов"
```

---

### Task 3: нарезка финалов (take_final)

**Files:**
- Modify: `scripts/whisper_stream.py` (после `advance`; константы — в блок констант)
- Test: `scripts/test_whisper_stream.py`

**Interfaces:**
- Produces: константы `SENT_END = ".?!…"`, `FINAL_MAX_WORDS = 30`; `take_final(pending: list[str], limit: int = FINAL_MAX_WORDS) -> tuple[str, list[str]]` — (текст к отправке или "", остаток слов).

- [ ] **Step 1: Написать падающие тесты**

Дописать импорт `take_final` и класс:

```python
class TestTakeFinal(unittest.TestCase):
    def test_no_boundary_keeps_pending(self):
        out, rest = take_final(["привет", "мир"])
        self.assertEqual(out, "")
        self.assertEqual(rest, ["привет", "мир"])

    def test_cuts_at_last_sentence_end(self):
        out, rest = take_final(["Привет.", "Как", "дела?", "Я"])
        self.assertEqual(out, "Привет. Как дела?")
        self.assertEqual(rest, ["Я"])

    def test_ellipsis_is_boundary(self):
        out, rest = take_final(["ну…", "и"])
        self.assertEqual(out, "ну…")
        self.assertEqual(rest, ["и"])

    def test_word_limit_flushes_all(self):
        words = ["слово"] * 30
        out, rest = take_final(words)
        self.assertEqual(out, " ".join(words))
        self.assertEqual(rest, [])

    def test_empty_pending(self):
        out, rest = take_final([])
        self.assertEqual(out, "")
        self.assertEqual(rest, [])
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: ImportError `cannot import name 'take_final'`

- [ ] **Step 3: Реализация**

В блок констант:

```python
SENT_END = ".?!…"
FINAL_MAX_WORDS = 30
```

После `advance`:

```python
def take_final(pending, limit=FINAL_MAX_WORDS):
    last = -1
    for i, w in enumerate(pending):
        if w and w[-1] in SENT_END:
            last = i
    if last >= 0:
        return " ".join(pending[:last + 1]), pending[last + 1:]
    if len(pending) >= limit:
        return " ".join(pending), []
    return "", pending
```

- [ ] **Step 4: Тесты зелёные**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: OK

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(stt): нарезка закоммиченных слов на финалы по границам предложений"
```

---

### Task 4: разбор сегментов в слова (flatten_words)

**Files:**
- Modify: `scripts/whisper_stream.py` (после `is_hallucination`)
- Test: `scripts/test_whisper_stream.py`

**Interfaces:**
- Consumes: `is_hallucination` (существующая).
- Produces: `flatten_words(segments) -> list[tuple[str, float, float]]` — сегменты faster-whisper (атрибуты `.text`, `.words[].word/.start/.end`, метрики) в плоский список слов; сегменты-галлюцинации выпадают целиком.

- [ ] **Step 1: Написать падающие тесты**

Дописать в импорты тестов `flatten_words` и:

```python
from types import SimpleNamespace as NS

def seg(text, words, **kw):
    return NS(
        text=text,
        words=[NS(word=w, start=s, end=e) for w, s, e in words],
        no_speech_prob=kw.get("no_speech_prob", 0.0),
        avg_logprob=kw.get("avg_logprob", 0.0),
        compression_ratio=kw.get("compression_ratio", 1.0),
    )

class TestFlattenWords(unittest.TestCase):
    def test_strips_and_flattens(self):
        segs = [seg("привет мир", [(" привет", 0.0, 0.4), (" мир", 0.5, 0.9)])]
        self.assertEqual(flatten_words(segs),
                         [("привет", 0.0, 0.4), ("мир", 0.5, 0.9)])

    def test_hallucinated_segment_dropped_entirely(self):
        segs = [
            seg("Продолжение следует...",
                [(" Продолжение", 0.0, 0.5), (" следует...", 0.6, 1.0)]),
            seg("реальная речь", [(" реальная", 1.2, 1.6), (" речь", 1.7, 2.0)]),
        ]
        self.assertEqual([w for w, _s, _e in flatten_words(segs)],
                         ["реальная", "речь"])

    def test_bad_metrics_dropped(self):
        segs = [seg("любой текст", [(" любой", 0.0, 0.4)],
                    no_speech_prob=0.9, avg_logprob=-0.8)]
        self.assertEqual(flatten_words(segs), [])

    def test_segment_without_words_skipped(self):
        segs = [NS(text="пусто", words=None, no_speech_prob=0.0,
                   avg_logprob=0.0, compression_ratio=1.0)]
        self.assertEqual(flatten_words(segs), [])
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: ImportError `cannot import name 'flatten_words'`

- [ ] **Step 3: Реализация**

```python
def flatten_words(segments):
    out = []
    for s in segments:
        if is_hallucination(
            s.text,
            getattr(s, "no_speech_prob", 0.0),
            getattr(s, "avg_logprob", 0.0),
            getattr(s, "compression_ratio", 0.0),
        ):
            continue
        for w in (s.words or []):
            out.append((w.word.strip(), w.start, w.end))
    return out
```

- [ ] **Step 4: Тесты зелёные**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: OK

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(stt): разворачивание сегментов в слова с фильтром галлюцинаций"
```

---

### Task 5: арифметика обрезки буфера (cut_bytes)

**Files:**
- Modify: `scripts/whisper_stream.py` (после `flatten_words`; константа — в блок констант)
- Test: `scripts/test_whisper_stream.py`

**Interfaces:**
- Produces: константа `MAX_BUFFER = 12.0`; `cut_bytes(buf_len: int, committed_end: float | None, max_seconds: float) -> int` — сколько байт срезать с начала буфера (0 = не резать). `committed_end` — конец последнего закоммиченного слова в секундах от начала буфера, `None` = коммитов нет.

- [ ] **Step 1: Написать падающие тесты**

Дописать импорт `cut_bytes, SAMPLE_RATE` и:

```python
class TestCutBytes(unittest.TestCase):
    def test_under_limit_no_cut(self):
        self.assertEqual(cut_bytes(10 * SAMPLE_RATE * 2, 5.0, 12.0), 0)

    def test_cut_at_committed_word_end(self):
        self.assertEqual(cut_bytes(13 * SAMPLE_RATE * 2, 6.5, 12.0),
                         int(6.5 * SAMPLE_RATE) * 2)

    def test_no_commits_keeps_tail(self):
        self.assertEqual(cut_bytes(15 * SAMPLE_RATE * 2, None, 12.0),
                         3 * SAMPLE_RATE * 2)

    def test_committed_end_beyond_buffer_clamped(self):
        self.assertEqual(cut_bytes(13 * SAMPLE_RATE * 2, 99.0, 12.0),
                         13 * SAMPLE_RATE * 2)
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: ImportError `cannot import name 'cut_bytes'`

- [ ] **Step 3: Реализация**

В блок констант:

```python
MAX_BUFFER = 12.0
```

После `flatten_words`:

```python
def cut_bytes(buf_len, committed_end, max_seconds):
    limit = int(max_seconds * SAMPLE_RATE) * 2
    if buf_len <= limit:
        return 0
    if committed_end is None:
        return buf_len - limit
    return min(buf_len, int(committed_end * SAMPLE_RATE) * 2)
```

- [ ] **Step 4: Тесты зелёные**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: OK

- [ ] **Step 5: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(stt): расчёт обрезки скользящего буфера"
```

---

### Task 6: перепись главного цикла на стриминг

**Files:**
- Modify: `scripts/whisper_stream.py` — модульный docstring, блок констант, `main()`; удаление `_match_form`, `texts_agree`, `MAX_SEGMENT`, `STABILITY_MIN_SIMILARITY`
- Modify: `scripts/test_whisper_stream.py` — удалить `TestTextsAgree` и импорт `texts_agree`

**Interfaces:**
- Consumes: `norm_word`, `common_prefix`, `advance`, `take_final`, `flatten_words`, `cut_bytes`, `SENT_END`, `FINAL_MAX_WORDS`, `MAX_BUFFER` (Tasks 1–5); существующие `is_hallucination`, `apply_corrections`, `load_hotwords`, `load_corrections`, `_drain_stdin`, `_take`, `_ensure_cuda_libpath`.
- Produces: рабочий стриминговый `main()`; протокол stdout: `{"partial": "..."}` после каждого декода (включая закоммиченное-но-неотправленное), `{"final": "..."}` по границам предложений/лимиту слов/флашу.

- [ ] **Step 1: Обновить модульный docstring**

Заменить в docstring абзац

```
и печатает построчный JSON в stdout — только законченные фразы:

    {"final": "Запусти Kubernetes"}

Endpointing по энергии (пауза => конец фразы), инференс на GPU (CUDA), два рычага под
IT-термины: hotwords при распознавании + словарь пост-коррекции.
Короткие сегменты декодируются дважды (второй раз со сдвигом входа): на шуме декод
нестабилен и такие сегменты отбрасываются, на речи — совпадает.
```

на

```
и печатает построчный JSON в stdout — черновик текущей фразы и законченные куски:

    {"partial": "Запусти Kuber"}
    {"final": "Запусти Kubernetes."}

Стриминг по LocalAgreement-2: скользящий буфер декодируется каждые ~1 с нового аудио,
совпавший префикс слов двух последовательных декодов коммитится в финал, хвост уходит
черновиком. Пауза ≥0.4 с — флаш; короткая изолированная фраза при флаше декодируется
дважды (второй раз со сдвигом входа): на шуме декод нестабилен и не коммитится.
Инференс на GPU (CUDA), два рычага под IT-термины: hotwords + словарь пост-коррекции.
```

- [ ] **Step 2: Обновить константы**

Было:

```python
SILENCE_TAIL = 0.6
MIN_SPEECH = 0.3
MAX_SEGMENT = 15.0
STABILITY_MAX_SECONDS = 3.0
STABILITY_MIN_SIMILARITY = 0.7
```

Стало:

```python
SILENCE_TAIL = 0.4
MIN_SPEECH = 0.3
MIN_NEW_AUDIO = 1.0
STABILITY_MAX_SECONDS = 3.0
```

(`MAX_SEGMENT` и `STABILITY_MIN_SIMILARITY` удалить; `PERTURB_PAD_SECONDS`, `PERTURB_GAIN` остаются.)

- [ ] **Step 3: Удалить мёртвый код**

Удалить целиком функции `_match_form` и `texts_agree` из `whisper_stream.py`. В `test_whisper_stream.py` удалить класс `TestTextsAgree` и `texts_agree` из импорта.

- [ ] **Step 4: Переписать main()**

Заменить в `main()` всё после `model = WhisperModel(...)` (функции `transcribe_text`, `perturb`, `emit` и цикл endpointing'а) на:

```python
    def to_float(raw: bytes):
        return np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0

    def perturbed(audio_f):
        pad = np.zeros(int(PERTURB_PAD_SECONDS * SAMPLE_RATE), dtype=np.float32)
        return np.concatenate([pad, audio_f * PERTURB_GAIN])

    def decode(audio_f):
        segments, _ = model.transcribe(
            audio_f, language="ru", beam_size=5, vad_filter=True,
            condition_on_previous_text=False, hotwords=hotwords,
            word_timestamps=True,
        )
        return flatten_words(list(segments))

    def emit_final(text: str):
        text = apply_corrections(text.strip(), corrections)
        if text:
            sys.stdout.write(json.dumps({"final": text}, ensure_ascii=False) + "\n")
            sys.stdout.flush()

    def emit_partial(text: str):
        sys.stdout.write(json.dumps({"partial": text}, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    audio = bytearray()
    pending_bytes = bytearray()
    prev_words = []
    committed = 0
    final_words = []
    speaking = False
    silence_run = 0.0
    speech_secs = 0.0
    since_decode = 0
    min_decode_bytes = int(MIN_NEW_AUDIO * SAMPLE_RATE) * 2

    def stream_step():
        nonlocal prev_words, committed, final_words, since_decode
        cur = decode(to_float(bytes(audio)))
        committed, newly, partial = advance(prev_words, committed, cur)
        prev_words = cur
        final_words.extend(newly)
        out, final_words = take_final(final_words)
        if out:
            emit_final(out)
        emit_partial(" ".join(final_words + ([partial] if partial else [])))
        since_decode = 0
        end = cur[committed - 1][2] if 0 < committed <= len(cur) else None
        cut = cut_bytes(len(audio), end, MAX_BUFFER)
        if cut:
            del audio[:cut]
            prev_words = []
            committed = 0

    def flush():
        nonlocal prev_words, committed, final_words, speaking
        nonlocal silence_run, speech_secs, since_decode
        if audio and (committed or final_words or speech_secs >= MIN_SPEECH):
            f = to_float(bytes(audio))
            cur = decode(f)
            if (
                not committed
                and not final_words
                and len(audio) <= STABILITY_MAX_SECONDS * SAMPLE_RATE * 2
            ):
                alt = decode(perturbed(f))
                cur = cur[: common_prefix(cur, alt)]
            tail = [w for w, _s, _e in cur[committed:]]
            emit_final(" ".join(final_words + tail))
        audio.clear()
        prev_words = []
        committed = 0
        final_words = []
        speaking = False
        silence_run = 0.0
        speech_secs = 0.0
        since_decode = 0
        emit_partial("")

    while True:
        chunk = _take(65536)
        if chunk:
            pending_bytes.extend(chunk)
        elif not _stdin_open:
            flush()
            break
        else:
            time.sleep(0.02)
            continue

        while len(pending_bytes) >= FRAME_BYTES:
            frame = bytes(pending_bytes[:FRAME_BYTES])
            del pending_bytes[:FRAME_BYTES]
            samples = np.frombuffer(frame, dtype=np.int16).astype(np.float32)
            rms = float(np.sqrt(np.mean(samples * samples)))
            if rms >= SILENCE_RMS:
                speaking = True
                silence_run = 0.0
                speech_secs += FRAME / SAMPLE_RATE
            elif speaking:
                silence_run += FRAME / SAMPLE_RATE
            if speaking:
                audio.extend(frame)
                since_decode += FRAME_BYTES
                if silence_run >= SILENCE_TAIL:
                    flush()

        if speaking and since_decode >= min_decode_bytes:
            stream_step()
    return 0
```

- [ ] **Step 5: Полный прогон тестов**

Run: `cd /home/mgu/projects/health-widget/scripts && python3 -m unittest test_whisper_stream -v`
Expected: OK, ни одного упоминания `texts_agree`

- [ ] **Step 6: Синтаксическая проверка скрипта**

Run: `python3 -c "import ast, pathlib; ast.parse(pathlib.Path('/home/mgu/projects/health-widget/scripts/whisper_stream.py').read_text())" && echo OK`
Expected: `OK`

- [ ] **Step 7: Commit**

```bash
git add scripts/whisper_stream.py scripts/test_whisper_stream.py
git commit -m "feat(stt): стриминговый декод с LocalAgreement-2 вместо ожидания тишины"
```

---

### Task 7: живой прогон и пересборка виджета

**Files:**
- Никаких правок кода; пересборка бинаря (скрипт вшит через `include_str!` в `src/transcribe.rs::ensure_script`).

**Interfaces:**
- Consumes: готовый `scripts/whisper_stream.py` из Task 6.

- [ ] **Step 1: Смоук на реальном аудио**

Найти WAV от прошлых звонков и прогнать в реальном темпе (`-re`):

```bash
wav=$(find ~/.local/share/health-widget -name "*.wav" -printf "%T@ %p\n" 2>/dev/null | sort -rn | head -1 | cut -d" " -f2-)
echo "$wav"
ffmpeg -v error -re -i "$wav" -f s16le -ar 16000 -ac 1 - \
  | timeout 60 ~/.local/share/health-widget/venv-whisper/bin/python \
      /home/mgu/projects/health-widget/scripts/whisper_stream.py large-v3
```

Expected: строки `{"partial": ...}` начинают идти через ~2–3 с после начала речи (плюс время загрузки модели), `{"final": ...}` появляются по ходу монолога, не только после пауз. Если WAV не нашёлся — записать голос с микрофона: `pw-record --rate 16000 --channels 1 --format s16 - | timeout 30 ~/.local/share/health-widget/venv-whisper/bin/python /home/mgu/projects/health-widget/scripts/whisper_stream.py large-v3` (говорить в микрофон; этот вариант требует присутствия пользователя — тогда просто попросить его).

- [ ] **Step 2: Пересборка виджета**

Run: `cd /home/mgu/projects/health-widget && cargo build --release 2>&1 | tail -3`
Expected: сборка без ошибок. Новый скрипт попадёт в `~/.local/share/health-widget/whisper_stream.py` при следующем старте транскрайбера.

- [ ] **Step 3: Сообщить пользователю**

Перезапуск виджета — руками пользователя, как обычно (setsid из шелла; НЕ через `systemd-run --collect`). Напомнить об этом в итоговом сообщении.

---

## Self-Review (выполнен)

- Покрытие спеки: §1 темп/гейт — Task 6 (цикл); §2 декод — Task 6 `decode` + Task 4 фильтр; §3 коммит — Tasks 1–2; §4 вывод — Task 3 + `stream_step`; §5 флаш и пертурбация коротких фраз — Task 6 `flush`; §6 обрезка — Task 5 + `stream_step`; EOF — ветка `not _stdin_open`; тесты — в каждой задаче; удаления — Task 6 Step 2–3.
- Плейсхолдеров нет; весь код приведён.
- Сигнатуры сквозные: `advance(prev, committed, cur) -> (int, list[str], str)`, `take_final(pending, limit) -> (str, list[str])`, `cut_bytes(len, end|None, max) -> int` — совпадают между задачами.
