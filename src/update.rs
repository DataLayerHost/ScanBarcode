use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const UPDATE_API: &str = "https://api.github.com/repos/DataLayerHost/ScanBarcode/releases/latest";

pub enum UpdateMessage {
    Available {
        version: String,
        asset_url: Option<String>,
        sha256: Option<[u8; 32]>,
    },
    Progress {
        downloaded: u64,
        total: Option<u64>,
    },
    Downloaded(PathBuf),
    Paused {
        path: PathBuf,
        downloaded: u64,
        total: Option<u64>,
    },
    Prepared(PathBuf),
    Error(String),
}

pub fn check_release() -> Result<UpdateMessage, String> {
    #[cfg(debug_assertions)]
    let local_fixture = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(Path::parent).map(Path::to_path_buf))
        .map(|contents| contents.join("Resources/update-simulation.json"))
        .filter(|path| path.is_file());
    #[cfg(debug_assertions)]
    let json = if let Some(path) = local_fixture {
        fs::read_to_string(path)
            .map_err(|error| format!("Could not read update simulation: {error}"))?
    } else {
        fetch_release_json()?
    };
    #[cfg(not(debug_assertions))]
    let json = fetch_release_json()?;
    let release: serde_json::Value = serde_json::from_str(&json)
        .map_err(|_| "Could not read release information".to_string())?;
    let tag = release
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let version = tag.trim_start_matches('v').to_owned();
    if version.is_empty() {
        return Err("The release has no version number".into());
    }
    let asset = select_asset(&release, asset_suffix());
    Ok(UpdateMessage::Available {
        version,
        asset_url: asset.as_ref().map(|(url, _)| url.clone()),
        sha256: asset.and_then(|(_, digest)| digest),
    })
}

fn fetch_release_json() -> Result<String, String> {
    ureq::get(UPDATE_API)
        .header("User-Agent", "ScanBarcode")
        .call()
        .map_err(|error| format!("Could not check for updates: {error}"))?
        .into_body()
        .read_to_string()
        .map_err(|error| format!("Could not read release information: {error}"))
}

fn asset_suffix() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "-macos-arm64.zip"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "-macos-x86_64.zip"
    }
    #[cfg(target_os = "windows")]
    {
        "-windows.zip"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "-linux.tar.gz"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "-linux-arm64.tar.gz"
    }
}

fn select_asset(release: &serde_json::Value, suffix: &str) -> Option<(String, Option<[u8; 32]>)> {
    let tag = release.get("tag_name")?.as_str()?;
    let expected_name = format!("ScanBarcode-{tag}{suffix}");
    release
        .get("assets")?
        .as_array()?
        .iter()
        .find_map(|asset| {
            let name = asset.get("name")?.as_str()?;
            (name == expected_name).then(|| {
                let url = asset
                    .get("browser_download_url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let digest = asset
                    .get("digest")
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_sha256_digest);
                (url, digest)
            })
        })
        .filter(|(url, _)| !url.is_empty())
}

fn parse_sha256_digest(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(digest)
}

fn update_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let dir = env::temp_dir().join(format!("scanbarcode-update-{}-{now}", std::process::id()));
    fs::create_dir(&dir).map_err(|error| format!("Could not prepare update download: {error}"))?;
    Ok(dir)
}

pub fn download_asset(
    url: &str,
    expected_sha256: [u8; 32],
    partial: Option<PathBuf>,
    pause: &AtomicBool,
    tx: &Sender<UpdateMessage>,
) -> Result<DownloadOutcome, String> {
    let path = match partial {
        Some(path) => path,
        None => update_dir()?.join(if asset_suffix().ends_with("tar.gz") {
            "release.tar.gz"
        } else {
            "release.zip"
        }),
    };
    let existing = fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if existing > 0 && verify_sha256(&path, expected_sha256)? {
        return Ok(DownloadOutcome::Downloaded(path));
    }
    let (mut source, total, offset): (Box<dyn Read>, Option<u64>, u64) = {
        #[cfg(debug_assertions)]
        if let Some(local) = url.strip_prefix("file://") {
            let mut file = fs::File::open(local)
                .map_err(|error| format!("Could not open simulated update: {error}"))?;
            let total = file.metadata().ok().map(|metadata| metadata.len());
            let offset = if total.is_some_and(|total| existing <= total) {
                existing
            } else {
                0
            };
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| error.to_string())?;
            (Box::new(file), total, offset)
        } else {
            open_remote(url, existing)?
        }
        #[cfg(not(debug_assertions))]
        open_remote(url, existing)?
    };
    let mut output = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("Could not save update download: {error}"))?;
    output
        .set_len(offset)
        .map_err(|error| format!("Could not save update download: {error}"))?;
    output
        .seek(SeekFrom::Start(offset))
        .map_err(|error| format!("Could not save update download: {error}"))?;
    let mut downloaded = offset;
    let _ = tx.send(UpdateMessage::Progress { downloaded, total });
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if pause.load(Ordering::Relaxed) {
            output
                .sync_all()
                .map_err(|error| format!("Could not pause update download: {error}"))?;
            return Ok(DownloadOutcome::Paused {
                path,
                downloaded,
                total,
            });
        }
        let count = source
            .read(&mut buffer)
            .map_err(|error| format!("Update download failed: {error}"))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("Could not save update download: {error}"))?;
        downloaded += count as u64;
        let _ = tx.send(UpdateMessage::Progress { downloaded, total });
        #[cfg(debug_assertions)]
        if url.starts_with("file://") {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    if downloaded == 0 || total.is_some_and(|expected| expected != downloaded) {
        return Err("The update download is incomplete".into());
    }
    output
        .sync_all()
        .map_err(|error| format!("Could not finish update download: {error}"))?;
    drop(output);
    if !verify_sha256(&path, expected_sha256)? {
        let _ = fs::remove_file(&path);
        return Err(
            "SHA-256 verification failed: the download does not match GitHub's release digest"
                .into(),
        );
    }
    Ok(DownloadOutcome::Downloaded(path))
}

pub enum DownloadOutcome {
    Downloaded(PathBuf),
    Paused {
        path: PathBuf,
        downloaded: u64,
        total: Option<u64>,
    },
}

fn verify_sha256(path: &Path, expected: [u8; 32]) -> Result<bool, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("Could not read update download: {error}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|error| format!("Could not verify update download: {error}"))?;
    Ok(hasher.finalize().as_slice() == expected)
}

fn open_remote(url: &str, offset: u64) -> Result<(Box<dyn Read>, Option<u64>, u64), String> {
    let mut request = ureq::get(url)
        .header("User-Agent", "ScanBarcode")
        .header("Accept-Encoding", "identity");
    if offset > 0 {
        request = request.header("Range", &format!("bytes={offset}-"));
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(416)) if offset > 0 => return open_remote(url, 0),
        Err(error) => return Err(format!("Update download failed: {error}")),
    };
    let content_length = response.body().content_length();
    if response.status().as_u16() == 206 {
        let range = response
            .headers()
            .get("Content-Range")
            .and_then(|value| value.to_str().ok())
            .ok_or("Update server omitted the content range")?;
        let (start, total) =
            parse_content_range(range).ok_or("Update server returned an invalid content range")?;
        if start != offset || total <= start {
            return Err("Update server returned the wrong content range".into());
        }
        Ok((
            Box::new(response.into_body().into_reader()),
            Some(total),
            offset,
        ))
    } else if response.status().as_u16() == 200 {
        Ok((
            Box::new(response.into_body().into_reader()),
            content_length,
            0,
        ))
    } else {
        Err(format!("Update server returned HTTP {}", response.status()))
    }
}

fn parse_content_range(value: &str) -> Option<(u64, u64)> {
    let range = value.strip_prefix("bytes ")?;
    let (span, total) = range.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let (start, end, total) = (
        start.parse::<u64>().ok()?,
        end.parse::<u64>().ok()?,
        total.parse::<u64>().ok()?,
    );
    (start <= end && end < total).then_some((start, total))
}

pub fn prepare_install(archive: &Path) -> Result<PathBuf, String> {
    let stage = archive
        .parent()
        .ok_or("Update download has no staging directory")?
        .join("staged");
    fs::create_dir(&stage).map_err(|error| format!("Could not prepare installation: {error}"))?;
    let status = {
        #[cfg(target_os = "macos")]
        {
            Command::new("ditto")
                .args(["-x", "-k"])
                .arg(archive)
                .arg(&stage)
                .status()
        }
        #[cfg(target_os = "windows")]
        {
            Command::new("tar")
                .arg("-xf")
                .arg(archive)
                .arg("-C")
                .arg(&stage)
                .status()
        }
        #[cfg(target_os = "linux")]
        {
            Command::new("tar")
                .arg("-xzf")
                .arg(archive)
                .arg("-C")
                .arg(&stage)
                .status()
        }
    }
    .map_err(|error| format!("Could not unpack update: {error}"))?;
    if !status.success() {
        return Err("Could not unpack the update package".into());
    }
    #[cfg(target_os = "macos")]
    let candidate = stage.join("ScanBarcode.app");
    #[cfg(not(target_os = "macos"))]
    let candidate = find_update_binary(&stage)?;
    #[cfg(target_os = "macos")]
    if !candidate.join("Contents/MacOS/ScanBarcode").is_file() {
        return Err("The update package has no ScanBarcode app".into());
    }
    #[cfg(target_os = "macos")]
    verify_macos_architecture(&candidate.join("Contents/MacOS/ScanBarcode"))?;
    #[cfg(target_os = "linux")]
    verify_linux_architecture(&candidate)?;
    Ok(candidate)
}

#[cfg(target_os = "linux")]
fn verify_linux_architecture(binary: &Path) -> Result<(), String> {
    let mut header = [0u8; 20];
    fs::File::open(binary)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| format!("Could not inspect update architecture: {error}"))?;
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected_machine = if cfg!(target_arch = "aarch64") {
        183
    } else {
        62
    };
    if header[..6] != *b"\x7fELF\x02\x01" || machine != expected_machine {
        return Err("The update package does not support this Linux architecture".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_macos_architecture(binary: &Path) -> Result<(), String> {
    let expected = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let output = Command::new("/usr/bin/lipo")
        .arg("-archs")
        .arg(binary)
        .output()
        .map_err(|error| format!("Could not inspect update architecture: {error}"))?;
    if !output.status.success() {
        return Err("Could not inspect update architecture".into());
    }
    let architectures =
        std::str::from_utf8(&output.stdout).map_err(|_| "Could not read update architecture")?;
    if !architectures
        .split_whitespace()
        .any(|arch| arch == expected)
    {
        return Err(format!("The update package does not support {expected}"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn find_update_binary(stage: &Path) -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let name = "ScanBarcode.exe";
    #[cfg(target_os = "linux")]
    let name = "ScanBarcode";
    for entry in fs::read_dir(stage).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let candidate = if path.is_dir() { path.join(name) } else { path };
        if candidate.file_name().is_some_and(|file| file == name) && candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("The update package has no ScanBarcode executable".into())
}

pub fn launch_install_helper(staged: &Path) -> Result<(), String> {
    let current_exe =
        env::current_exe().map_err(|error| format!("Could not find running app: {error}"))?;
    #[cfg(target_os = "macos")]
    let destination = current_exe
        .ancestors()
        .find(|part| part.extension().is_some_and(|extension| extension == "app"))
        .ok_or("Automatic installation needs a macOS app bundle")?
        .to_path_buf();
    #[cfg(not(target_os = "macos"))]
    let destination = current_exe;
    let parent = destination
        .parent()
        .ok_or("Could not find installation directory")?;
    let probe = parent.join(format!(".scanbarcode-write-check-{}", std::process::id()));
    fs::write(&probe, []).map_err(|_| {
        "The app folder is not writable. Move the app to a writable folder and try again."
            .to_string()
    })?;
    let _ = fs::remove_file(probe);
    let backup = parent.join(format!(".scanbarcode-previous-{}", std::process::id()));
    if backup.exists() {
        return Err("An old update backup is blocking installation".into());
    }
    let script = staged
        .parent()
        .ok_or("Update staging directory is missing")?
        .join(if cfg!(target_os = "windows") {
            "install.ps1"
        } else {
            "install.sh"
        });
    #[cfg(target_os = "windows")]
    let body = include_str!("update_windows.ps1");
    #[cfg(not(target_os = "windows"))]
    let body = include_str!("update_unix.sh");
    fs::write(&script, body)
        .map_err(|error| format!("Could not prepare update helper: {error}"))?;
    let mut command = {
        #[cfg(target_os = "windows")]
        {
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
            command
        }
        #[cfg(not(target_os = "windows"))]
        {
            Command::new("sh")
        }
    };
    command
        .arg(&script)
        .arg(std::process::id().to_string())
        .arg(staged)
        .arg(&destination)
        .arg(&backup)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Could not start update installer: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_the_matching_version_and_architecture() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let release = serde_json::json!({"tag_name": "0.1.4", "assets": [
            {"name": "ScanBarcode-0.1.3-macos-arm64.zip", "browser_download_url": "stale-arm64", "digest": digest},
            {"name": "Other-0.1.4-macos-arm64.zip", "browser_download_url": "wrong-app", "digest": digest},
            {"name": "ScanBarcode-0.1.4-windows.zip", "browser_download_url": "windows", "digest": digest},
            {"name": "ScanBarcode-0.1.4-linux.tar.gz", "browser_download_url": "linux", "digest": digest},
            {"name": "ScanBarcode-0.1.4-linux-arm64.tar.gz", "browser_download_url": "linux-arm64", "digest": digest},
            {"name": "ScanBarcode-0.1.4-macos-arm64.zip", "browser_download_url": "arm64", "digest": digest},
            {"name": "ScanBarcode-0.1.4-macos-x86_64.zip", "browser_download_url": "x86_64", "digest": digest}
        ]});
        for (suffix, expected_url) in [
            ("-macos-arm64.zip", "arm64"),
            ("-macos-x86_64.zip", "x86_64"),
            ("-windows.zip", "windows"),
            ("-linux.tar.gz", "linux"),
            ("-linux-arm64.tar.gz", "linux-arm64"),
        ] {
            let (url, sha256) = select_asset(&release, suffix).unwrap();
            assert_eq!(url, expected_url);
            assert_eq!(sha256, Some([0xab; 32]));
        }
        assert!(select_asset(&release, "-macos-universal.zip").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_an_update_for_the_wrong_linux_architecture() {
        assert_eq!(
            asset_suffix(),
            if cfg!(target_arch = "aarch64") {
                "-linux-arm64.tar.gz"
            } else {
                "-linux.tar.gz"
            }
        );
        let path = update_dir().unwrap().join("architecture-test");
        let mut header = [0u8; 20];
        header[..6].copy_from_slice(b"\x7fELF\x02\x01");
        let native_machine: u16 = if cfg!(target_arch = "aarch64") {
            183
        } else {
            62
        };
        header[18..20].copy_from_slice(&native_machine.to_le_bytes());
        fs::write(&path, header).unwrap();
        assert!(verify_linux_architecture(&path).is_ok());
        header[18..20]
            .copy_from_slice(&if native_machine == 62 { 183u16 } else { 62u16 }.to_le_bytes());
        fs::write(&path, header).unwrap();
        assert!(verify_linux_architecture(&path).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn checks_the_architecture_before_installing_an_update() {
        let native_binary = env::current_exe().unwrap();
        assert!(verify_macos_architecture(&native_binary).is_ok());

        let invalid_binary = update_dir().unwrap().join("invalid-binary");
        fs::write(&invalid_binary, b"not a Mach-O binary").unwrap();
        assert!(verify_macos_architecture(&invalid_binary).is_err());
    }

    #[test]
    fn simulated_download_reports_progress_and_keeps_all_bytes() {
        let source = update_dir().unwrap().join("source.zip");
        let bytes = vec![0x5a; 150_000];
        fs::write(&source, &bytes).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let destination = match download_asset(
            &format!("file://{}", source.display()),
            digest,
            None,
            &AtomicBool::new(false),
            &tx,
        )
        .unwrap()
        {
            DownloadOutcome::Downloaded(path) => path,
            DownloadOutcome::Paused { .. } => panic!("download paused unexpectedly"),
        };
        let progress: Vec<_> = rx.try_iter().collect();
        assert!(progress.len() >= 3);
        assert!(matches!(
            progress.last(),
            Some(UpdateMessage::Progress {
                downloaded: 150_000,
                total: Some(150_000)
            })
        ));
        assert_eq!(fs::read(destination).unwrap(), bytes);
    }

    #[test]
    fn rejects_missing_or_mismatched_sha256() {
        assert!(parse_sha256_digest("sha256:bad").is_none());
        assert!(parse_sha256_digest(&"ab".repeat(32)).is_none());
        let release = serde_json::json!({"tag_name": "0.1.4", "assets": [{
            "name": format!("ScanBarcode-0.1.4{}", asset_suffix()),
            "browser_download_url": "file:///tmp/update.zip"
        }]});
        assert!(select_asset(&release, asset_suffix()).unwrap().1.is_none());

        let source = update_dir().unwrap().join("tampered.zip");
        fs::write(&source, b"tampered update").unwrap();
        let (tx, _) = std::sync::mpsc::channel();
        let error = match download_asset(
            &format!("file://{}", source.display()),
            [0; 32],
            None,
            &AtomicBool::new(false),
            &tx,
        ) {
            Err(error) => error,
            Ok(_) => panic!("invalid digest was accepted"),
        };
        assert!(error.contains("SHA-256 verification failed"));
    }

    #[test]
    fn resumes_after_a_pause() {
        let source = update_dir().unwrap().join("source.zip");
        let bytes = vec![0x4b; 2_000_000];
        fs::write(&source, &bytes).unwrap();
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let url = format!("file://{}", source.display());
        let (tx, rx) = std::sync::mpsc::channel();
        let pause = std::sync::Arc::new(AtomicBool::new(false));
        let worker_pause = pause.clone();
        let worker_url = url.clone();
        let worker = std::thread::spawn(move || {
            download_asset(&worker_url, digest, None, &worker_pause, &tx).unwrap()
        });
        while let Ok(message) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            if matches!(message, UpdateMessage::Progress { downloaded, .. } if downloaded > 0) {
                pause.store(true, Ordering::Relaxed);
                break;
            }
        }
        let (partial, saved) = match worker.join().unwrap() {
            DownloadOutcome::Paused {
                path, downloaded, ..
            } => (path, downloaded),
            DownloadOutcome::Downloaded(_) => panic!("download finished before pause"),
        };
        assert!(saved > 0 && saved < bytes.len() as u64);
        assert_eq!(fs::metadata(&partial).unwrap().len(), saved);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (tx, rx) = std::sync::mpsc::channel();
        let completed =
            download_asset(&url, digest, Some(partial), &AtomicBool::new(false), &tx).unwrap();
        assert!(
            matches!(rx.try_recv(), Ok(UpdateMessage::Progress { downloaded, .. }) if downloaded == saved)
        );
        let DownloadOutcome::Downloaded(path) = completed else {
            panic!("resume paused unexpectedly")
        };
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn restarts_safely_when_http_server_ignores_range() {
        use std::net::TcpListener;
        let bytes = vec![0x36; 150_000];
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let partial = update_dir().unwrap().join("release.zip");
        fs::write(&partial, &bytes[..50_000]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_bytes = bytes.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            assert!(request.contains("range: bytes=50000-"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_bytes.len()
            )
            .unwrap();
            stream.write_all(&server_bytes).unwrap();
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let result = download_asset(
            &format!("http://{address}/release.zip"),
            digest,
            Some(partial),
            &AtomicBool::new(false),
            &tx,
        )
        .unwrap();
        server.join().unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(UpdateMessage::Progress { downloaded: 0, .. })
        ));
        let DownloadOutcome::Downloaded(path) = result else {
            panic!("download paused unexpectedly")
        };
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn resumes_from_http_partial_content() {
        use std::net::TcpListener;
        let bytes = vec![0x7c; 150_000];
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let partial = update_dir().unwrap().join("release.zip");
        fs::write(&partial, &bytes[..50_000]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_bytes = bytes.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            assert!(request.contains("range: bytes=50000-"));
            write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Length: 100000\r\nContent-Range: bytes 50000-149999/150000\r\nConnection: close\r\n\r\n").unwrap();
            stream.write_all(&server_bytes[50_000..]).unwrap();
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let result = download_asset(
            &format!("http://{address}/release.zip"),
            digest,
            Some(partial),
            &AtomicBool::new(false),
            &tx,
        )
        .unwrap();
        server.join().unwrap();
        assert!(matches!(
            rx.try_recv(),
            Ok(UpdateMessage::Progress {
                downloaded: 50_000,
                ..
            })
        ));
        let DownloadOutcome::Downloaded(path) = result else {
            panic!("download paused unexpectedly")
        };
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn validates_http_content_ranges() {
        assert_eq!(parse_content_range("bytes 5-9/10"), Some((5, 10)));
        assert_eq!(parse_content_range("bytes 5-10/10"), None);
        assert_eq!(parse_content_range("bytes */10"), None);
    }
}
