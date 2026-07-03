//! Спайк Фазы 0 — авто-ввод (Typist) на чистом Rust через /dev/uinput.
//!
//! Цель: доказать, что синтез клавиатурного ввода работает БЕЗ ydotool/демона/root —
//! только за счёт прямого rw-доступа пользователя к /dev/uinput (ACL).
//!
//! Самопроверка (headless, без фокуса на текстовом поле): создаём виртуальную клавиатуру,
//! в отдельном потоке читаем ЕЁ ЖЕ event-ноду и эмитим несколько нажатий RIGHTCTRL
//! (модификатор без побочных эффектов — не печатает символов в чужое окно). Если ридер
//! видит те же коды — путь ядро→libinput→KWin рабочий.
//!
//! `--type "текст"` — демонстрация реальной печати с переменной скоростью в активное окно.
//! Запускать осознанно: символы уйдут в то окно, что сейчас в фокусе.

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use evdev::{
    uinput::VirtualDeviceBuilder, AttributeSet, EventType, InputEvent, Key,
};

const DEVICE_NAME: &str = "health-widget-virtkbd";

fn main() -> io::Result<()> {
    // Набор клавиш устройства: RIGHTCTRL для самопроверки + буквы/пробел для демо печати.
    let mut keys = AttributeSet::<Key>::new();
    keys.insert(Key::KEY_RIGHTCTRL);
    keys.insert(Key::KEY_LEFTSHIFT);
    keys.insert(Key::KEY_SPACE);
    for k in typing_keys() {
        keys.insert(k);
    }

    let mut device = VirtualDeviceBuilder::new()?
        .name(DEVICE_NAME)
        .with_keys(&keys)?
        .build()?;

    println!("[ok] виртуальная клавиатура '{DEVICE_NAME}' создана (доступ к /dev/uinput есть)");

    let mode_type = std::env::args().nth(1).as_deref() == Some("--type");

    if mode_type {
        let text = std::env::args().nth(2).unwrap_or_else(|| "hello from health-widget".into());
        println!("[demo] через 2с печатаю в активное окно: {text:?}");
        thread::sleep(Duration::from_secs(2));
        type_text(&mut device, &text)?;
        println!("[demo] готово");
        return Ok(());
    }

    // --- Самопроверка round-trip ---
    // Дать ядру зарегистрировать event-ноду, затем найти её по имени устройства.
    thread::sleep(Duration::from_millis(300));
    let node = evdev::enumerate()
        .find(|(_, d)| d.name() == Some(DEVICE_NAME))
        .map(|(path, _)| path);

    let (tx, rx) = mpsc::channel::<u16>();
    match node {
        Some(path) => {
            println!("[ok] event-нода нашего устройства: {}", path.display());
            let mut reader = evdev::Device::open(&path)?;
            thread::spawn(move || {
                // Читаем ~2с; отправляем коды нажатий (value==1) в канал.
                for _ in 0..200 {
                    if let Ok(events) = reader.fetch_events() {
                        for ev in events {
                            if ev.event_type() == EventType::KEY && ev.value() == 1 {
                                let _ = tx.send(ev.code());
                            }
                        }
                    }
                }
            });
        }
        None => {
            eprintln!("[warn] не нашли свою event-ноду через enumerate() — round-trip пропущен");
        }
    }

    // Эмитим 3 нажатия RIGHTCTRL с переменной задержкой (проверяем и «переменную скорость»).
    thread::sleep(Duration::from_millis(150));
    let code = Key::KEY_RIGHTCTRL.code();
    let mut emitted = 0;
    for (i, delay) in [40u64, 120, 250].into_iter().enumerate() {
        device.emit(&[InputEvent::new(EventType::KEY, code, 1)])?; // down
        device.emit(&[InputEvent::new(EventType::KEY, code, 0)])?; // up
        emitted += 1;
        println!("[emit] RIGHTCTRL #{} (задержка {delay}мс)", i + 1);
        thread::sleep(Duration::from_millis(delay));
    }

    // Собираем принятое ридером.
    thread::sleep(Duration::from_millis(200));
    let mut received = 0;
    while let Ok(c) = rx.recv_timeout(Duration::from_millis(300)) {
        if c == code {
            received += 1;
        }
    }

    println!("--- итог: эмитили {emitted}, ридер поймал {received} нажатий RIGHTCTRL ---");
    if received >= emitted {
        println!("[PASS] round-trip через ядро работает — авто-ввод на чистом Rust жизнеспособен");
    } else {
        println!("[PARTIAL] устройство создаётся и эмитит, но ридер поймал не все события");
    }
    Ok(())
}

/// Клавиши, нужные для печати ASCII-строки в демо-режиме (только a-z0-9 и пробел).
fn typing_keys() -> Vec<Key> {
    "abcdefghijklmnopqrstuvwxyz0123456789"
        .chars()
        .filter_map(char_to_key)
        .map(|(k, _)| k)
        .collect()
}

/// Печать строки с переменной скоростью (задержка растёт/падает — грубая имитация человека).
fn type_text(device: &mut evdev::uinput::VirtualDevice, text: &str) -> io::Result<()> {
    for (i, ch) in text.chars().enumerate() {
        let lower = ch.to_ascii_lowercase();
        let key = if lower == ' ' {
            Some((Key::KEY_SPACE, false))
        } else {
            char_to_key(lower)
        };
        if let Some((k, _)) = key {
            let shift = ch.is_ascii_uppercase();
            if shift {
                device.emit(&[InputEvent::new(EventType::KEY, Key::KEY_LEFTSHIFT.code(), 1)])?;
            }
            device.emit(&[InputEvent::new(EventType::KEY, k.code(), 1)])?;
            device.emit(&[InputEvent::new(EventType::KEY, k.code(), 0)])?;
            if shift {
                device.emit(&[InputEvent::new(EventType::KEY, Key::KEY_LEFTSHIFT.code(), 0)])?;
            }
        }
        // Переменная скорость: базовая задержка + «дрожание» по индексу символа.
        let jitter = 30 + (i * 17) % 90;
        thread::sleep(Duration::from_millis(jitter as u64));
    }
    Ok(())
}

/// Карта ASCII-символ → (клавиша, нужен ли shift). Хватит для спайка.
fn char_to_key(c: char) -> Option<(Key, bool)> {
    let k = match c {
        'a' => Key::KEY_A, 'b' => Key::KEY_B, 'c' => Key::KEY_C, 'd' => Key::KEY_D,
        'e' => Key::KEY_E, 'f' => Key::KEY_F, 'g' => Key::KEY_G, 'h' => Key::KEY_H,
        'i' => Key::KEY_I, 'j' => Key::KEY_J, 'k' => Key::KEY_K, 'l' => Key::KEY_L,
        'm' => Key::KEY_M, 'n' => Key::KEY_N, 'o' => Key::KEY_O, 'p' => Key::KEY_P,
        'q' => Key::KEY_Q, 'r' => Key::KEY_R, 's' => Key::KEY_S, 't' => Key::KEY_T,
        'u' => Key::KEY_U, 'v' => Key::KEY_V, 'w' => Key::KEY_W, 'x' => Key::KEY_X,
        'y' => Key::KEY_Y, 'z' => Key::KEY_Z,
        '0' => Key::KEY_0, '1' => Key::KEY_1, '2' => Key::KEY_2, '3' => Key::KEY_3,
        '4' => Key::KEY_4, '5' => Key::KEY_5, '6' => Key::KEY_6, '7' => Key::KEY_7,
        '8' => Key::KEY_8, '9' => Key::KEY_9,
        ' ' => Key::KEY_SPACE,
        _ => return None,
    };
    Some((k, false))
}
