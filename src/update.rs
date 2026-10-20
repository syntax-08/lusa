//! Self-update: `lusalang self-update` downloads the newest release of the
//! runtime from GitHub and replaces the installed executable — no cargo, no
//! npm, only the built-in Windows `curl.exe` and `tar.exe`.
//!
//! Update strategy: the running process renames its own image to
//! `lusa.exe.old` (allowed on Windows even while running), writes the freshly
//! downloaded exe at the official install path, and tries to delete the
//! backup. If anything fails mid-way the old exe is still in place under the
//! `.old` name, so a broken update never bricks the install.

use std::process::Command;

use crate::lsp::Json;

const REPO: &str = "syntax-08/lusa";
const API_URL: &str = "https://api.github.com/repos/syntax-08/lusa/releases/latest";
pub const ASSET_NAME: &str = "lusa-setup.zip";

/// Info about the newest GitHub release.
pub struct ReleaseInfo {
    /// tag, e.g. `v0.1.1`
    pub tag: String,
    /// tag without the leading `v`, e.g. `0.1.1`
    pub version: String,
    /// direct download URL of the `lusa-setup.zip` asset
    pub asset_url: Option<String>,
}

/// The version this binary reports as itself (overridable for tests).
pub fn running_version() -> String {
    std::env::var("LUSA_SELF_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// The official install location (`%LOCALAPPDATA%\Lusa\bin`).
pub fn install_dir() -> std::path::PathBuf {
    match std::env::var("LOCALAPPDATA") {
        Ok(base) if !base.is_empty() => {
            std::path::PathBuf::from(base).join("Lusa").join("bin")
        }
        _ => std::path::PathBuf::from("Lusa").join("bin"),
    }
}

fn exe_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("lusa.exe")
}

/// Version triple of a `vX.Y.Z`-style tag.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let t = text.trim().trim_start_matches('v');
    let mut it = t.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next().unwrap_or("0").trim().parse().ok()?;
    Some((major, minor, patch))
}

/// Fetches metadata about the latest release using `curl.exe`.
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--retry", "3"])
        .arg(API_URL)
        .output()
        .map_err(|_| "network: could not start curl.exe".to_string())?;
    if !out.status.success() {
        return Err("network: could not reach GitHub (check your connection)".to_string());
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let j = Json::parse(&body).ok_or("invalid response from GitHub (not JSON)")?;
    let tag = j
        .get("tag_name")
        .and_then(Json::as_str)
        .ok_or("invalid response from GitHub (missing tag_name)")?
        .to_string();
    let mut asset_url: Option<String> = None;
    if let Some(assets) = j.get("assets").and_then(Json::as_arr) {
        for a in assets {
            if a.get("name").and_then(Json::as_str) == Some(ASSET_NAME) {
                asset_url = a
                    .get("browser_download_url")
                    .and_then(Json::as_str)
                    .map(str::to_string);
                break;
            }
        }
    }
    if asset_url.is_none() {
        return Err(format!(
            "release {} has no \"{}\" asset attached",
            tag, ASSET_NAME
        ));
    }
    Ok(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        tag,
        asset_url,
    })
}

/// `self-update --check`: prints the status only, never touches the exe.
pub fn check_print() -> i32 {
    match fetch_latest() {
        Ok(rel) => {
            let running = running_version();
            let status = match (parse_version(&running), parse_version(&rel.version)) {
                (Some(r), Some(l)) if r >= l => "up to date",
                (Some(_), Some(_)) => "update available",
                _ => "cannot compare versions",
            };
            println!("latest release : {}", rel.tag);
            println!("you are running: lusa {}", running);
            println!("status         : {}", status);
            if status == "update available" {
                println!("run `lusalang self-update` to install {}", rel.tag);
            }
            0
        }
        Err(e) => {
            eprintln!("lusa: self-update --check failed: {}", e);
            1
        }
    }
}

/// Download `url` into `dest` with the built-in `curl.exe`.
fn download(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|_| "network: could not start curl.exe".to_string())?;
    if !status.success() {
        return Err("network: download failed".to_string());
    }
    Ok(())
}

/// Extracts a zip with the built-in `tar.exe` (bsdtar understands zip).
fn extract_zip(zip: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let status = Command::new("tar")
        .args(["-xf"])
        .arg(zip)
        .args(["-C"])
        .arg(dest)
        .status()
        .map_err(|_| "system: could not start tar.exe".to_string())?;
    if !status.success() {
        return Err("extract failed: tar.exe could not unpack the release zip".to_string());
    }
    Ok(())
}

/// Chooses where the new exe goes: the directory of the running exe when it is
/// the official install path, the official install dir if it exists, else
/// creates (bootstraps) the official install dir.
fn target_dir() -> std::path::PathBuf {
    let official = install_dir();
    if let Ok(cur) = std::env::current_exe() {
        if cur.parent() == Some(official.as_path()) {
            return official;
        }
    }
    official
}

/// Performs an update. Returns a human-readable success message.
pub fn update(force: bool) -> Result<String, String> {
    let rel = fetch_latest()?;
    let running = running_version();
    let (rv, lv) = (parse_version(&running), parse_version(&rel.version));
    if !force {
        if let (Some(r), Some(l)) = (rv, lv) {
            if r == l {
                return Ok(format!(
                    "already up to date (lusa {} / {}) — nothing to do",
                    running, rel.tag
                ));
            }
            if r >= l {
                return Ok(format!(
                    "your build ({}) is newer than the latest release ({}) — skipping (--force to install anyway)",
                    running, rel.tag
                ));
            }
        }
    }
    let url = rel.asset_url.clone().ok_or_else(|| {
        format!("release {} has no \"{}\" asset attached", rel.tag, ASSET_NAME)
    })?;

    // download + verify + unpack in a private temp folder
    let tmp = std::env::temp_dir().join(format!("lusa-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create temp dir: {}", e))?;
    let zip = tmp.join(ASSET_NAME);
    download(&url, &zip)?;
    extract_zip(&zip, &tmp)?;

    let new_exe = tmp.join("lusa.exe");
    if !new_exe.is_file() {
        return Err(format!(
            "extracted zip does not contain lusa.exe (looked in {})",
            tmp.display()
        ));
    }

    let dir = target_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let dest = exe_in(&dir);
    let cur = std::env::current_exe().ok();

    // if we are replacing the file our process is running from, move it aside
    if cur.as_deref() == Some(dest.as_path()) {
        let old = dir.join("lusa.exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&dest, &old)
            .map_err(|e| format!("cannot move the running exe out of the way: {}", e))?;
    }
    // move the new exe into place (rename is atomic within the same drive)
    std::fs::rename(&new_exe, &dest).or_with(|| {
        std::fs::copy(&new_exe, &dest)
            .map(|_| ())
            .map_err(|e| format!("cannot place the new exe at {}: {}", dest.display(), e))
    })?;

    let _ = std::fs::remove_file(dir.join("lusa.exe.old")); // best effort cleanup
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(format!(
        "updated to {} — {}\nrun `lusalang version` to confirm",
        rel.tag,
        dest.display()
    ))
}

/// helper: run `backup` when the main call fails, propagating its error
trait OrWithSelf {
    fn or_with<F: FnOnce() -> Result<(), String>>(self, backup: F) -> Result<(), String>;
}

impl OrWithSelf for Result<(), String> {
    fn or_with<F: FnOnce() -> Result<(), String>>(self, backup: F) -> Result<(), String> {
        match self {
            Ok(()) => Ok(()),
            Err(_) => backup(),
        }
    }
}

impl OrWithSelf for std::io::Result<()> {
    fn or_with<F: FnOnce() -> Result<(), String>>(self, backup: F) -> Result<(), String> {
        match self {
            Ok(()) => Ok(()),
            Err(_) => backup(),
        }
    }
}

/// Removes a leftover `lusa.exe.old` from a previous update. Fails silently
/// while some process still runs from it; the next run succeeds.
pub fn clean_stale_backup() {
    let old = install_dir().join("lusa.exe.old");
    if old.is_file() {
        let _ = std::fs::remove_file(&old);
    }
}

/// `self-update [--check] [--force]` CLI entry.
pub fn cli(rest: &[String]) -> i32 {
    let check_only = rest.iter().any(|a| a == "--check");
    let force = rest.iter().any(|a| a == "--force");
    if check_only {
        return check_print();
    }
    match update(force) {
        Ok(msg) => {
            println!("{}", msg);
            0
        }
        Err(e) => {
            eprintln!("lusa: self-update failed: {}", e);
            1
        }
    }
}
