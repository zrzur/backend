use vergen::{vergen, Config};

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Authoritative Zarzour product version (backend/ZARZOUR_VERSION).
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let version_file = manifest_dir.join("../../ZARZOUR_VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());
    let version = fs::read_to_string(&version_file)
        .unwrap_or_else(|e| panic!("missing authoritative {}: {e}", version_file.display()))
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or_else(|| panic!("{} has no version line", version_file.display()))
        .to_string();
    println!("cargo:rustc-env=ZARZOUR_PKG_VERSION={version}");

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
