
use std::process::Command;

pub fn available() -> bool {
    has_tool("pw-dump", &["--version"]) || has_tool("busctl", &["--version"])
}

pub fn screencast_active() -> bool {
    pipewire_screencast_active() || mutter_screencast_active()
}

fn has_tool(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn pipewire_screencast_active() -> bool {
    let output = match Command::new("pw-dump").output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let objs: Vec<serde_json::Value> = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return false,
    };

    objs.iter().any(|o| {
        o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node") && {
            let info = match o.get("info") {
                Some(i) => i,
                None => return false,
            };
            let running = info.get("state").and_then(|s| s.as_str()) == Some("running");
            let is_screencast = info
                .get("props")
                .and_then(|p| p.get("media.class"))
                .and_then(|c| c.as_str())
                == Some("Stream/Output/Video");
            running && is_screencast
        }
    })
}

fn mutter_screencast_active() -> bool {
    let output = Command::new("busctl")
        .args([
            "--user",
            "tree",
            "org.gnome.Mutter.ScreenCast",
            "--no-pager",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.contains("/org/gnome/Mutter/ScreenCast/Session")
                || text.contains("/org/gnome/Mutter/RemoteDesktop/Session")
        }
        _ => false,
    }
}
