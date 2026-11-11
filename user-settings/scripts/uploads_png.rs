// Pasted images land in ~/.agent-box/uploads as .tiff (what the macOS clipboard offers), a folder the
// designer agent may not read images from. This mirrors every pasted image as a .png into
// `<settings folder>/uploads/<same name>.png` (converted with sips when the source is a .tiff) as soon
// as it appears, so an agent reads it from the settings folder, where it has access.

fn home() -> String {
    if let Ok(v) = ab::call_sync("env", json!({"name": "HOME"})) {
        let h = v.as_str().or(v.get("value").and_then(|x| x.as_str())).unwrap_or("").to_string();
        if !h.is_empty() {
            return h;
        }
    }
    "/Users/jarron.conover".to_string()
}

fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home()),
        None => p.to_string(),
    }
}

fn uploads_dir() -> String {
    expand(ab::context()["settings"]["uploads"]["dir"].as_str().unwrap_or("~/.agent-box/uploads"))
}

fn mirror_dir() -> String {
    format!("{}/.agent-box/state/settings/uploads", home())
}

fn names_in(dir: &str) -> Vec<String> {
    ab::fs::list(dir)
        .unwrap_or(Value::Null)
        .as_array()
        .map(|a| a.iter().filter_map(|e| e["name"].as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

thread_local! {
    // Files already converted (or being converted) this run, so a burst of watch events converts once.
    static DONE: std::cell::RefCell<std::collections::HashSet<String>> = std::cell::RefCell::new(std::collections::HashSet::new());
}

fn convert_missing() {
    let src_dir = uploads_dir();
    let dst_dir = mirror_dir();
    let _ = ab::fs::mkdir(&dst_dir);
    let have = names_in(&dst_dir);
    for name in names_in(&src_dir) {
        let lower = name.to_lowercase();
        let is_tiff = lower.ends_with(".tiff") || lower.ends_with(".tif");
        let is_img = is_tiff || lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".webp") || lower.ends_with(".gif");
        if !is_img {
            continue;
        }
        let stem = name[..name.rfind('.').unwrap_or(name.len())].to_string();
        let png = format!("{stem}.png");
        if have.iter().any(|n| n == &png) {
            continue;
        }
        if !DONE.with(|d| d.borrow_mut().insert(png.clone())) {
            continue;
        }
        let src = format!("{src_dir}/{name}");
        let dst = format!("{dst_dir}/{png}");
        let cmd = if lower.ends_with(".png") {
            "cp \"$SRC\" \"$DST\"".to_string()
        } else {
            "sips -s format png \"$SRC\" --out \"$DST\" >/dev/null 2>&1".to_string()
        };
        ab::shell(&cmd, json!({"env": {"SRC": src, "DST": dst.clone()}, "timeout": 30}), move |r, _err| {
            if r["code"].as_i64().unwrap_or(1) == 0 {
                ab::log(format!("uploads_png: wrote {dst}"));
            } else {
                ab::log(format!("uploads_png: failed for {dst}: {}", r["stderr"].as_str().unwrap_or("")));
            }
        });
    }
}

fn init() {
    ab::on("start", |_, _| {
        convert_missing();
        let dir = uploads_dir();
        ab::fs::watch(&dir, |_paths| {
            // Give the app a moment to finish writing the file it just pasted.
            ab::after(400, || convert_missing());
        });
    });
}
