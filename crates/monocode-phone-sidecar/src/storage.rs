//! Private, sidecar-owned state. Never uses the desktop keyring or host store.
use monocode_mobile_link::Identity;
use serde::{de::DeserializeOwned, Serialize};
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, os::unix::fs::{MetadataExt, OpenOptionsExt}, path::Path};

pub fn directory(path: &Path) -> Result<(), String> {
    if !path.exists() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path).map_err(|e| e.to_string())?;
    }
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o7777 != 0o700 {
        return Err("Sidecar data directory must be owned by this user, not a symlink, and mode 0700".into());
    }
    Ok(())
}

pub fn private_file(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
        .map_err(|_| "Cannot open private sidecar file".to_string())?;
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o7777 != 0o600 {
        return Err("Private files must be owned by this user and mode 0600".into());
    }
    Ok(file)
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    serde_json::from_reader(private_file(path)?).map_err(|_| "Invalid sidecar state file".into())
}

pub fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    // Atomic replacement plus directory fsync keeps retry records durable.
    let parent = path.parent().ok_or("Missing state directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    File::open(parent).and_then(|f| f.sync_all()).map_err(|e| e.to_string())
}

pub fn lock(dir: &Path) -> Result<File, String> {
    let path = dir.join("sidecar.lock");
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(&path).map_err(|e| e.to_string())?;
    file.try_lock().map_err(|_| "Another sidecar owns this data directory".to_string())?;
    Ok(file)
}

pub fn token(path: &Path) -> Result<String, String> {
    let mut value = String::new();
    private_file(path)?.take(1024).read_to_string(&mut value).map_err(|_| "Cannot read host token")?;
    let token = value.trim();
    if token.len() != 43 || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("Host token file must contain one 43-character bearer token".into());
    }
    Ok(token.into())
}

pub fn identity(dir: &Path) -> Result<Identity, String> {
    let path = dir.join("identity.json");
    if path.exists() { return read(&path); }
    if dir.join("mobile-access.json").exists() {
        return Err("Sidecar identity missing; restore it rather than silently replacing paired identity".into());
    }
    let value = Identity::generate()?;
    write(&path, &value)?;
    Ok(value)
}
