use vergen::{vergen, Config};

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Authoritative Zrzur product version (backend/ZRZUR_VERSION).
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let version_file = manifest_dir.join("../../ZRZUR_VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());
    let raw = fs::read_to_string(&version_file)
        .unwrap_or_else(|e| panic!("missing authoritative {}: {e}", version_file.display()));
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let version = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or_else(|| panic!("{} has no version line", version_file.display()))
        .to_string();
    println!("cargo:rustc-env=ZRZUR_PKG_VERSION={version}");

    // Commit of this build, for the source-code link (AGPL section 13).
    // ZRZUR_GIT_COMMIT wins (Docker builds have no .git); else git; else empty.
    println!("cargo:rerun-if-env-changed=ZRZUR_GIT_COMMIT");
    let commit = std::env::var("ZRZUR_GIT_COMMIT")
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty() && !c.starts_with("__"))
        .or_else(|| {
            let out = Command::new("git").args(["rev-parse", "HEAD"]).output().ok()?;
            let c = String::from_utf8(out.stdout).ok()?.trim().to_string();
            (out.status.success() && !c.is_empty()).then_some(c)
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=ZRZUR_GIT_COMMIT={commit}");

    if let Ok(output) = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
    {
        if let Ok(git_origin) = String::from_utf8(output.stdout) {
            println!("cargo:rustc-env=GIT_ORIGIN_URL={git_origin}");
        }
    }

    vergen(Config::default()).ok();
}
