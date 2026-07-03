//! Спайк Фазы 0 — глобальный хоткей через KGlobalAccel (нативно KDE, Wayland).
//!
//! Флоу регистрации (по D-Bus org.kde.kglobalaccel /kglobalaccel):
//!   doRegister(actionId) -> setShortcut(actionId, [qtKey], flags) -> getComponent(name) -> o
//!   затем слушаем сигнал globalShortcutPressed(ssx) на объекте компонента.
//!
//! actionId = [componentUnique, actionUnique, componentFriendly, actionFriendly].
//! Клавиша кодируется как Qt QKeyCombination: Qt::Key | модификаторы.
//!
//! Самопроверка (headless): регистрируем Ctrl+Alt+Y, затем через uinput (спайк авто-ввода)
//! «нажимаем» эту комбинацию и ждём сигнал. Если реальное нажатие не долетело — фолбэк на
//! метод invokeShortcut (D-Bus), чтобы отдельно проверить, что связка регистрация+сигнал жива.

use std::time::Duration;

use evdev::{uinput::VirtualDeviceBuilder, AttributeSet, EventType, InputEvent, Key};
use futures_util::stream::StreamExt;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const SERVICE: &str = "org.kde.kglobalaccel";
const PATH: &str = "/kglobalaccel";
const IFACE: &str = "org.kde.KGlobalAccel";
const COMP_IFACE: &str = "org.kde.kglobalaccel.Component";

const COMPONENT: &str = "healthwidget_spike";
const ACTION: &str = "trigger";

// Qt-модификаторы (QKeyCombination) — стабильные значения Qt5/Qt6.
const CTRL: i32 = 0x0400_0000;
const ALT: i32 = 0x0800_0000;

/// Кандидаты хоткея: (человекочит., Qt-код, evdev-клавиша для инъекции).
/// Все — Ctrl+Alt+<буква>: модификаторы сами по себе безвредны, конфликтов мало.
fn candidates() -> Vec<(&'static str, i32, Key)> {
    vec![
        ("Ctrl+Alt+Y", CTRL | ALT | 0x59, Key::KEY_Y),
        ("Ctrl+Alt+U", CTRL | ALT | 0x55, Key::KEY_U),
        ("Ctrl+Alt+J", CTRL | ALT | 0x4A, Key::KEY_J),
    ]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::session().await?;
    let kga = Proxy::new(&conn, SERVICE, PATH, IFACE).await?;

    let action_id = vec![
        COMPONENT.to_string(),
        ACTION.to_string(),
        "Health Widget Spike".to_string(),
        "Trigger".to_string(),
    ];

    kga.call_method("doRegister", &(action_id.clone(),)).await?;
    println!("[ok] doRegister({COMPONENT}/{ACTION})");

    // Выбираем первый доступный хоткей.
    let mut chosen: Option<(&'static str, i32, Key)> = None;
    for (name, qt, evk) in candidates() {
        let available: bool = kga
            .call("isGlobalShortcutAvailable", &(qt, COMPONENT.to_string()))
            .await
            .unwrap_or(false);
        println!("[..] {name} ({qt:#010x}) доступен: {available}");
        if available {
            chosen = Some((name, qt, evk));
            break;
        }
    }
    let (name, qt_key, ev_key) = chosen.ok_or("нет свободного хоткея из кандидатов")?;

    // setShortcut, flags = SetPresent(2) | NoAutoloading(4) = 6.
    let assigned: Vec<i32> = kga
        .call("setShortcut", &(action_id.clone(), vec![qt_key], 6u32))
        .await?;
    println!("[ok] setShortcut {name} -> назначено ядром: {assigned:?}");

    let comp_path: OwnedObjectPath = kga.call("getComponent", &(COMPONENT.to_string(),)).await?;
    println!("[ok] getComponent -> {}", comp_path.as_str());

    let comp = Proxy::new(&conn, SERVICE, comp_path.as_str().to_string(), COMP_IFACE).await?;
    let mut stream = comp.receive_signal("globalShortcutPressed").await?;

    // Режим --listen: ждём РЕАЛЬНЫХ нажатий пользователя, без авто-инъекции.
    if std::env::args().any(|a| a == "--listen") {
        println!("\n>>> Жми {name} (это Ctrl+Alt+Y). Каждое нажатие отобразится ниже. Ctrl+C — выход. <<<\n");
        let mut n = 0u32;
        loop {
            tokio::select! {
                sig = stream.next() => match sig {
                    Some(_) => { n += 1; println!("[HOTKEY #{n}] нажатие {name} поймано ✔"); }
                    None => break,
                },
                _ = tokio::signal::ctrl_c() => { println!("\n[..] выход по Ctrl+C"); break; }
            }
        }
        let _ = kga.call_method("setInactive", &(action_id.clone(),)).await;
        let _: Result<bool, _> = kga
            .call("unregister", &(COMPONENT.to_string(), ACTION.to_string()))
            .await;
        println!("[ok] cleanup: поймано нажатий = {n}");
        return Ok(());
    }

    // Инъекция реального нажатия через uinput спустя 1.5с (стрим уже подписан).
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        match inject_combo(ev_key) {
            Ok(()) => println!("[emit] инжектировал {name} через uinput"),
            Err(e) => eprintln!("[warn] инъекция не удалась: {e}"),
        }
    });

    // Ждём сигнал от реального нажатия.
    let mut fired_by: Option<&str> = None;
    match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        Ok(Some(_)) => fired_by = Some("uinput-инъекция (реальное нажатие клавиш)"),
        _ => {
            println!("[..] сигнал от инъекции не пришёл за 5с — пробую invokeShortcut (D-Bus)");
            let _ = comp.call_method("invokeShortcut", &(ACTION.to_string(),)).await;
            if let Ok(Some(_)) = tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
                fired_by = Some("invokeShortcut (D-Bus)");
            }
        }
    }

    // Уборка: снять хоткей и разрегистрировать.
    let _ = kga.call_method("setInactive", &(action_id.clone(),)).await;
    let _: Result<bool, _> = kga
        .call("unregister", &(COMPONENT.to_string(), ACTION.to_string()))
        .await;
    println!("[ok] cleanup: setInactive + unregister");

    match fired_by {
        Some(how) => {
            println!("\n[PASS] globalShortcutPressed получен через: {how}");
            println!("Глобальные хоткеи KGlobalAccel работают — фундамент шины Action готов.");
        }
        None => {
            println!("\n[FAIL] сигнал не пойман ни инъекцией, ни invokeShortcut.");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// «Нажать» Ctrl+Alt+<key> через свежую виртуальную клавиатуру uinput.
fn inject_combo(letter: Key) -> std::io::Result<()> {
    let mut keys = AttributeSet::<Key>::new();
    keys.insert(Key::KEY_LEFTCTRL);
    keys.insert(Key::KEY_LEFTALT);
    keys.insert(letter);

    let mut dev = VirtualDeviceBuilder::new()?
        .name("health-widget-hotkey-spike")
        .with_keys(&keys)?
        .build()?;

    // Дать KWin увидеть новое устройство до эмита.
    std::thread::sleep(Duration::from_millis(500));

    let down = [Key::KEY_LEFTCTRL, Key::KEY_LEFTALT, letter];
    for k in down {
        dev.emit(&[InputEvent::new(EventType::KEY, k.code(), 1)])?;
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(40));
    for k in down.into_iter().rev() {
        dev.emit(&[InputEvent::new(EventType::KEY, k.code(), 0)])?;
        std::thread::sleep(Duration::from_millis(25));
    }
    // Дать событиям дойти до композитора до дропа устройства.
    std::thread::sleep(Duration::from_millis(150));
    Ok(())
}
