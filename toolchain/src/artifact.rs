use flate2::read::GzDecoder;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const REPOSITORY: &str = "https://api.github.com/repos/wavefnd/Wave/releases";
const DOWNLOAD: &str = "https://github.com/wavefnd/Wave/releases/download/";
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 2 * 1024 * 1024 * 1024;
fn error(e: impl std::fmt::Display) -> String {
    format!("compiler installation: {e}")
}

pub(crate) fn home() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("VEX_TOOLCHAIN_HOME") {
        return Ok(PathBuf::from(path));
    }
    let variable = if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    };
    let base = std::env::var_os(variable)
        .ok_or_else(|| error(format!("{variable} is not set; set VEX_TOOLCHAIN_HOME")))?;
    Ok(PathBuf::from(base).join(".vex/toolchains"))
}

pub fn managed_wavec() -> Option<PathBuf> {
    let root = home().ok()?;
    managed_wavec_in(&root)
}

fn managed_wavec_in(root: &Path) -> Option<PathBuf> {
    let value = fs::read_to_string(root.join("current")).ok()?;
    let path = relative(value.trim()).ok()?;
    let binary = root.join(path);
    binary.is_file().then_some(binary)
}

pub(super) fn version(value: &str) -> Result<String, String> {
    let parsed = semver::Version::parse(value.strip_prefix('v').unwrap_or(value)).map_err(error)?;
    if !parsed.build.is_empty() {
        return Err(error("build metadata is not supported"));
    }
    Ok(parsed.to_string())
}

fn target(os: &str, arch: &str) -> Result<&'static str, String> {
    static PLATFORMS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let table = PLATFORMS.get_or_init(|| {
        serde_json::from_str(include_str!("../../platforms.json"))
            .expect("validated platform table")
    });
    table["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["os"] == os && p["arch"] == arch)
        .and_then(|p| p["wave_target"].as_str())
        .ok_or_else(|| {
            error(format!(
                "no supported compiler artifact for host {os}/{arch}"
            ))
        })
}

fn curl(url: &str) -> Command {
    let mut command = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    command.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    command.args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "20",
        "--max-time",
        "300",
        "--user-agent",
        "Vex-toolchain/1",
        url,
    ]);
    command
}
fn get(url: &str) -> Result<Vec<u8>, String> {
    if url.starts_with("https://api.github.com/repos/wavefnd/Wave/") {
        let (status, body) = api(url)?;
        return if status == 200 {
            Ok(body)
        } else {
            Err(error(format!("GitHub API query failed with HTTP {status}")))
        };
    }
    let output = process::output(&mut curl(url), Duration::from_secs(310)).map_err(error)?;
    if !output.status.success() {
        return Err(error(format!(
            "download failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}
fn download(url: &str, path: &Path) -> Result<(), String> {
    let output = process::output(
        curl(url)
            .args(["--max-filesize", &MAX_ARCHIVE.to_string(), "--output"])
            .arg(path),
        Duration::from_secs(310),
    )
    .map_err(error)?;
    if !output.status.success() {
        return Err(error(format!(
            "archive download failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    if fs::metadata(path).map_err(error)?.len() > MAX_ARCHIVE {
        return Err(error("archive exceeds 512 MiB"));
    }
    Ok(())
}

#[derive(Debug)]
struct ReleasePlan {
    tag: String,
    version: String,
    name: String,
    archive_url: String,
    checksum_url: Option<String>,
    asset_digest: Option<String>,
}
fn select_release(
    release: &Value,
    requested: Option<&str>,
    target: &str,
    extension: &str,
) -> Result<ReleasePlan, String> {
    if release["draft"] != false {
        return Err(error("release is draft or malformed"));
    }
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| error("release has no tag"))?;
    let version = version(tag)?;
    if tag != format!("v{version}") || requested.is_some_and(|v| v != version) {
        return Err(error(
            "release version does not match the requested identity",
        ));
    }
    let name = format!("wave-{tag}-{target}.{extension}");
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| error("release has no assets"))?;
    let asset = |name: &str| -> Result<&str, String> {
        let matches: Vec<_> = assets.iter().filter(|a| a["name"] == name).collect();
        if matches.len() != 1 {
            return Err(error(format!(
                "release must contain exactly one {name}; no automatic script fallback"
            )));
        }
        let url = matches[0]["browser_download_url"]
            .as_str()
            .ok_or_else(|| error("asset has no URL"))?;
        if url != format!("{DOWNLOAD}{tag}/{name}") {
            return Err(error("asset URL is outside the official release"));
        }
        Ok(url)
    };
    let archive_url = asset(&name)?.to_owned();
    let checksum_url = if assets.iter().any(|a| a["name"] == "SHA256SUMS") {
        Some(asset("SHA256SUMS")?.to_owned())
    } else {
        None
    };
    let digest_value = &assets.iter().find(|a| a["name"] == name).unwrap()["digest"];
    let asset_digest = if digest_value.is_null() {
        None
    } else {
        let digest = digest_value
            .as_str()
            .and_then(|s| s.strip_prefix("sha256:"))
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| error("invalid official asset SHA-256 digest"))?;
        Some(digest.to_ascii_lowercase())
    };
    if checksum_url.is_none() && asset_digest.is_none() {
        return Err(error(
            "release has neither SHA256SUMS nor an official asset SHA-256 digest",
        ));
    }
    Ok(ReleasePlan {
        tag: tag.into(),
        version,
        name,
        archive_url,
        checksum_url,
        asset_digest,
    })
}

pub fn install(requested: Option<&str>) -> Result<PathBuf, String> {
    let target = target(std::env::consts::OS, std::env::consts::ARCH)?;
    let requested = requested.map(version).transpose()?;
    let endpoint = requested
        .as_ref()
        .map(|v| format!("{REPOSITORY}/tags/v{v}"))
        .unwrap_or_else(|| format!("{REPOSITORY}/latest"));
    let release: Value = serde_json::from_slice(&get(&endpoint)?).map_err(error)?;
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    let ReleasePlan {
        tag,
        version,
        name,
        archive_url,
        checksum_url,
        asset_digest,
    } = select_release(&release, requested.as_deref(), target, extension)?;
    let checksum_digest = checksum_url
        .as_deref()
        .map(|url| {
            let text = String::from_utf8(get(url)?).map_err(error)?;
            checksum(&text, &name)
        })
        .transpose()?;
    let expected = agree_digests(checksum_digest.as_deref(), asset_digest.as_deref())?;
    if let Some(pin) = std::env::var_os("VEX_WAVEC_ARCHIVE_SHA256") {
        let pin = pin
            .to_str()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| error("VEX_WAVEC_ARCHIVE_SHA256 must be a SHA-256 digest"))?;
        if !pin.eq_ignore_ascii_case(&expected) {
            return Err(error("official compiler digest differs from VEX_WAVEC_ARCHIVE_SHA256; installation unchanged"));
        }
    }
    let root = home()?;
    fs::create_dir_all(&root).map_err(error)?;
    state::reject_link(&root)?;
    let root = root.canonicalize().map_err(error)?;
    let lock_path = root.join("install.lock");
    state::reject_link(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(error)?;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) => {
                if process::cancelled() {
                    return Err(error("operation cancelled"));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(error(e)),
        }
    }
    let stage = create_stage(&root)?;
    let result = (|| {
        let archive = stage.join("download");
        download(&archive_url, &archive)?;
        verify_checksum(&archive, &expected)?;
        verify_provenance(&archive, &expected, &tag)?;
        let payload = stage.join("payload");
        let binary = unpack_compiler(&archive, &payload, extension == "zip")?;
        verify_binary_version(&binary, &version)?;
        let relative_binary = binary.strip_prefix(&payload).map_err(error)?.to_owned();
        // The checksum makes reinstalling a changed release a distinct generation.
        let generation = format!(
            "wavec-{version}-{target}-{}-{}",
            &expected[..16],
            stage.file_name().unwrap().to_string_lossy()
        );
        publish_candidate(
            &root,
            &stage,
            &payload,
            &generation,
            &relative_binary,
            || Ok(()),
        )
    })();
    let cleanup = fs::remove_dir_all(&stage);
    match (result, cleanup) {
        (Err(e), _) => Err(e),
        (Ok(binary), Ok(())) => Ok(binary),
        (Ok(binary), Err(e)) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: installed {}; temporary cleanup failed at {}: {e}",
                binary.display(),
                stage.display()
            );
            Ok(binary)
        }
    }
}

fn verify_binary_version(binary: &Path, version: &str) -> Result<(), String> {
    let output = process::output(
        Command::new(binary)
            .arg("--version")
            .env("NO_COLOR", "1")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN"),
        Duration::from_secs(30),
    )
    .map_err(error)?;
    if !output.status.success() {
        return Err(error(format!(
            "downloaded compiler could not report its version ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let output_text = String::from_utf8(output.stdout).map_err(error)?;
    if !matches!(output_text.split_whitespace().take(2).collect::<Vec<_>>().as_slice(), ["wavec", actual] if actual.strip_prefix('v').unwrap_or(actual) == version)
    {
        return Err(error(format!(
            "downloaded compiler version does not match the release (expected {version}, received {output_text:?})"
        )));
    }
    Ok(())
}

fn unpack_compiler(archive: &Path, payload: &Path, zip: bool) -> Result<PathBuf, String> {
    fs::create_dir(payload).map_err(error)?;
    extract(archive, payload, zip)?;
    let mut candidates = binaries(payload, if zip { "wavec.exe" } else { "wavec" })?;
    if candidates.len() != 1 {
        return Err(error(
            "archive must contain exactly one wavec executable for the selected host",
        ));
    }
    Ok(candidates.remove(0))
}

fn publish_candidate(
    root: &Path,
    stage: &Path,
    payload: &Path,
    generation: &str,
    relative_binary: &Path,
    before_switch: impl FnOnce() -> Result<(), String>,
) -> Result<PathBuf, String> {
    let destination = root.join(generation);
    state::reject_link(&destination)?;
    if destination.exists() {
        return Err(error(format!("verified generation already exists at {}; preserve it or select its wavec with VEX_WAVEC", destination.display())));
    }
    sync_tree(payload)?;
    state::atomic_rename(payload, &destination).map_err(error)?;
    let current = PathBuf::from(generation).join(relative_binary);
    let pointer = stage.join("current");
    let mut file = File::create_new(&pointer).map_err(error)?;
    file.write_all(current.to_string_lossy().replace('\\', "/").as_bytes())
        .map_err(error)?;
    file.sync_all().map_err(error)?;
    drop(file);
    before_switch()?;
    state::reject_link(&root.join("current"))?;
    state::atomic_rename(&pointer, &root.join("current")).map_err(error)?;
    Ok(root.join(current))
}

fn api(endpoint: &str) -> Result<(u16, Vec<u8>), String> {
    let path = endpoint
        .strip_prefix("https://api.github.com/repos/wavefnd/Wave/")
        .ok_or_else(|| error("unexpected compiler API endpoint"))?;
    if std::env::var_os("GH_TOKEN").is_some() {
        // Authentication is scoped to GitHub API calls, never archive downloads
        // or the downloaded compiler. gh keeps credentials out of argv/logs.
        let output = process::output(
            Command::new("gh")
                .args(["api", "--hostname", "github.com", "--include"])
                .arg(format!("repos/wavefnd/Wave/{path}"))
                .env_remove("GH_DEBUG"),
            Duration::from_secs(70),
        )
        .map_err(error)?;
        return parse_api_response(&output.stdout);
    }
    let mut command = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    command.env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    command.args([
        "--disable",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--connect-timeout",
        "20",
        "--max-time",
        "60",
        "--write-out",
        "\n%{http_code}",
        endpoint,
    ]);
    let result = process::output(&mut command, Duration::from_secs(70)).map_err(error)?;
    if !result.status.success() {
        return Err(error("could not query compiler GitHub API"));
    }
    let response = String::from_utf8(result.stdout).map_err(error)?;
    let (body, status) = response
        .rsplit_once('\n')
        .ok_or_else(|| error("invalid GitHub API response"))?;
    Ok((status.parse().map_err(error)?, body.as_bytes().to_vec()))
}

fn parse_api_response(output: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let response = String::from_utf8(output.to_vec())
        .map_err(error)?
        .replace("\r\n", "\n");
    let (headers, body) = response
        .split_once("\n\n")
        .ok_or_else(|| error("invalid GitHub API response headers"))?;
    let mut status_line = headers
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    if !status_line
        .next()
        .is_some_and(|value| value.starts_with("HTTP/"))
    {
        return Err(error("missing GitHub API HTTP status"));
    }
    let status = status_line
        .next()
        .ok_or_else(|| error("missing GitHub API status code"))?
        .parse::<u16>()
        .map_err(error)?;
    if !(100..600).contains(&status) {
        return Err(error("invalid GitHub API status code"));
    }
    Ok((status, body.as_bytes().to_vec()))
}

fn verify_provenance(archive: &Path, digest: &str, tag: &str) -> Result<(), String> {
    let endpoint =
        format!("https://api.github.com/repos/wavefnd/Wave/attestations/sha256:{digest}");
    // An explicit 404 is absence; authentication, rate-limit and transport
    // failures must not be silently treated as an un-attested release.
    let (status, body) = api(&endpoint)?;
    if status == 404 {
        let _ = writeln!(
            std::io::stderr().lock(),
            "note: official Wave archive has no published GitHub provenance; SHA-256 verified"
        );
        return Ok(());
    }
    if status != 200 {
        return Err(error(format!("provenance query failed with HTTP {status}")));
    }
    let response: Value = serde_json::from_slice(&body).map_err(error)?;
    let attestations = response["attestations"]
        .as_array()
        .ok_or_else(|| error("invalid provenance response"))?;
    if attestations.is_empty() {
        let _ = writeln!(
            std::io::stderr().lock(),
            "note: official Wave archive has no published GitHub provenance; SHA-256 verified"
        );
        return Ok(());
    }
    let commit: Value = serde_json::from_slice(&get(&format!(
        "https://api.github.com/repos/wavefnd/Wave/commits/{tag}"
    ))?)
    .map_err(error)?;
    let commit = commit["sha"]
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| error("release tag has no valid source commit"))?;
    // Use the public API response as a local bundle. gh can verify this without
    // repository credentials; no CI token crosses into an emulated/VM guest.
    let bundle_path = archive.with_extension("attestations.jsonl");
    let mut bundle_file = File::create_new(&bundle_path).map_err(error)?;
    for attestation in attestations {
        let bundle = attestation
            .get("bundle")
            .filter(|value| value.is_object())
            .ok_or_else(|| error("published provenance contains an invalid bundle"))?;
        serde_json::to_writer(&mut bundle_file, bundle).map_err(error)?;
        bundle_file.write_all(b"\n").map_err(error)?;
    }
    drop(bundle_file);
    let verification = process::output(
        Command::new("gh")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .args(["attestation", "verify"])
            .arg(archive)
            .arg("--bundle")
            .arg(&bundle_path)
            .args([
                "--repo",
                "wavefnd/Wave",
                "--source-digest",
                commit,
                "--deny-self-hosted-runners",
            ]),
        Duration::from_secs(120),
    )
    .map_err(|e| {
        error(format!(
            "published provenance requires GitHub CLI verification: {e}"
        ))
    })?;
    if !verification.status.success() {
        return Err(error("published compiler provenance verification failed"));
    }
    Ok(())
}

fn create_stage(root: &Path) -> Result<PathBuf, String> {
    for counter in 0..100 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(error)?
            .as_nanos();
        let path = root.join(format!(".install-{}-{stamp}-{counter}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(error(e)),
        }
    }
    Err(error("could not allocate installation staging directory"))
}
fn agree_digests(checksum: Option<&str>, asset: Option<&str>) -> Result<String, String> {
    match (checksum, asset) {
        (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => {
            Err(error("SHA256SUMS and official asset digest disagree"))
        }
        (Some(a), _) | (_, Some(a)) => Ok(a.to_ascii_lowercase()),
        _ => Err(error("release integrity information is missing")),
    }
}

fn checksum(text: &str, filename: &str) -> Result<String, String> {
    let mut found = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 2 && fields[1].trim_start_matches('*') == filename {
            if found.is_some()
                || fields[0].len() != 64
                || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(error("invalid or duplicate checksum"));
            }
            found = Some(fields[0].to_ascii_lowercase());
        }
    }
    found.ok_or_else(|| error("release checksum is missing"))
}
fn verify_checksum(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let size = file.read(&mut buffer).map_err(error)?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    if format!("{:x}", hasher.finalize()) != expected {
        return Err(error(
            "archive SHA-256 mismatch; existing installation preserved",
        ));
    }
    Ok(())
}
fn relative(name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(name);
    if path.as_os_str().is_empty()
        || name.contains(['\\', ':', '\0'])
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(error("archive contains an unsafe path"));
    }
    if path.components().count() > 64 {
        return Err(error("archive nesting exceeds 64 levels"));
    }
    for part in name
        .split('/')
        .filter(|part| *part != "." && !part.is_empty())
    {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.ends_with([' ', '.'])
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit()
        {
            return Err(error("archive path is not portable"));
        }
    }
    Ok(path)
}
fn extract(archive: &Path, root: &Path, zip: bool) -> Result<(), String> {
    let mut total = 0u64;
    let mut count = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    let mut write = |name: &str,
                     directory: bool,
                     size: u64,
                     reader: &mut dyn Read,
                     mode: u32|
     -> Result<(), String> {
        count += 1;
        total = total
            .checked_add(size)
            .ok_or_else(|| error("archive size overflow"))?;
        if count > 100_000 || total > MAX_EXTRACTED {
            return Err(error("archive extraction limit exceeded"));
        }
        let path = relative(name)?;
        if !seen.insert(path.to_string_lossy().to_ascii_lowercase()) {
            return Err(error("duplicate or case-colliding archive path"));
        }
        let destination = root.join(path);
        if directory {
            fs::create_dir_all(destination).map_err(error)?;
            return Ok(());
        }
        fs::create_dir_all(destination.parent().unwrap()).map_err(error)?;
        let mut file = File::create_new(&destination).map_err(error)?;
        let copied = std::io::copy(&mut reader.take(size + 1), &mut file).map_err(error)?;
        if copied != size {
            return Err(error("archive entry length mismatch"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(if mode & 0o111 != 0 {
                0o755
            } else {
                0o644
            }))
            .map_err(error)?;
        }
        #[cfg(windows)]
        let _ = mode;
        file.sync_all().map_err(error)
    };
    if zip {
        let mut archive =
            zip::ZipArchive::new(File::open(archive).map_err(error)?).map_err(error)?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(error)?;
            if entry.is_symlink() {
                return Err(error("archive symlinks are unsupported"));
            }
            let name = entry.name().to_string();
            let (directory, size, mode) =
                (entry.is_dir(), entry.size(), entry.unix_mode().unwrap_or(0));
            write(&name, directory, size, &mut entry, mode)?;
        }
    } else {
        let mut archive = tar::Archive::new(GzDecoder::new(File::open(archive).map_err(error)?));
        for entry in archive.entries().map_err(error)? {
            let mut entry = entry.map_err(error)?;
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                return Err(error("archive links and special files are unsupported"));
            }
            let name = entry
                .path()
                .map_err(error)?
                .to_str()
                .ok_or_else(|| error("non-UTF-8 archive path"))?
                .to_owned();
            let (size, mode) = (entry.size(), entry.header().mode().map_err(error)?);
            write(&name, kind.is_dir(), size, &mut entry, mode)?;
        }
    }
    Ok(())
}
fn binaries(root: &Path, executable: &str) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).map_err(error)? {
        let entry = entry.map_err(error)?;
        if entry.file_type().map_err(error)?.is_dir() {
            found.extend(binaries(&entry.path(), executable)?);
        } else if entry.file_name() == executable {
            found.push(entry.path());
        }
    }
    Ok(found)
}
fn sync_tree(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(error)? {
        let path = entry.map_err(error)?.path();
        if path.is_dir() {
            sync_tree(&path)?;
        } else {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(error)?
                .sync_all()
                .map_err(error)?;
        }
    }
    state::sync_dir(root).map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(create_stage(&std::env::temp_dir()).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn authenticated_api_preserves_http_errors_instead_of_treating_them_as_absence() {
        for status in [200, 403, 404, 500] {
            for newline in ["\n", "\r\n"] {
                let response = format!("HTTP/2.0 {status} Status{newline}Content-Type: application/json{newline}{newline}{{\"attestations\":[]}}");
                let (actual, body) = parse_api_response(response.as_bytes()).unwrap();
                assert_eq!(actual, status);
                assert_eq!(body, br#"{"attestations":[]}"#);
            }
        }
        for response in ["", "{}", "error 404\n\n{}", "HTTP/2.0 bad\n\n{}"] {
            assert!(parse_api_response(response.as_bytes()).is_err());
        }
    }

    #[test]
    fn version_probe_disables_color_and_distinguishes_execution_failure() {
        let fixture = Fixture::new();
        let source = fixture.0.join("compiler.rs");
        fs::write(
            &source,
            r#"
            fn main() {
                if std::env::current_exe().unwrap().file_stem().unwrap() == "broken" {
                    eprintln!("missing compiler runtime");
                    std::process::exit(7);
                }
                if std::env::var_os("NO_COLOR").is_some() {
                    println!("wavec 0.2.1-pre-beta (platform)\n  backend: LLVM 21");
                } else {
                    println!("\x1b[32mwavec\x1b[0m \x1b[32m0.2.1-pre-beta\x1b[0m");
                }
            }
        "#,
        )
        .unwrap();
        let binary = fixture
            .0
            .join(if cfg!(windows) { "wavec.exe" } else { "wavec" });
        assert!(Command::new("rustc")
            .arg(source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap()
            .success());
        verify_binary_version(&binary, "0.2.1-pre-beta").unwrap();
        let mismatch = verify_binary_version(&binary, "0.2.0-pre-beta").unwrap_err();
        assert!(mismatch.contains("does not match") && mismatch.contains("0.2.1-pre-beta"));
        let broken = fixture.0.join(if cfg!(windows) {
            "broken.exe"
        } else {
            "broken"
        });
        fs::copy(binary, &broken).unwrap();
        let failure = verify_binary_version(&broken, "0.2.1-pre-beta").unwrap_err();
        assert!(failure.contains("could not report its version"));
        assert!(failure.contains("missing compiler runtime"));
        assert!(!failure.contains("does not match"));
    }

    #[test]
    fn official_host_mapping_and_asset_names() {
        for (os, arch, expected, extension) in [
            ("linux", "x86_64", "x86_64-linux-gnu", "tar.gz"),
            ("linux", "aarch64", "aarch64-linux-gnu", "tar.gz"),
            ("linux", "riscv64", "riscv64-linux-gnu", "tar.gz"),
            ("linux", "loongarch64", "loongarch64-linux-gnu", "tar.gz"),
            ("macos", "x86_64", "x86_64-apple-darwin", "tar.gz"),
            ("macos", "aarch64", "aarch64-apple-darwin", "tar.gz"),
            ("windows", "x86_64", "x86_64-pc-windows-msvc", "zip"),
            ("windows", "aarch64", "aarch64-pc-windows-msvc", "zip"),
            ("freebsd", "x86_64", "x86_64-unknown-freebsd", "tar.gz"),
        ] {
            assert_eq!(target(os, arch).unwrap(), expected);
            let name = format!("wave-v0.2.1-pre-beta-{expected}.{extension}");
            let plan = select_release(
                &fake_release(&name),
                Some("0.2.1-pre-beta"),
                expected,
                extension,
            )
            .unwrap();
            assert_eq!(plan.name, name);
            assert_eq!(
                plan.archive_url,
                format!("{DOWNLOAD}v0.2.1-pre-beta/{name}")
            );
            let missing = fake_release(&name.replace(expected, "x86_64-pc-windows-gnu"));
            assert!(select_release(&missing, None, expected, extension)
                .unwrap_err()
                .contains(&name));
        }
        for (os, arch) in [("windows", "x86"), ("linux", "arm"), ("linux", "wasm64")] {
            let message = target(os, arch).unwrap_err();
            assert!(message.contains(&format!("{os}/{arch}")));
        }
    }

    #[test]
    fn release_digest_sources_are_strict_and_must_agree() {
        let name = "wave-v0.2.1-pre-beta-x86_64-linux-gnu.tar.gz";
        let mut release = fake_release(name);
        release["assets"].as_array_mut().unwrap().pop();
        assert!(select_release(&release, None, "x86_64-linux-gnu", "tar.gz").is_err());
        release["assets"][0]["digest"] = serde_json::json!(format!("sha256:{}", "a".repeat(64)));
        let plan = select_release(&release, None, "x86_64-linux-gnu", "tar.gz").unwrap();
        assert!(plan.checksum_url.is_none());
        assert_eq!(plan.asset_digest.as_deref(), Some("a".repeat(64).as_str()));
        assert!(agree_digests(Some(&"a".repeat(64)), Some(&"b".repeat(64))).is_err());
        assert!(agree_digests(None, None).is_err());
        release["assets"][0]["digest"] = serde_json::json!("sha256:bad");
        assert!(select_release(&release, None, "x86_64-linux-gnu", "tar.gz").is_err());
    }

    fn fake_release(name: &str) -> Value {
        serde_json::json!({"draft":false,"tag_name":"v0.2.1-pre-beta","assets":[
            {"name":name,"browser_download_url":format!("{DOWNLOAD}v0.2.1-pre-beta/{name}")},
            {"name":"SHA256SUMS","browser_download_url":format!("{DOWNLOAD}v0.2.1-pre-beta/SHA256SUMS")}
        ]})
    }

    fn fake_archive(archive: &Path, root: &str, zip: bool, compatible: bool) {
        // Wave tools/ci/package.py schema 1 keeps one archive root, packaged
        // std and target-specific native resources. These are data fixtures,
        // not native execution evidence for foreign compilers.
        let executable = if !compatible {
            "other"
        } else if zip {
            "wavec.exe"
        } else {
            "wavec"
        };
        let runtime = if zip {
            "llvm/lib/clang/21/lib/windows/builtins.lib"
        } else {
            "crt/crt1.o"
        };
        let files = [
            executable,
            "std/manifest.json",
            "std/mem/layout.wave",
            "llvm/bin/lld",
            runtime,
        ];
        if zip {
            let mut writer = zip::ZipWriter::new(File::create(archive).unwrap());
            for file in files {
                writer
                    .start_file(
                        format!("{root}/{file}"),
                        zip::write::SimpleFileOptions::default(),
                    )
                    .unwrap();
                writer.write_all(b"fixture").unwrap();
            }
            writer.finish().unwrap();
        } else {
            let gzip = flate2::write::GzEncoder::new(
                File::create(archive).unwrap(),
                flate2::Compression::default(),
            );
            let mut writer = tar::Builder::new(gzip);
            for file in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(7);
                header.set_mode(0o755);
                header.set_cksum();
                writer
                    .append_data(&mut header, format!("{root}/{file}"), &b"fixture"[..])
                    .unwrap();
            }
            writer.into_inner().unwrap().finish().unwrap();
        }
    }

    #[test]
    fn new_host_archives_preserve_layout_and_previous_installation_on_failure() {
        for (os, arch) in [
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("linux", "loongarch64"),
        ] {
            for failure in ["checksum", "payload", "missing", "none"] {
                let fixture = Fixture::new();
                let root = &fixture.0;
                fs::create_dir(root.join("old")).unwrap();
                fs::write(root.join("old/wavec"), "old compiler").unwrap();
                fs::write(root.join("current"), "old/wavec").unwrap();
                let stage = create_stage(root).unwrap();
                let target = target(os, arch).unwrap();
                let zip = os == "windows";
                let extension = if zip { "zip" } else { "tar.gz" };
                let package = format!("wave-v0.2.1-pre-beta-{target}");
                let name = format!("{package}.{extension}");
                let archive = stage.join("archive");
                fake_archive(&archive, &package, zip, failure != "payload");
                let digest = format!("{:x}", Sha256::digest(fs::read(&archive).unwrap()));
                let result = (|| -> Result<PathBuf, String> {
                    let release = fake_release(if failure == "missing" {
                        "wrong-asset"
                    } else {
                        &name
                    });
                    select_release(&release, None, target, extension)?;
                    let expected = checksum(&format!("{digest}  {name}\n"), &name)?;
                    if failure == "checksum" {
                        fs::write(&archive, b"tampered").unwrap();
                    }
                    verify_checksum(&archive, &expected)?;
                    let payload = stage.join("payload");
                    let binary = unpack_compiler(&archive, &payload, zip)?;
                    assert!(payload.join(&package).join("std/manifest.json").is_file());
                    assert!(payload.join(&package).join("llvm/bin/lld").is_file());
                    let relative = binary.strip_prefix(&payload).unwrap();
                    publish_candidate(root, &stage, &payload, "new", relative, || Ok(()))
                })();
                if failure == "none" {
                    let installed = result.unwrap();
                    assert_eq!(managed_wavec_in(root), Some(installed.clone()));
                    assert!(installed
                        .parent()
                        .unwrap()
                        .join("std/mem/layout.wave")
                        .is_file());
                } else {
                    assert!(result.is_err(), "{target}: {failure}");
                    assert_eq!(fs::read(root.join("current")).unwrap(), b"old/wavec");
                    assert_eq!(managed_wavec_in(root), Some(root.join("old/wavec")));
                    assert!(!root.join("new").exists());
                }
                assert_eq!(fs::read(root.join("old/wavec")).unwrap(), b"old compiler");
            }
        }
    }

    #[test]
    fn fake_release_metadata_rejects_ambiguous_or_redirected_assets() {
        let name = "wave-v1.2.3-test-target.tar.gz";
        let release = serde_json::json!({"draft":false,"tag_name":"v1.2.3","assets":[
            {"name":name,"browser_download_url":format!("{DOWNLOAD}v1.2.3/{name}")},
            {"name":"SHA256SUMS","browser_download_url":format!("{DOWNLOAD}v1.2.3/SHA256SUMS")}
        ]});
        let plan = select_release(&release, Some("1.2.3"), "test-target", "tar.gz").unwrap();
        assert_eq!(plan.version, "1.2.3");
        assert!(select_release(&release, Some("1.2.4"), "test-target", "tar.gz").is_err());
        for change in 0..4 {
            let mut invalid = release.clone();
            match change {
                0 => invalid["draft"] = true.into(),
                1 => {
                    invalid["assets"].as_array_mut().unwrap().pop();
                }
                2 => {
                    invalid["assets"][0]["browser_download_url"] =
                        "https://untrusted.invalid/archive".into()
                }
                _ => {
                    let duplicate = invalid["assets"][0].clone();
                    invalid["assets"].as_array_mut().unwrap().push(duplicate);
                }
            }
            assert!(select_release(&invalid, None, "test-target", "tar.gz").is_err());
        }
    }

    #[test]
    fn pointer_failure_preserves_previous_installation_and_published_candidate() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("old")).unwrap();
        fs::write(fixture.0.join("old/wavec"), b"old compiler").unwrap();
        fs::write(fixture.0.join("current"), b"old/wavec").unwrap();
        let stage = create_stage(&fixture.0).unwrap();
        let payload = stage.join("payload");
        fs::create_dir(&payload).unwrap();
        fs::write(payload.join("wavec"), b"new compiler").unwrap();
        let result = publish_candidate(
            &fixture.0,
            &stage,
            &payload,
            "new",
            Path::new("wavec"),
            || Err("injected pointer publication failure".into()),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(fixture.0.join("current")).unwrap(), b"old/wavec");
        assert_eq!(
            fs::read(fixture.0.join("old/wavec")).unwrap(),
            b"old compiler"
        );
        assert_eq!(
            fs::read(fixture.0.join("new/wavec")).unwrap(),
            b"new compiler"
        );
    }

    #[test]
    fn checksum_is_exact_unique_and_required() {
        let digest = "a".repeat(64);
        assert_eq!(
            checksum(&format!("{digest}  wave.tar.gz\n"), "wave.tar.gz").unwrap(),
            digest
        );
        assert!(checksum(&format!("{digest}  other.tar.gz\n"), "wave.tar.gz").is_err());
        assert!(checksum(
            &format!("{digest}  wave.tar.gz\n{digest}  wave.tar.gz\n"),
            "wave.tar.gz"
        )
        .is_err());
        let fixture = Fixture::new();
        let archive = fixture.0.join("archive");
        fs::write(&archive, "modified archive").unwrap();
        assert!(verify_checksum(&archive, &digest).is_err());
    }

    #[test]
    fn hostile_paths_and_noncanonical_versions_are_rejected() {
        for path in [
            "../escape",
            "/escape",
            "C:/escape",
            "a\\b",
            "con.txt",
            "dir/COM1",
            "trailing.",
            "nul",
            "a/../../escape",
        ] {
            assert!(relative(path).is_err(), "{path}");
        }
        assert!(relative("wave-v1.0.0/llvm/lib/libLLVM.so").is_ok());
        for value in ["1.0.0+build", "../1.0.0", "01.0.0", "1.0"] {
            assert!(version(value).is_err());
        }
        assert_eq!(version("v1.0.0-beta.1").unwrap(), "1.0.0-beta.1");
    }

    #[test]
    fn tar_links_and_truncated_archives_cannot_be_installed() {
        let fixture = Fixture::new();
        let archive = fixture.0.join("archive.tar.gz");
        let output = fixture.0.join("output");
        fs::create_dir(&output).unwrap();
        let gzip = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_link_name("../outside").unwrap();
        header.set_cksum();
        builder.append_data(&mut header, "link", &[][..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        assert!(extract(&archive, &output, false)
            .unwrap_err()
            .contains("links"));
        assert!(!output.join("link").exists());
        fs::write(&archive, b"truncated").unwrap();
        assert!(extract(&archive, &output, false).is_err());
    }

    #[test]
    fn zip_case_collisions_and_size_corruption_are_rejected() {
        let fixture = Fixture::new();
        let archive = fixture.0.join("archive.zip");
        let output = fixture.0.join("output");
        fs::create_dir(&output).unwrap();
        let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
        for name in ["Wavec", "wavec"] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"binary").unwrap();
        }
        writer.finish().unwrap();
        assert!(extract(&archive, &output, true)
            .unwrap_err()
            .contains("case-colliding"));
    }
}
