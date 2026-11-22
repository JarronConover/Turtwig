// Embeds the baseline settings (`user-settings/` at the repo root) into the app: every script there
// becomes an `include_str!` entry of `BASELINE_SCRIPTS`, so a fresh install starts from this commit's files.
use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..").canonicalize().expect("repo root");
    let scripts = root.join("user-settings/scripts");
    println!("cargo:rerun-if-changed={}", scripts.display());
    println!("cargo:rerun-if-changed={}", root.join("user-settings/settings.jsonc").display());
    println!("cargo:rerun-if-changed={}", root.join("user-settings/ui-state.json").display());
    let mut names: Vec<String> = fs::read_dir(&scripts)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".rs"))
        .collect();
    names.sort();
    let mut out = String::from("pub const BASELINE_SCRIPTS: &[(&str, &str)] = &[\n");
    for n in &names {
        let p = scripts.join(n);
        println!("cargo:rerun-if-changed={}", p.display());
        out.push_str(&format!("    ({n:?}, include_str!({:?})),\n", p.display().to_string()));
    }
    out.push_str("];\n");
    out.push_str(&format!("pub const BASELINE_SETTINGS: &str = include_str!({:?});\n", root.join("user-settings/settings.jsonc").display().to_string()));
    out.push_str(&format!("pub const BASELINE_UI_STATE: &str = include_str!({:?});\n", root.join("user-settings/ui-state.json").display().to_string()));
    let dest = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("baseline_files.rs");
    fs::write(dest, out).expect("write baseline_files.rs");
}
