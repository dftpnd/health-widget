//! Зашиваем в бинарь короткий git-хеш и время сборки, чтобы версия виджета была видна
//! в UI и запущенный устаревший экземпляр палился с одного взгляда.
use std::process::Command;

fn main() {
    // Короткий git-хеш (+суффикс "-dirty", если есть незакоммиченные правки).
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "nogit".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let hash = if dirty { format!("{hash}-dirty") } else { hash };
    println!("cargo:rustc-env=GIT_HASH={hash}");

    // Локальное время сборки (без внешних крейтов — через системный `date`).
    let when = Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "?".into());
    println!("cargo:rustc-env=BUILD_TIME={when}");

    // Пересобирать штамп при смене HEAD/индекса.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
