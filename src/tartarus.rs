
use egui::Context;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, InputEvent, KeyCode, RelativeAxisCode};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const VENDOR: u16 = 0x1532;
const PRODUCT: u16 = 0x0244;

const COLOR: (u8, u8, u8) = (0, 255, 0);

const EV_KEY: u16 = 1;
const EV_SYN: u16 = 0;

const KEY_MAP: &[(KeyCode, &[KeyCode])] = &[
    (KeyCode::KEY_1, &[KeyCode::KEY_F13]),
    (KeyCode::KEY_5, &[KeyCode::KEY_F17]),
    (KeyCode::KEY_TAB, &[KeyCode::KEY_F18]),
    (KeyCode::KEY_Q, &[KeyCode::KEY_F19]),
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

#[derive(Clone)]
pub struct Handles {
    pub shot_request: Arc<AtomicBool>,
    // pub cursor_warp_request: Arc<AtomicBool>,
    pub send_mic: Arc<AtomicBool>,
    pub send_zoom: Arc<AtomicBool>,
    pub send_mic_p2: Arc<AtomicBool>,
    pub send_zoom_p2: Arc<AtomicBool>,
    pub clear_chat: Arc<AtomicBool>,
    pub paste_code: Arc<AtomicBool>,
    pub switch_provider: Arc<AtomicBool>,
    pub move_dx: Arc<AtomicI32>,
    pub move_dy: Arc<AtomicI32>,
    pub move_next: Arc<AtomicBool>,
    pub wheel: Arc<AtomicI32>,
    pub ctx: Context,
}

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
            serve(devs, &handles);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn find_devices() -> Vec<Device> {
    evdev::enumerate()
        .filter_map(|(_, d)| {
            let id = d.input_id();
            (id.vendor() == VENDOR && id.product() == PRODUCT).then_some(d)
        })
        .collect()
}

fn serve(mut devs: Vec<Device>, handles: &Handles) {
    for d in &mut devs {
        if let Err(e) = d.grab() {
            eprintln!("tartarus: не удалось захватить ноду ({e}); пропускаю цикл");
            return;
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
                return;
            }
            if re & libc::POLLIN == 0 {
                continue;
            }
            let events: Vec<(EventType, u16, i32)> = match devs[i].fetch_events() {
                Ok(it) => it
                    .filter(|ev| {
                        ev.event_type() == EventType::KEY
                            || ev.event_type() == EventType::RELATIVE
                    })
                    .map(|ev| (ev.event_type(), ev.code(), ev.value()))
                    .collect(),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        continue;
                    }
                    eprintln!("tartarus: устройство отключено ({e})");
                    return;
                }
            };
            for (etype, code, value) in events {
                if etype == EventType::RELATIVE {
                    if code == RelativeAxisCode::REL_WHEEL.0 && value != 0 {
                        handles.wheel.fetch_add(value, Ordering::Relaxed);
                        handles.ctx.request_repaint();
                    }
                    continue;
                }
                handle_key(&mut ui, handles, code, value);
            }
        }
    }
}

fn dpad_delta(code: u16) -> Option<(i32, i32)> {
    if code == KeyCode::KEY_UP.0 {
        Some((1, 0))
    } else if code == KeyCode::KEY_DOWN.0 {
        Some((-1, 0))
    } else if code == KeyCode::KEY_LEFT.0 {
        Some((0, -1))
    } else if code == KeyCode::KEY_RIGHT.0 {
        Some((0, 1))
    } else {
        None
    }
}

fn handle_key(ui: &mut VirtualDevice, h: &Handles, code: u16, value: i32) {
    if let Some((dx, dy)) = dpad_delta(code) {
        if value != 0 {
            h.move_dx.fetch_add(dx, Ordering::Relaxed);
            h.move_dy.fetch_add(dy, Ordering::Relaxed);
            h.ctx.request_repaint();
        }
        return;
    }
    if value == 1 {
        if code == KeyCode::KEY_W.0 {
            h.send_zoom_p2.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_5.0 {
            h.shot_request.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_SPACE.0 {
            h.move_next.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_2.0 {
            h.send_mic.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_Q.0 {
            h.send_zoom.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_3.0 {
            h.send_mic_p2.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_A.0 {
            h.clear_chat.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_R.0 {
            h.paste_code.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
        if code == KeyCode::KEY_4.0 {
            h.switch_provider.store(true, Ordering::Relaxed);
            h.ctx.request_repaint();
            return;
        }
    } else if matches!(code, c if c == KeyCode::KEY_5.0
        || c == KeyCode::KEY_SPACE.0
        || c == KeyCode::KEY_2.0
        || c == KeyCode::KEY_Q.0
        || c == KeyCode::KEY_W.0
        || c == KeyCode::KEY_A.0
        || c == KeyCode::KEY_R.0
        || c == KeyCode::KEY_4.0
        || c == KeyCode::KEY_3.0)
    {
        return;
    }

    if let Some((_, out)) = KEY_MAP.iter().find(|(k, _)| k.0 == code) {
        emit(ui, out, value);
    } else if value == 1 {
        crate::telemetry::event(
            "tartarus.unknown_key",
            serde_json::json!({ "code": code, "name": format!("{:?}", KeyCode(code)) }),
        );
    }
}

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
    evs.push(InputEvent::new(EV_SYN, 0, 0));
    let _ = ui.emit(&evs);
}

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

const TRANSACTION_ID: u8 = 0x1F;

fn set_color() {
    match control_node().and_then(|p| static_color(&p, COLOR.0, COLOR.1, COLOR.2).ok()) {
        Some(()) => eprintln!("tartarus: rgb {COLOR:?}"),
        None => eprintln!("tartarus: rgb не удалось (нет доступа к hidraw?)"),
    }
}

fn control_node() -> Option<std::path::PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir("/sys/class/hidraw")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    nodes.sort();
    for node in nodes {
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

fn static_color(path: &std::path::Path, r: u8, g: u8, b: u8) -> std::io::Result<()> {
    let report = razer_report(0x0F, 0x02, &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, r, g, b]);
    send(path, &report)
}

fn razer_report(cmd_class: u8, cmd_id: u8, data: &[u8]) -> [u8; 90] {
    assert!(data.len() <= 80);
    let mut body = [0u8; 88];
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
    report[88] = crc;
    report
}

fn send(path: &std::path::Path, report: &[u8; 90]) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut buf = [0u8; 91];
    buf[1..].copy_from_slice(report);
    let req = hidiocsfeature(buf.len());
    let ret = unsafe { libc::ioctl(f.as_raw_fd(), req, buf.as_ptr()) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    std::thread::sleep(Duration::from_micros(600));
    Ok(())
}

fn hidiocsfeature(len: usize) -> libc::c_ulong {
    let dir = 3u32;
    let typ = b'H' as u32;
    let nr = 0x06u32;
    (((dir) << 30) | ((len as u32) << 16) | (typ << 8) | nr) as libc::c_ulong
}
