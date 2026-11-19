//! Self-update: `lusalang self-update` downloads the newest release of the
//! runtime from GitHub and replaces the installed executable — no cargo, no
//! npm, only the built-in Windows `curl.exe` and `tar.exe`.
//!
//! `self-update --check` works on every platform.  Installing is Windows-only:
//! the Windows release ships `lusa-setup.zip` and installs to
//! `%LOCALAPPDATA%\Lusa\bin`; on Linux/macOS the user replaces the binary from
//! the release tarball themselves.
//!
//! Integrity: every release publishes `SHA256SUMS`.  The downloaded zip is
//! hashed and compared with its line in that file before anything is unpacked;
//! a missing or mismatching checksum aborts the update.
//!
//! Update strategy: the running process renames its own image to
//! `lusa.exe.old` (allowed on Windows even while running), moves the freshly
//! downloaded exe to the official install path, and tries to delete the
//! backup.  If placing the new exe fails, the backup is renamed back, so a
//! failed update leaves the previous `lusa.exe` in place.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lsp::Json;

const API_URL: &str = "https://api.github.com/repos/syntax-08/lusa/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/syntax-08/lusa/releases/latest";
pub const ASSET_NAME: &str = "lusa-setup.zip";
pub const CHECKSUMS_NAME: &str = "SHA256SUMS";

/// Info about the newest GitHub release.
pub struct ReleaseInfo {
    /// tag, e.g. `v0.1.1`
    pub tag: String,
    /// tag without the leading `v`, e.g. `0.1.1`
    pub version: String,
    /// direct download URL of the `lusa-setup.zip` asset
    pub asset_url: Option<String>,
    /// direct download URL of the `SHA256SUMS` asset
    pub checksums_url: Option<String>,
}

/// The version this binary reports as itself (overridable for tests).
pub fn running_version() -> String {
    std::env::var("LUSA_SELF_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// The official install location (`%LOCALAPPDATA%\Lusa\bin`), when
/// `LOCALAPPDATA` is set.  Never falls back to a path relative to the current
/// directory.
pub fn install_dir() -> Option<PathBuf> {
    match std::env::var("LOCALAPPDATA") {
        Ok(base) if !base.is_empty() => Some(PathBuf::from(base).join("Lusa").join("bin")),
        _ => None,
    }
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

fn asset_url(release: &Json, name: &str) -> Option<String> {
    release
        .get("assets")
        .and_then(Json::as_arr)?
        .iter()
        .find(|a| a.get("name").and_then(Json::as_str) == Some(name))
        .and_then(|a| a.get("browser_download_url").and_then(Json::as_str))
        .map(str::to_string)
}

/// Fetches metadata about the latest release using `curl`.
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--retry", "3"])
        .arg(API_URL)
        .output()
        .map_err(|_| "network: could not start curl".to_string())?;
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
    Ok(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        asset_url: asset_url(&j, ASSET_NAME),
        checksums_url: asset_url(&j, CHECKSUMS_NAME),
        tag,
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
                if cfg!(windows) {
                    println!("run `lusalang self-update` to install {}", rel.tag);
                } else {
                    println!("download it from {}", RELEASES_PAGE);
                }
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
fn download(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|_| "network: could not start curl".to_string())?;
    if !status.success() {
        return Err("network: download failed".to_string());
    }
    Ok(())
}

/// Extracts a zip with the built-in `tar.exe` (bsdtar understands zip).
fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
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

// ===== SHA-256 (FIPS 180-4), std-only =====

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Lower-case hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    h.iter().map(|v| format!("{:08x}", v)).collect()
}

/// Finds the hash for `file_name` in `sha256sum`-style text
/// (`<64 hex>  <name>` or `<64 hex> *<name>` per line).
pub fn expected_sha256(sums: &str, file_name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        let name = name.trim_start_matches('*').trim_start_matches("./");
        if name == file_name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Checks `file` against its entry in the `SHA256SUMS` text.
fn verify_checksum(file: &Path, sums: &str, file_name: &str) -> Result<(), String> {
    let expected = expected_sha256(sums, file_name)
        .ok_or_else(|| format!("{} has no entry for {}", CHECKSUMS_NAME, file_name))?;
    let bytes =
        std::fs::read(file).map_err(|e| format!("cannot read {}: {}", file.display(), e))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {}: expected {}, got {} — the download is corrupt or was \
             tampered with; nothing was installed",
            file_name, expected, actual
        ));
    }
    Ok(())
}

/// Moves `new_exe` to `dest`.  When `dest` is the exe this process runs from,
/// it is first renamed to `lusa.exe.old`; if the new exe cannot be placed the
/// backup is renamed back so the install keeps working.
fn swap_in(new_exe: &Path, dest: &Path, replacing_running: bool) -> Result<(), String> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let old = dir.join("lusa.exe.old");
    if replacing_running {
        let _ = std::fs::remove_file(&old);
        std::fs::rename(dest, &old)
            .map_err(|e| format!("cannot move the running exe out of the way: {}", e))?;
    }
    // rename is atomic within the same drive; temp may live on another one
    let placed =
        std::fs::rename(new_exe, dest).or_else(|_| std::fs::copy(new_exe, dest).map(|_| ()));
    if let Err(e) = placed {
        let mut msg = format!("cannot place the new exe at {}: {}", dest.display(), e);
        if replacing_running {
            match std::fs::rename(&old, dest) {
                Ok(()) => msg.push_str(" (the previous version was restored)"),
                Err(re) => msg.push_str(&format!(
                    " — and restoring the previous version failed ({}); rename {} back to \
                     lusa.exe to recover",
                    re,
                    old.display()
                )),
            }
        }
        return Err(msg);
    }
    let _ = std::fs::remove_file(&old); // best effort; fails while still running
    Ok(())
}

/// Performs an update. Returns a human-readable success message.
pub fn update(force: bool) -> Result<String, String> {
    if !cfg!(windows) {
        return Err(format!(
            "installing updates is only automated on Windows; download the archive for your \
             system from {} (`lusalang self-update --check` works here)",
            RELEASES_PAGE
        ));
    }
    let dir =
        install_dir().ok_or("LOCALAPPDATA is not set; cannot locate the Lusa install folder")?;

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
        format!(
            "release {} has no \"{}\" asset attached",
            rel.tag, ASSET_NAME
        )
    })?;
    let sums_url = rel.checksums_url.clone().ok_or_else(|| {
        format!(
            "release {} has no \"{}\" asset, so the download cannot be verified — install it \
             manually from {}",
            rel.tag, CHECKSUMS_NAME, RELEASES_PAGE
        )
    })?;

    // download + verify + unpack in a private temp folder
    let tmp = std::env::temp_dir().join(format!("lusa-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create temp dir: {}", e))?;
    let result = (|| {
        let zip = tmp.join(ASSET_NAME);
        let sums_file = tmp.join(CHECKSUMS_NAME);
        download(&sums_url, &sums_file)?;
        let sums = std::fs::read_to_string(&sums_file)
            .map_err(|e| format!("cannot read {}: {}", CHECKSUMS_NAME, e))?;
        download(&url, &zip)?;
        verify_checksum(&zip, &sums, ASSET_NAME)?;

        let unpacked = tmp.join("unpacked");
        std::fs::create_dir_all(&unpacked).map_err(|e| format!("cannot create temp dir: {}", e))?;
        extract_zip(&zip, &unpacked)?;
        let new_exe = unpacked.join("lusa.exe");
        if !new_exe.is_file() {
            return Err(format!(
                "extracted zip does not contain lusa.exe (looked in {})",
                unpacked.display()
            ));
        }

        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
        let dest = dir.join("lusa.exe");
        let replacing_running = std::env::current_exe()
            .map(|cur| same_file(&cur, &dest))
            .unwrap_or(false);
        swap_in(&new_exe, &dest, replacing_running)?;
        Ok(dest)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    let dest = result?;
    Ok(format!(
        "updated to {} (SHA-256 verified) — {}\nrun `lusalang version` to confirm",
        rel.tag,
        dest.display()
    ))
}

/// Path equality that tolerates `\\?\` prefixes and letter case (Windows).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Removes a leftover `lusa.exe.old` from a previous update. Fails silently
/// while some process still runs from it; the next run succeeds.
pub fn clean_stale_backup() {
    if !cfg!(windows) {
        return;
    }
    if let Some(dir) = install_dir() {
        let old = dir.join("lusa.exe.old");
        if old.is_file() {
            let _ = std::fs::remove_file(&old);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_fips_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // crosses the 55/56-byte padding boundary and spans several blocks
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn checksum_lines_are_parsed_like_sha256sum() {
        let h = "a".repeat(64);
        let sums = format!(
            "{h}  lusa-linux-x86_64.tar.gz\n{}  lusa-setup.zip\n",
            "B".repeat(64)
        );
        assert_eq!(
            expected_sha256(&sums, "lusa-setup.zip"),
            Some("b".repeat(64))
        );
        assert_eq!(
            expected_sha256(&format!("{h} *lusa-setup.zip"), "lusa-setup.zip"),
            Some(h.clone())
        );
        assert_eq!(
            expected_sha256(&format!("{h}  ./lusa-setup.zip"), "lusa-setup.zip"),
            Some(h)
        );
        assert_eq!(expected_sha256(&sums, "missing.zip"), None);
        assert_eq!(
            expected_sha256("xyz  lusa-setup.zip", "lusa-setup.zip"),
            None
        );
    }

    #[test]
    fn verify_checksum_rejects_tampered_files() {
        let dir = std::env::temp_dir().join(format!("lusa_upd_verify_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lusa-setup.zip");
        std::fs::write(&file, b"abc").unwrap();
        let good = format!("{}  lusa-setup.zip\n", sha256_hex(b"abc"));
        assert!(verify_checksum(&file, &good, "lusa-setup.zip").is_ok());
        std::fs::write(&file, b"abd").unwrap();
        let err = verify_checksum(&file, &good, "lusa-setup.zip").unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(verify_checksum(&file, "", "lusa-setup.zip").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_swap_restores_the_previous_exe() {
        let dir = std::env::temp_dir().join(format!("lusa_upd_swap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("lusa.exe");
        std::fs::write(&dest, b"old build").unwrap();
        // the "new exe" does not exist, so both rename and copy fail
        let missing = dir.join("does-not-exist.exe");
        let err = swap_in(&missing, &dest, true).unwrap_err();
        assert!(err.contains("restored"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old build");
        assert!(!dir.join("lusa.exe.old").exists());

        // and a successful swap leaves only the new exe
        let new_exe = dir.join("new.exe");
        std::fs::write(&new_exe, b"new build").unwrap();
        swap_in(&new_exe, &dest, true).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new build");
        assert!(!dir.join("lusa.exe.old").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("v2.10.0") > parse_version("v2.9.9"));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("nope"), None);
    }
}
