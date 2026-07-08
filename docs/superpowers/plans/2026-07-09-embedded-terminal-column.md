# Embedded Terminal Column Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Заменить содержимое боковой колонки виджета на настоящий встроенный терминал системы, где пользователь сам запускает `claude` и любые команды.

**Architecture:** Новый модуль `terminal.rs` инкапсулирует терминал на базе крейта `egui_term` (egui-виджет поверх `alacritty_terminal` + `portable-pty`): PTY, VT-парсинг, рендер сетки, ввод и ресайз даёт крейт. `main.rs` рендерит терминал в существующей `SidePanel` вместо `draw_chat`. `chat.rs` остаётся в дереве нетронутым. Кнопка «Скрин» кладёт в буфер путь к PNG вместо картинки.

**Tech Stack:** Rust 2021, eframe/egui 0.31, `egui_term` (или фолбэк `portable-pty` + `alacritty_terminal` 0.24), wl-copy.

## Global Constraints

- Rust edition 2021; eframe и egui зафиксированы на `0.31` (см. `Cargo.toml`). **Любой крейт терминала обязан быть совместим с egui 0.31** — иначе типы egui двух версий конфликтуют и eframe-интеграция не соберётся.
- Идиома проекта: инструменты через `Command`/`wl-copy`, а не новые библиотеки, где это дёшево. Новый крейт добавляется только под терминал.
- Комментарии и UI-строки — на русском, в тон существующему коду.
- Проверка — сборкой и запуском реального виджета (`cargo run`), а не только юнит-тестами: деливерабл интерактивный. Unit-тест пишем только там, где есть чистая функция.
- Частые коммиты: один коммит на задачу.

---

### Task 1: Валидация и выбор стека терминала (спайк → рабочий минимум)

Первым делом выяснить, ложится ли `egui_term` на egui 0.31. Это блокирующее решение: результат задачи — либо рабочий минимальный терминал на `egui_term`, либо зафиксированное решение о фолбэке.

**Files:**
- Modify: `Cargo.toml` (секция `[dependencies]`)
- Create (временный спайк): `src/bin/term_spike.rs`

**Interfaces:**
- Produces: подтверждённое имя+версия крейта терминала в `Cargo.toml`; знание публичного API (имя виджета, тип бэкенда, конструктор) из примеров крейта — используется в Task 2.

- [ ] **Step 1: Узнать, на какой версии egui собран egui_term**

Run:
```bash
cargo add egui_term --dry-run 2>&1 | head -40
```
Затем сверить требуемую версию egui у крейта:
```bash
cargo search egui_term
```
Открыть `https://github.com/Harzu/egui_term` → `Cargo.toml`, посмотреть строку `egui = "0.xx"`.

Ожидаемо: если egui_term требует egui 0.31 (или диапазон, включающий 0.31) — идём на нём (Step 2). Если требует другую мажорную/минорную (например 0.27/0.29) — переходим к Step 5 (фолбэк-решение).

- [ ] **Step 2: Добавить egui_term и собрать спайк-бинарь**

В `Cargo.toml` в `[dependencies]` добавить (версию подставить фактическую из Step 1):
```toml
egui_term = "0.2"
```

Создать `src/bin/term_spike.rs` по образцу примера `examples/full_screen.rs` из репозитория egui_term соответствующей версии (скопировать пример как есть, подогнав под eframe 0.31). Цель — не финальный код, а проверка сборки и рендера.

- [ ] **Step 3: Собрать и запустить спайк**

Run:
```bash
cargo build --bin term_spike 2>&1 | tail -30
```
Ожидаемо: PASS (сборка без ошибок версий egui). Если в ошибках `expected egui::...::X, found egui::...::X` с разными версиями — это конфликт версий egui, крейт несовместим → Step 5.

Затем:
```bash
cargo run --bin term_spike
```
Ожидаемо: открывается окно с рабочим shell — виден prompt, ввод работает, `ls`/стрелки/`claude`/`vim` работают, Ctrl-C работает.

- [ ] **Step 4: Зафиксировать API и удалить спайк**

Записать для Task 2 (из примера, который завёлся): точное имя виджета (напр. `TerminalView`), тип бэкенда (напр. `TerminalBackend`), сигнатуру конструктора бэкенда, и как в примере хранятся/обрабатываются события PTY. Затем удалить спайк:
```bash
rm src/bin/term_spike.rs
```
Перейти к Step 6 (коммит).

- [ ] **Step 5: Фолбэк — если egui_term несовместим с egui 0.31**

Не собирать эмулятор с нуля наугад. Порядок фолбэка:
1. Проверить `egui-terminal` (Quinntyx) — `cargo add egui-terminal --dry-run` и его требуемую версию egui. Если совместим с 0.31 — использовать его (повторить Step 2–4 с ним).
2. Если и он несовместим — использовать ручную связку: `portable-pty` + `alacritty_terminal = "0.24"` + свой рендер сетки в egui `Painter`. В этом случае **остановиться и сообщить пользователю**, что объём Task 2 существенно вырастает (рендер, ввод, ресайз пишутся вручную), и дождаться подтверждения перед продолжением — это меняет размер работы, зафиксированный в спеке.

Записать выбранный стек и версии в `Cargo.toml`, убрать неиспользуемые зависимости.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build(terminal): зафиксировать крейт встроенного терминала (egui_term)"
```

---

### Task 2: Модуль `terminal.rs` — обёртка терминала для колонки

Тонкая обёртка над крейтом из Task 1: держит состояние терминала, лениво поднимает shell при первом показе, рисует виджет в переданный `ui`, переживает выход shell.

**Files:**
- Create: `src/terminal.rs`
- Modify: `src/main.rs` (объявление модуля — рядом с прочими `mod`)

**Interfaces:**
- Consumes: публичный API крейта, зафиксированный в Task 1 (имена типов/конструктора).
- Produces:
  - `pub struct Terminal` — состояние терминала (владеет бэкендом крейта).
  - `pub fn Terminal::new(ctx: &egui::Context) -> Self` — поднять shell (`$SHELL`, иначе `/usr/bin/zsh`), cwd = `$HOME`.
  - `pub fn Terminal::ui(&mut self, ui: &mut egui::Ui)` — отрисовать терминал на всю доступную площадь `ui`, обработать ввод/фокус, перезапустить shell при выходе.

- [ ] **Step 1: Объявить модуль в main.rs**

В `src/main.rs` рядом с существующими `mod chat;` / `mod screenshot;` добавить:
```rust
mod terminal;
```

- [ ] **Step 2: Написать `terminal.rs` (шапка модуля + структура)**

Создать `src/terminal.rs`. Реализовать по образцу примера egui_term, зафиксированного в Task 1. Ниже — целевая форма; **точные имена методов виджета/бэкенда взять из примера крейта Task 1** (это единственное место, зависящее от версии крейта — вся остальная логика ниже точна):

```rust
//! Встроенный терминал в боковой колонке виджета. Тонкая обёртка над egui_term:
//! PTY, VT-парсинг, рендер сетки, ввод и ресайз даёт крейт. Держим один shell на
//! время жизни колонки; при выходе shell — поднимаем заново при следующей отрисовке.

use egui_term::{TerminalBackend, TerminalView, PtyEvent};

/// Состояние терминала колонки. Владеет бэкендом (PTY + VT-модель крейта).
pub struct Terminal {
    backend: TerminalBackend,
    // Канал событий PTY от бэкенда (крейт присылает сюда «shell вышел» и пр.).
    pty_rx: std::sync::mpsc::Receiver<(u64, PtyEvent)>,
    id: u64,
}

impl Terminal {
    /// Поднять shell в PTY. cwd = $HOME; shell из $SHELL, иначе /usr/bin/zsh.
    pub fn new(ctx: &egui::Context) -> Self {
        let (pty_tx, pty_rx) = std::sync::mpsc::channel();
        let id = 0;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/usr/bin/zsh".into());
        let backend = TerminalBackend::new(
            id,
            ctx.clone(),
            pty_tx,
            egui_term::BackendSettings { shell, ..Default::default() },
        )
        .expect("не удалось поднять PTY");
        Self { backend, pty_rx, id }
    }
}
```

> Примечание: имена `TerminalBackend::new`, `BackendSettings`, `TerminalView`, `PtyEvent` и точная сигнатура конструктора **должны совпасть с примером egui_term версии из Task 1**. Если поле называется иначе (напр. `working_directory` вместо cwd-через-shell) — использовать имя из примера. Логика (один backend, канал событий, ленивое владение из main) не меняется.

- [ ] **Step 3: Реализовать отрисовку и перезапуск (`ui`)**

Дописать в `impl Terminal`:
```rust
    /// Отрисовать терминал на всю площадь `ui`. Обработать события PTY (выход shell
    /// → перезапуск), отрисовать виджет крейта (он сам обрабатывает ввод и ресайз).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        // Разгрести события PTY: если shell вышел — пересоздать бэкенд.
        let mut shell_exited = false;
        while let Ok((_id, ev)) = self.pty_rx.try_recv() {
            if matches!(ev, PtyEvent::Exit) {
                shell_exited = true;
            }
        }
        if shell_exited {
            let ctx = ui.ctx().clone();
            *self = Terminal::new(&ctx);
        }
        // Виджет крейта: рендер сетки + ввод + ресайз под размер ui.
        TerminalView::new(ui, &mut self.backend)
            .set_focus(true)
            .show();
    }
```

> `PtyEvent::Exit`, `TerminalView::new(...).set_focus(...).show()` — сверить с примером Task 1. Если событие выхода называется иначе — подставить его вариант; форма (перезапуск через `Terminal::new`) остаётся.

- [ ] **Step 4: Собрать модуль**

Run:
```bash
cargo build 2>&1 | tail -30
```
Ожидаемо: PASS. Если ошибки об именах API крейта — привести к именам из примера Task 1.

- [ ] **Step 5: Commit**

```bash
git add src/terminal.rs src/main.rs
git commit -m "feat(terminal): модуль terminal.rs — обёртка встроенного терминала"
```

---

### Task 3: Показать терминал в колонке вместо чата

Заменить вызов `draw_chat` в боковой панели на отрисовку терминала. `draw_chat`/`chat.rs` не трогаем (остаются в дереве, просто не вызываются из панели). Терминал создаём лениво при первом показе.

**Files:**
- Modify: `src/main.rs` — `struct App` (~строка 101), инициализация App (~строка 469), блок `SidePanel::right("chat_panel")` (~строки 975–985), лейбл тумблера 💬 (~строка 1032)

**Interfaces:**
- Consumes: `terminal::Terminal::new`, `terminal::Terminal::ui` (Task 2).
- Produces: поле `App.terminal: Option<terminal::Terminal>` — лениво инициализируемый терминал.

- [ ] **Step 1: Добавить поле в `struct App`**

В `src/main.rs` в `struct App` (рядом с `chat: chat::ChatState,`) добавить:
```rust
    /// Встроенный терминал колонки. None до первого открытия колонки (ленивый старт).
    terminal: Option<terminal::Terminal>,
```

- [ ] **Step 2: Инициализировать поле при создании App**

В конструкторе App (рядом с `chat: chat::ChatState::default(),`, ~строка 473) добавить:
```rust
            terminal: None,
```

- [ ] **Step 3: Рендерить терминал в боковой панели**

Заменить тело `SidePanel::right("chat_panel")` — вызов `self.draw_chat(ui, ctx);` (строка 981) на ленивую инициализацию и отрисовку терминала:
```rust
                    .show(ctx, |ui| {
                        let term = self
                            .terminal
                            .get_or_insert_with(|| terminal::Terminal::new(ctx));
                        term.ui(ui);
                    });
```

- [ ] **Step 4: Переименовать подсказку тумблера**

Строка ~1032: заменить лейбл/подсказку тумблера колонки с чата на терминал:
```rust
                        if ui
                            .selectable_label(self.chat_open, "🖥")
                            .on_hover_text("Терминал")
                            .clicked()
```

- [ ] **Step 5: Собрать и убрать предупреждения о неиспользуемом `draw_chat`**

Run:
```bash
cargo build 2>&1 | tail -30
```
Ожидаемо: PASS. `draw_chat` и `chat` теперь не вызываются из панели — компилятор выдаст `warning: method draw_chat is never used` и т.п. Это ожидаемо (chat.rs оставляем намеренно). Чтобы предупреждения не мешали, добавить над `fn draw_chat` (строка ~735) атрибут:
```rust
    #[allow(dead_code)] // чат оставлен в дереве под будущий рефакторинг
    fn draw_chat(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
```
Если компилятор укажет на неиспользуемое поле `chat` или методы `ChatState` — добавить `#[allow(dead_code)]` точечно на них по указанию компилятора.

- [ ] **Step 6: Запустить и проверить поведение**

Run:
```bash
cargo run
```
Проверить: тумблер 🖥 открывает колонку; в ней рабочий shell; ввод/вывод, стрелки, `claude`, `vim`, Ctrl-C работают; ресайз колонки (перетягивание границы) корректно меняет размер терминала без артефактов; выход из shell (`exit`) → терминал поднимается заново.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat(terminal): рендер терминала в колонке вместо чата (chat.rs оставлен)"
```

---

### Task 4: «Скрин» кладёт в буфер путь к PNG

Сейчас `screenshot::grab` после сохранения копирует саму картинку в буфер (`wl-copy -t image/png`). Для терминала нужен путь: заменить копирование картинки на копирование строки пути.

**Files:**
- Modify: `src/screenshot.rs` — функция `grab`, блок сохранения (~строки 107–124)

**Interfaces:**
- Consumes: ничего нового.
- Produces: после успешного снимка в буфере обмена лежит абсолютный путь к PNG (текст), а не image/png.

- [ ] **Step 1: Заменить копирование картинки на копирование пути**

В `src/screenshot.rs`, в `grab`, в ветке успешного `img.save(&path)` (`Ok(_) => { ... }`) заменить блок, копирующий файл как `image/png` через `wl-copy -t image/png`, на копирование строки пути:
```rust
            Ok(_) => {
                // Кладём в буфер ПУТЬ к PNG (а не саму картинку): его удобно вставить
                // во встроенный терминал и скормить, напр., `claude ... < <путь>`.
                // wl-copy демонизируется и держит содержимое сам — просто spawn со stdin.
                let p = path.display().to_string();
                if let Ok(mut child) = std::process::Command::new("wl-copy")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    use std::io::Write;
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(p.as_bytes());
                    }
                }
                finish(&status, &ctx, ShotStatus::Saved(path.display().to_string()))
            }
```

- [ ] **Step 2: Собрать**

Run:
```bash
cargo build 2>&1 | tail -20
```
Ожидаемо: PASS.

- [ ] **Step 3: Запустить и проверить**

Run:
```bash
cargo run
```
Нажать «📸 Область», кликнуть две точки. Затем проверить буфер:
```bash
wl-paste
```
Ожидаемо: печатается абсолютный путь к сохранённому PNG (в `~/.local/share/health-widget/screenshots/...png`), файл существует по этому пути. Вставка пути в терминал колонки даёт корректную строку.

- [ ] **Step 4: Commit**

```bash
git add src/screenshot.rs
git commit -m "feat(screenshot): «Скрин» кладёт в буфер путь к PNG (для терминала)"
```

---

## Self-Review Notes

- Спека покрыта: встроенный терминал (Task 1–3), путь в буфер (Task 4), `chat.rs` оставлен (Task 3 Step 5 `#[allow(dead_code)]`), LLM/OCR не добавляются (нет таких задач).
- Единственная зависимость от версии крейта локализована в Task 1 (спайк фиксирует имена) и явно помечена в Task 2 — не placeholder, а инструкция сверить с конкретным примером.
- Критерии готовности из спеки проверяются в Task 3 Step 6 и Task 4 Step 3.
