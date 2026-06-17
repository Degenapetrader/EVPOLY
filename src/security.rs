use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    let max_len = a.len().max(b.len());
    for idx in 0..max_len {
        let left = a.get(idx).copied().unwrap_or(0);
        let right = b.get(idx).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

pub fn env_truthy(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

#[cfg(unix)]
pub fn harden_secret_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn harden_secret_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub fn write_secret_file(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    if path.exists() {
        harden_secret_file_permissions(path)?;
    }

    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;
    file.write_all(content.as_ref())?;
    file.flush()?;
    harden_secret_file_permissions(path)
}
