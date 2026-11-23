//! Credentials: the header barre sets on requests to a name whose rule
//! names the credential (`api.example.com@token`), for one VM.
//!
//! Each is a file, `<dir>/<address>/<name>`, holding one header line,
//! `Name: value`. Root writes it (`barre credential`); the service only
//! reads it, on every request, so a value replaced on disk is used at once.
//! Files and their directories take the group of `dir`, which is barre's,
//! and nobody else can read them.

use std::fs;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hyper::header::{HeaderName, HeaderValue};

use crate::rules::is_credential_name;

/// Where credentials live. `/run` is cleared at boot, like the VMs.
pub const DIR: &str = "/run/barre/credentials";

/// The most a credential file may hold.
const LIMIT: usize = 16 * 1024;

fn vm_dir(dir: &Path, addr: Ipv4Addr) -> PathBuf {
    dir.join(addr.to_string())
}

fn check_name(name: &str) -> io::Result<()> {
    if is_credential_name(name) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad credential name {name:?}: letters, digits, - and _"),
        ))
    }
}

/// Parses a credential: one `Name: value` line, an optional newline after.
pub fn parse(bytes: &[u8]) -> Result<(HeaderName, HeaderValue), String> {
    if bytes.len() > LIMIT {
        return Err("too long".into());
    }
    let line = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    if line.iter().any(|&b| b == b'\n' || b == b'\r') {
        return Err("more than one line".into());
    }
    let colon = line
        .iter()
        .position(|&b| b == b':')
        .ok_or("not a header line: Name: value")?;
    let name = HeaderName::from_bytes(&line[..colon]).map_err(|_| "not a header name")?;
    let value = line[colon + 1..].trim_ascii();
    if value.is_empty() {
        return Err("empty value".into());
    }
    let value = HeaderValue::from_bytes(value).map_err(|_| "not a header value")?;
    Ok((name, value))
}

/// Writes `addr`'s credential `name`, replacing it. `bytes` must parse.
pub fn write(dir: &Path, addr: Ipv4Addr, name: &str, bytes: &[u8]) -> io::Result<()> {
    check_name(name)?;
    parse(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let gid = fs::metadata(dir)?.gid();
    let vm = vm_dir(dir, addr);
    if !vm.is_dir() {
        fs::create_dir(&vm)?;
        std::os::unix::fs::chown(&vm, None, Some(gid))?;
        fs::set_permissions(&vm, fs::Permissions::from_mode(0o750))?;
    }
    let tmp = vm.join(format!(".{name}.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o640)
        .open(&tmp)?;
    std::os::unix::fs::fchown(&file, None, Some(gid))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, vm.join(name))
}

/// Reads `addr`'s credential `name`.
pub fn read(dir: &Path, addr: Ipv4Addr, name: &str) -> Result<(HeaderName, HeaderValue), String> {
    check_name(name).map_err(|e| e.to_string())?;
    let bytes = fs::read(vm_dir(dir, addr).join(name)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("no credential {name}"),
        _ => format!("credential {name}: {e}"),
    })?;
    parse(&bytes).map_err(|e| format!("credential {name}: {e}"))
}

/// Removes `addr`'s credential `name`. Safe to repeat.
pub fn remove(dir: &Path, addr: Ipv4Addr, name: &str) -> io::Result<()> {
    check_name(name)?;
    match fs::remove_file(vm_dir(dir, addr).join(name)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Removes every credential of `addr`. Safe to repeat.
pub fn remove_all(dir: &Path, addr: Ipv4Addr) -> io::Result<()> {
    match fs::remove_dir_all(vm_dir(dir, addr)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Removes the credentials of every address not in `keep`.
pub fn prune(dir: &Path, keep: &[Ipv4Addr]) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        r => r?,
    };
    for entry in entries {
        let entry = entry?;
        if let Some(addr) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<Ipv4Addr>().ok())
            && !keep.contains(&addr)
        {
            remove_all(dir, addr)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "barre-cred-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn credentials_parse_as_one_header_line() {
        let (n, v) = parse(b"Authorization: Bearer abc\n").unwrap();
        assert_eq!(n, "authorization");
        assert_eq!(v, "Bearer abc");
        assert_eq!(parse(b"X-Api-Key:k").unwrap().1, "k");
        for bad in [
            &b""[..],
            b"Authorization",
            b"Authorization: ",
            b"Bad Name: x",
            b"Authorization: a\nb",
            b"Authorization: a\n\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn credentials_are_written_read_and_removed() {
        let dir = dir();
        let a = Ipv4Addr::new(10, 200, 0, 5);
        let b = Ipv4Addr::new(10, 200, 0, 9);
        write(&dir, a, "claude", b"Authorization: Bearer one\n").unwrap();
        write(&dir, a, "claude", b"Authorization: Bearer two\n").unwrap();
        write(&dir, b, "git", b"Authorization: Basic x").unwrap();
        assert_eq!(read(&dir, a, "claude").unwrap().1, "Bearer two");
        let mode = fs::metadata(dir.join("10.200.0.5/claude")).unwrap().mode();
        assert_eq!(mode & 0o777, 0o640);
        assert!(read(&dir, a, "git").is_err());
        assert!(write(&dir, a, "../x", b"A: b").is_err());
        assert!(write(&dir, a, "bad", b"not a header").is_err());
        remove(&dir, a, "claude").unwrap();
        remove(&dir, a, "claude").unwrap();
        assert!(read(&dir, a, "claude").is_err());
        prune(&dir, &[a]).unwrap();
        assert!(read(&dir, b, "git").is_err());
        remove_all(&dir, a).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
