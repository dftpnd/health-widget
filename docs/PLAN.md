# План развития: из виджета-метрик в AI-ассистента-оверлей

> Черновик архитектуры для итеративной работы. Двигаемся по фазам, каждая фаза — рабочий бинарь.

## 0. Где мы сейчас (точка старта)

Текущий `health-widget` уже даёт готовый фундамент, на который ложится всё остальное:

- **`src/main.rs`** — egui/eframe окно: без декораций, прозрачное, always-on-top, `clear_color` = 0 alpha, тумблер видимости по `SIGUSR1`, авто-reload данных. → станет слоем **`ui`** + оркестратором.
- **`src/detect.rs`** — детект активного захвата экрана через PipeWire (`pw-dump`, нода `Stream/Output/Video` в `running`) с резервом на Mutter/D-Bus. → переезжает в **`platform`**.
- **`src/config.rs` / `src/data.rs`** — конфиг + модель данных из JSON. → основа доменных типов.
- **`kwin-script/health-widget-exclude`** + `install-kde-privacy.sh` — `excludeFromCapture`, виджет виден локально, но не в захвате. → слой **`platform`**, переиспользуется как есть.

Иными словами: окно, прозрачность, always-on-top, скрытие от захвата, хоткей-тумблер, детект шаринга — **уже есть**. Достраиваем аудио, STT, LLM, скриншот, авто-ввод.

## Замечание о назначении

Часть возможностей (слушать собеседников в созвоне → GPT → авто-печать ответа «как человек» при скрытом от screenshare виджете) заточена под то, чтобы выдавать ответы ИИ за свои на живой проверке. Механизмы описаны как общие возможности; это часто нарушает правила площадок. Решение за пользователем.

---

## 1. Целевая архитектура (SOLID / GRASP)

Домен зависит от **портов** (трейтов), не от реализаций (Dependency Inversion). Каждая capability — свой модуль/крейт с одной ответственностью (High Cohesion), связь — через каналы и трейты (Low Coupling).

Переходим на **cargo workspace**:

```
health-widget/
├─ crates/
│  ├─ core/       # доменные трейты (порты) + типы Action/Segment/Prompt. Без зависимостей на impl
│  ├─ audio/      # PipeWire захват (мик + monitor вывода) + VAD        -> impl AudioSource
│  ├─ stt/        # whisper-rs на GPU (CUDA/Vulkan), стриминг            -> impl Transcriber
│  ├─ llm/        # async-openai (GPT, vision, стриминг)                 -> impl LlmClient
│  ├─ capture/    # ashpd (xdg-desktop-portal) скриншот + UI кропа       -> impl ScreenGrabber
│  ├─ input/      # ydotool / zwp_virtual_keyboard_v1, авто-ввод         -> impl Typist
│  ├─ hotkeys/    # KGlobalAccel (D-Bus) / портал GlobalShortcuts        -> impl HotkeyRegistry
│  ├─ platform/   # KWin excludeFromCapture, флаги окна, detect (из detect.rs)
│  └─ ui/         # egui: осциллограф, чат, панель кнопок, кроп, прозрачность (из main.rs)
└─ app/           # bin: AppController, собирает порты, гоняет шину Action
```

### Порты в `core`

```rust
trait AudioSource   { fn frames(&self) -> Receiver<PcmFrame>; fn switch(&self, src: Src); }
trait Transcriber   { async fn stream(&self, rx: Receiver<PcmFrame>) -> Receiver<Segment>; } // partial+final
trait LlmClient     { async fn ask(&self, req: Prompt) -> TokenStream; }   // текст + картинки
trait ScreenGrabber { async fn grab(&self, target: Target) -> Image; }
trait Typist        { async fn type_text(&self, text: &str, speed: Speed, cancel: Token); }
trait HotkeyRegistry{ fn events(&self) -> Receiver<Action>; }
```

### Ключевой приём — «кнопки дублируют хоткеи» (Open/Closed + Command / GRASP Controller)

Единый `enum Action` (`AskVoice`, `Screenshot`, `AutoSolve`, `ToggleOpacity`, `SwitchAudioSrc`, …). И кнопки UI, и `HotkeyRegistry` шлют **один и тот же** `Action` в общую шину. `AppController` — единственное место, где Action превращается в вызовы портов. Новая функция = один вариант enum + одна ветка; кнопка и хоткей подхватываются автоматически.

GRASP-роли: `AppController` — Controller/Indirection; трейты — Polymorphism + Protected Variations (заменить whisper/GPT без правки ядра); границы крейтов — High Cohesion; каналы `tokio::mpsc`/`crossbeam` — Low Coupling.

---

## 2. Три конвейера (требования → код)

**A. Голосовой вопрос.** `Action::AskVoice` → `AudioSource` (мик или monitor «собеседников») → кадры параллельно в `ui` (осциллограмма) и в VAD → сегменты → `Transcriber` (whisper на GPU, стриминг) → текст → `LlmClient.ask` → токены стримятся в чат виджета.

**B. Вопрос по скриншоту.** `Action::Screenshot` → `ScreenGrabber.grab` (портал/KWin) → **UI-кроп** (обрезка перед отправкой) → `LlmClient.ask` с картинкой (vision) + опц. текст → ответ в виджет.

**C. Авто-решение / печать.** Ответ из A/B → `Typist.type_text(text, speed)` печатает в активное окно с настраиваемой скоростью и стоп/паузой (cancel-token).

**Осциллограмма:** `ui` держит кольцевой буфер последних N сэмплов, рисует их egui `Painter` каждый кадр.

**Окно:** уже без декораций/прозрачное/always-on-top. Добавить перетаскивание за любую точку, ручки ресайза, слайдер прозрачности (альфа окна), сохранить `excludeFromCapture`.

---

## 3. Подводные камни Wayland/KDE (снять первыми!)

Три пункта могут «убить» проект — прототипировать в Фазе 0, а не в конце.

| Возможность | Технология | Подвох |
|---|---|---|
| Глобальные хоткеи | **KGlobalAccel** по D-Bus (нативно KDE) или портал `GlobalShortcuts` | `global-hotkey` crate под Wayland почти не работает |
| Скриншот | xdg-desktop-portal через **`ashpd`** | нельзя молча снять экран — только через портал |
| Авто-ввод | uinput напрямую (`evdev` crate) или `ydotool` | синтез ввода под Wayland запрещён на уровне протокола |
| Захват «собеседников» | **PipeWire** monitor вывода (`pipewire` crate / `cpal`) | это monitor-источник, а не микрофон |
| STT на GPU | **`whisper-rs`** (whisper.cpp + Vulkan/CUDA) | сборка vulkan требует `glslc`; стриминг = скользящее окно + VAD |
| VAD | Silero (`voice_activity_detector`) или `webrtc-vad` | нужен для низкой задержки |
| LLM | `async-openai` | порт `LlmClient` спроектировать провайдеронезависимым |

## 3b. Результаты Фазы 0 (разведка проведена — факты этой машины)

Окружение: **KDE Plasma 6.6.5 / Wayland**, **RTX 5080 (16 ГБ)**, cargo/rustc 1.96, PipeWire 1.6.2.

| Риск | Статус | Вывод |
|---|---|---|
| **Авто-ввод** | ✅ снят, спайк работает | У `mgu` прямой `rw` к `/dev/uinput` (ACL). Спайк `spikes/uinput-type` на чистом Rust (`evdev`) создаёт виртуальную клавиатуру — ядро видит её как `kbd`/eventN, KWin отдаёт ввод в фокус. **ydotool/демон/root не нужны.** (Round-trip чтение недоступно — `mgu` не в группе `input` — но для Typist это не нужно, ввод пишется в uinput.) |
| **Хоткеи** | ✅ снят, спайк работает | Спайк `spikes/hotkey-kglobalaccel` (`zbus`): `doRegister` → `setShortcut(qtKey, flags=6)` → `getComponent` → сигнал `globalShortcutPressed(ssx)`. Сквозной тест PASS: зарегистрировал Ctrl+Alt+Y, «нажал» его инъекцией через uinput, поймал сигнал. Qt-код = `Qt::Key \| модификаторы` (Ctrl=0x04000000, Alt=0x08000000). Уборка: `setInactive` + `unregister`. |
| **Скриншот** | ✅ снят | `spectacle -b -n -f` снял весь экран (5120×2160) в фоне без диалога. `kde.portal` есть → прод-путь через `ashpd`. |
| **STT на GPU** | ⚠️ путь ясен, нужен install | CUDA toolkit (nvcc) **нет**; Vulkan на 5080 **работает**. Берём whisper.cpp на **Vulkan**. Единственная доустановка: **`glslc`/shaderc** (компилятор шейдеров) для сборки vulkan-бэкенда. |
| **Аудио** | ✅ жизнеспособно | PipeWire 1.6.2, `pw-dump`/`pactl` на месте; захват monitor-источника реалистичен. |

Решения, закрытые разведкой: **авто-ввод — uinput напрямую на Rust** (а не ydotool); **STT — Vulkan** (а не CUDA); **хоткеи — KGlobalAccel**.

---

## 4. Дорожная карта по фазам

- **Фаза 0 — разведка боем (снять риски).** ✅ ПРОВЕДЕНА (см. §3b). Работают спайки: авто-ввод `spikes/uinput-type` и хоткей `spikes/hotkey-kglobalaccel`. Осталось по желанию: спайк `ashpd`-скриншота. Для STT — доустановить `glslc`.
- **Фаза 1 — каркас.** Перевод в workspace, `core`-трейты, `AppController`, шина `Action`. Текущий виджет работает как раньше + добавлены: перетаскивание, ресайз, слайдер прозрачности.
- **Фаза 2 — аудио + осциллограмма.** PipeWire: выбор источника (мик / monitor), рисование волны. Без STT.
- **Фаза 3 — STT на GPU.** `whisper-rs` + CUDA, стриминг с VAD, замер задержки, выбор модели (small/medium int8 / distil-whisper).
- **Фаза 4 — LLM.** `async-openai`, стриминг ответа в чат, история диалога.
- **Фаза 5 — скриншот + кроп + vision.** Портал → кроп-UI → отправка картинки в GPT.
- **Фаза 6 — авто-ввод** с регулируемой скоростью и старт/стоп.
- **Фаза 7 — полировка.** Единая панель кнопок = хоткеи, экран настроек, конфиг.

---

## 5. Открытые решения (обсудить перед стартом)

- **Хоткеи:** KGlobalAccel (нативно KDE, надёжно) vs портал `GlobalShortcuts` (портабельно, с диалогом). Рекомендация: KGlobalAccel.
- **Захват собеседников:** monitor всего вывода (проще) vs линковка к потоку Zoom/Телемост в PipeWire (чище). Начать с monitor.
- **LLM:** пользователь просит GPT (`async-openai`), но порт `LlmClient` заложить так, чтобы Claude/другой провайдер подключался без переписывания.
- **Судьба текущего функционала метрик здоровья:** оставить как отдельную вкладку/панель виджета или убрать? (Сейчас это основной функционал бинаря.)

---

## 6. Инвариант при переходе

На каждой фазе бинарь остаётся запускаемым, `excludeFromCapture` и детект захвата не ломаются, приватностный слой (README) продолжает работать.
