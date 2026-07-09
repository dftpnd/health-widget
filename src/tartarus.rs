//! Razer Tartarus Pro (1532:0244): эксклюзивный захват + ремап на F13–F24 + RGB.
//!
//! Порт бывшего внешнего root-сервиса `tartarus` (Python `remap.py`/`rgb.py`)
//! прямо внутрь виджета. Запускается фоновым потоком при старте виджета и живёт
//! вместе с ним — биндинги работают, только пока виджет запущен.
//!
//! Что делает поток:
//!   * ждёт устройство, грабит все его evdev-ноды (`EVIOCGRAB`) — родные коды
//!     (1/2/3/Q/W/E…) в систему НЕ уходят;
//!   * через виртуальное uinput-устройство шлёт невидимые `F13–F24`
//!     (и `Ctrl+F13…`), удобные как хоткеи;
//!   * четыре клавиши-действия (5 / R / F / Space) вместо F-клавиш напрямую
//!     дёргают состояние виджета (скрин / выделение микрофона / выделение
//!     телемоста / фокус терминала) — раньше это делал внешний `pkill --signal`;
//!   * при подключении ставит зелёную подсветку через `hidraw`;
//!   * переживает отключение/переподключение устройства (цикл с паузой 2 с).
//!
//! Доступ к нодам даёт udev-правило `99-tartarus.rules` (TAG uaccess) — без root
//! и без группы `input`. `/dev/uinput` уже доступен пользователю (ACL от dotool).

use egui::Context;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, InputEvent, KeyCode};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const VENDOR: u16 = 0x1532;
const PRODUCT: u16 = 0x0244; // Tartarus Pro

const COLOR: (u8, u8, u8) = (0, 255, 0); // зелёная подсветка при подключении

const EV_KEY: u16 = 1; // EventType::KEY
const EV_SYN: u16 = 0; // EventType::SYNCHRONIZATION (SYN_REPORT = code 0)

/// Физический код (как шлёт Tartarus) → что эмитим (срез = комбо клавиш).
/// Полная копия `KEY_MAP` из `remap.py`. Клавиши-действия (5/R/F/Space) здесь
/// тоже есть, но перебиваются `ACTIONS` в [`handle_key`].
const KEY_MAP: &[(KeyCode, &[KeyCode])] = &[
    (KeyCode::KEY_1, &[KeyCode::KEY_F13]),
    (KeyCode::KEY_2, &[KeyCode::KEY_F14]),
    (KeyCode::KEY_3, &[KeyCode::KEY_F15]),
    (KeyCode::KEY_4, &[KeyCode::KEY_F16]),
    (KeyCode::KEY_5, &[KeyCode::KEY_F17]),
    (KeyCode::KEY_TAB, &[KeyCode::KEY_F18]),
    (KeyCode::KEY_Q, &[KeyCode::KEY_F19]),
    (KeyCode::KEY_W, &[KeyCode::KEY_F20]),
    (KeyCode::KEY_E, &[KeyCode::KEY_F21]),
    (KeyCode::KEY_R, &[KeyCode::KEY_F22]),
    (KeyCode::KEY_CAPSLOCK, &[KeyCode::KEY_F23]),
    (KeyCode::KEY_A, &[KeyCode::KEY_F24]),
    (KeyCode::KEY_S, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F13]),
    (KeyCode::KEY_D, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F14]),
    (KeyCode::KEY_F, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F15]),
    (KeyCode::KEY_LEFTSHIFT, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F16]),
    (KeyCode::KEY_Z, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F17]),
    (KeyCode::KEY_X, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F18]),
    (KeyCode::KEY_C, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F19]),
    (KeyCode::KEY_SPACE, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F20]),
    (KeyCode::KEY_LEFTALT, &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_F21]),
    (KeyCode::KEY_UP, &[KeyCode::KEY_UP]),
    (KeyCode::KEY_DOWN, &[KeyCode::KEY_DOWN]),
    (KeyCode::KEY_LEFT, &[KeyCode::KEY_LEFT]),
    (KeyCode::KEY_RIGHT, &[KeyCode::KEY_RIGHT]),
];

/// Разделяемое с UI состояние — те же `Arc`, что дёргает поток сигналов в
/// `main.rs`. Клавиши-действия Tartarus теперь пишут прямо сюда, минуя pkill.
#[derive(Clone)]
pub struct Handles {
    /// Скрин области (клавиша 5). Раньше — SIGUSR2.
    pub shot_request: Arc<AtomicBool>,
    /// Тумблер выделения транскрипции микрофона (клавиша R). Раньше — SIGRTMIN+0.
    pub mark_mic: Arc<AtomicU32>,
    /// Тумблер выделения транскрипции телемоста (клавиша F). Раньше — SIGRTMIN+1.
    pub mark_zoom: Arc<AtomicU32>,
    /// Фокус терминала + увод курсора (клавиша Space). Раньше — SIGRTMIN+2.
    pub cursor_warp_request: Arc<AtomicBool>,
    pub ctx: Context,
}

/// Запускает фоновый поток обработки Tartarus. Вызывать один раз при старте.
pub fn spawn(handles: Handles) {
    std::thread::Builder::new()
        .name("tartarus".into())
        .spawn(move || run(handles))
        .expect("cannot spawn tartarus thread");
}

fn run(handles: Handles) {
    eprintln!("tartarus: жду устройство {VENDOR:04x}:{PRODUCT:04x}…");
    loop {
        let devs = find_devices();
        if !devs.is_empty() {
            serve(devs, &handles); // блокирует, пока устройство на месте
        }
        std::thread::sleep(Duration::from_secs(2)); // ждём (пере)подключения
    }
}

/// Все evdev-ноды нашего устройства (как `find_devices` в Python).
fn find_devices() -> Vec<Device> {
    evdev::enumerate()
        .filter_map(|(_, d)| {
            let id = d.input_id();
            (id.vendor() == VENDOR && id.product() == PRODUCT).then_some(d)
        })
        .collect()
}

/// Грабит устройство, ставит цвет, ремапит. Возвращается при отключении.
fn serve(mut devs: Vec<Device>, handles: &Handles) {
    for d in &mut devs {
        if let Err(e) = d.grab() {
            eprintln!("tartarus: не удалось захватить ноду ({e}); пропускаю цикл");
            return; // отпустим уже захваченные через Drop и попробуем снова
        }
    }
    let mut ui = match build_uinput() {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("tartarus: uinput недоступен ({e})");
            return;
        }
    };
    eprintln!("tartarus: захвачено {} нод, ремап активен (F13–F24)", devs.len());
    set_color();

    // Опрашиваем fd всех нод через poll(2) — как selector в Python.
    let mut fds: Vec<libc::pollfd> = devs
        .iter()
        .map(|d| libc::pollfd {
            fd: d.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();

    loop {
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("tartarus: poll error ({err})");
            break;
        }
        for i in 0..fds.len() {
            let re = fds[i].revents;
            if re & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                eprintln!("tartarus: устройство отключено");
                return; // grab снимется Drop-ом Device
            }
            if re & libc::POLLIN == 0 {
                continue;
            }
            // Сначала считываем события (борроу devs[i]), затем обрабатываем.
            let events: Vec<(u16, i32)> = match devs[i].fetch_events() {
                Ok(it) => it
                    .filter(|ev| ev.event_type() == EventType::KEY)
                    .map(|ev| (ev.code(), ev.value()))
                    .collect(),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        continue;
                    }
                    eprintln!("tartarus: устройство отключено ({e})");
                    return;
                }
            };
            for (code, value) in events {
                handle_key(&mut ui, handles, code, value);
            }
        }
    }
}

/// Одно событие клавиши: приоритет у действий (только на нажатие), иначе ремап.
fn handle_key(ui: &mut VirtualDevice, h: &Handles, code: u16, value: i32) {
    // ACTIONS: срабатывают только на нажатие (value == 1), имеют приоритет.
    if value == 1 {
        if code == KeyCode::KEY_5.0 {
            h.shot_request.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_R.0 {
            h.mark_mic.fetch_add(1, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_F.0 {
            h.mark_zoom.fetch_add(1, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_SPACE.0 {
            h.cursor_warp_request.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
    } else if matches!(code, c if c == KeyCode::KEY_5.0
        || c == KeyCode::KEY_R.0
        || c == KeyCode::KEY_F.0
        || c == KeyCode::KEY_SPACE.0)
    {
        // hold/up клавиш-действий гасим — они не должны утекать как F-клавиши.
        return;
    }

    if let Some((_, out)) = KEY_MAP.iter().find(|(k, _)| k.0 == code) {
        emit(ui, out, value);
    }
}

/// Эмитит комбо через uinput: на нажатие в прямом порядке, на отпускание —
/// в обратном, затем SYN_REPORT (как `emit()`/`ui.syn()` в Python).
fn emit(ui: &mut VirtualDevice, codes: &[KeyCode], value: i32) {
    let mut evs: Vec<InputEvent> = Vec::with_capacity(codes.len() + 1);
    if value == 1 {
        for c in codes {
            evs.push(InputEvent::new(EV_KEY, c.0, value));
        }
    } else {
        for c in codes.iter().rev() {
            evs.push(InputEvent::new(EV_KEY, c.0, value));
        }
    }
    evs.push(InputEvent::new(EV_SYN, 0, 0)); // SYN_REPORT
    let _ = ui.emit(&evs);
}

/// Создаёт виртуальное устройство, объявив все коды, которые вообще эмитим.
fn build_uinput() -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for (_, out) in KEY_MAP {
        for c in *out {
            keys.insert(*c);
        }
    }
    VirtualDevice::builder()?
        .name("tartarus-virtual")
        .with_keys(&keys)?
        .build()
}

// ---------------------------------------------------------------------------
// RGB через hidraw (порт rgb.py). Управляющий интерфейс Tartarus Pro — :1.2.
// ---------------------------------------------------------------------------

const TRANSACTION_ID: u8 = 0x1F; // выверено для Tartarus Pro

/// Ставит наш цвет подсветки. Best-effort: при неудаче только логируем.
fn set_color() {
    match control_node().and_then(|p| static_color(&p, COLOR.0, COLOR.1, COLOR.2).ok()) {
        Some(()) => eprintln!("tartarus: rgb {COLOR:?}"),
        None => eprintln!("tartarus: rgb не удалось (нет доступа к hidraw?)"),
    }
}

/// Ищет /dev/hidrawN управляющего интерфейса (:1.2) нашего устройства.
fn control_node() -> Option<std::path::PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir("/sys/class/hidraw")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    nodes.sort();
    for node in nodes {
        // Нода без device-симлинка или недоступная — просто пропускаем, не бросаем поиск.
        let Ok(real) = std::fs::canonicalize(node.join("device")) else {
            continue;
        };
        let s = real.to_string_lossy();
        if s.contains("1532:0244") && s.contains(":1.2/") {
            if let Some(name) = node.file_name() {
                return Some(std::path::PathBuf::from(format!(
                    "/dev/{}",
                    name.to_string_lossy()
                )));
            }
        }
    }
    None
}

/// Сплошной цвет на всё устройство (razer extended matrix, class 0x0F id 0x02).
fn static_color(path: &std::path::Path, r: u8, g: u8, b: u8) -> std::io::Result<()> {
    let report = razer_report(0x0F, 0x02, &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, r, g, b]);
    send(path, &report)
}

/// Собирает 90-байтный razer_report с корректным CRC (XOR байтов 2..87).
fn razer_report(cmd_class: u8, cmd_id: u8, data: &[u8]) -> [u8; 90] {
    assert!(data.len() <= 80);
    let mut body = [0u8; 88];
    // [0]=status [1]=tid [2..4]=remaining(be16)=0 [4]=proto [5]=data_size
    // [6]=class [7]=id [8..]=args
    body[1] = TRANSACTION_ID;
    body[5] = data.len() as u8;
    body[6] = cmd_class;
    body[7] = cmd_id;
    body[8..8 + data.len()].copy_from_slice(data);
    let mut crc = 0u8;
    for &x in &body[2..88] {
        crc ^= x;
    }
    let mut report = [0u8; 90];
    report[..88].copy_from_slice(&body);
    report[88] = crc; // [89] reserved = 0
    report
}

/// Шлёт репорт как HID feature (report id 0 спереди → 91 байт).
fn send(path: &std::path::Path, report: &[u8; 90]) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut buf = [0u8; 91]; // buf[0] = report id 0
    buf[1..].copy_from_slice(report);
    let req = hidiocsfeature(buf.len());
    let ret = unsafe { libc::ioctl(f.as_raw_fd(), req, buf.as_ptr()) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    std::thread::sleep(Duration::from_micros(600)); // как в драйвере
    Ok(())
}

/// HIDIOCSFEATURE(len) = _IOC(READ|WRITE, 'H', 0x06, len).
fn hidiocsfeature(len: usize) -> libc::c_ulong {
    let dir = 3u32; // READ(2) | WRITE(1)
    let typ = b'H' as u32;
    let nr = 0x06u32;
    (((dir) << 30) | ((len as u32) << 16) | (typ << 8) | nr) as libc::c_ulong
}
