#![allow(dead_code)] // Wired up in a later round (local-model task).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

pub fn sha256_hex(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok(hex)
}

pub fn human_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangePlan {
    Fresh,
    Resume(u64),
    Restart,
}

fn range_plan(part_len: u64, size: u64) -> RangePlan {
    if part_len == 0 {
        RangePlan::Fresh
    } else if part_len < size {
        RangePlan::Resume(part_len)
    } else {
        RangePlan::Restart
    }
}

const OVERSIZED_ERROR: &str = "The download was larger than expected, so Writely stopped it.";

fn write_error(part: &Path, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::StorageFull {
        "Not enough free disk space to finish the download. Free some space and try again."
            .to_string()
    } else {
        format!("Could not save the download ({}): {e}", part.display())
    }
}

fn copy_verified(
    reader: &mut dyn Read,
    part: &Path,
    start_at: u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut file = if start_at == 0 {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(part)
            .map_err(|e| write_error(part, &e))?
    } else {
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(part)
            .map_err(|e| write_error(part, &e))?
    };
    let mut bytes_so_far = start_at;
    let mut last_reported = start_at;
    let mut buf = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Download cancelled.".to_string());
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Could not read the downloaded data: {e}"))?;
        if n == 0 {
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Download cancelled.".to_string());
        }
        file.write_all(&buf[..n])
            .map_err(|e| write_error(part, &e))?;
        bytes_so_far += n as u64;
        if bytes_so_far > total {
            return Err(OVERSIZED_ERROR.to_string());
        }
        if bytes_so_far - last_reported >= 1024 * 1024 {
            progress(bytes_so_far, total);
            last_reported = bytes_so_far;
        }
    }
    file.flush().map_err(|e| write_error(part, &e))?;
    progress(bytes_so_far, total);
    Ok(())
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

#[allow(clippy::too_many_arguments)]
pub fn download_file(
    url: &str,
    dest: &Path,
    sha256: &str,
    size: u64,
    name: &str,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<(), String> {
    if dest.exists() {
        let existing = sha256_hex(dest)?;
        if existing.eq_ignore_ascii_case(sha256) {
            return Ok(());
        }
    }

    let parent: PathBuf = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent)
        .map_err(|e| format!("Could not save the download ({}): {e}", dest.display()))?;

    let part = part_path(dest);
    let part_len = match std::fs::metadata(&part) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => {
            return Err(format!(
                "Could not save the download ({}): {e}",
                part.display()
            ))
        }
    };
    let plan = range_plan(part_len, size);
    let need = match plan {
        RangePlan::Fresh | RangePlan::Restart => size,
        RangePlan::Resume(from) => size - from,
    };
    let free = fs4::available_space(&parent).map_err(|e| {
        format!(
            "Could not check free disk space for {name} ({}): {e}",
            parent.display()
        )
    })?;
    if free < need {
        return Err(format!(
            "Not enough free disk space for {name}: it needs {} but only {} is free. Free some space and try again.",
            human_size(need),
            human_size(free)
        ));
    }

    let resume_from: Option<u64> = match plan {
        RangePlan::Fresh => None,
        RangePlan::Resume(from) => Some(from),
        RangePlan::Restart => {
            std::fs::remove_file(&part)
                .map_err(|e| format!("Could not save the download ({}): {e}", part.display()))?;
            None
        }
    };

    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let request = agent.get(url);
    let request = match resume_from {
        Some(from) => request.header("Range", &format!("bytes={from}-")),
        None => request,
    };
    let mut response = request.call().map_err(|e| {
        format!("Could not download {name}: {e}. Check your internet connection and try again.")
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!(
            "Could not download {name} (HTTP {status}). Check your internet connection and try again."
        ));
    }
    let start_at = match (resume_from, status) {
        (Some(from), 206) => from,
        _ => 0,
    };

    {
        let mut reader = response.body_mut().as_reader();
        if let Err(e) = copy_verified(&mut reader, &part, start_at, size, progress, cancel) {
            if e == OVERSIZED_ERROR {
                if let Err(delete_err) = std::fs::remove_file(&part) {
                    eprintln!(
                        "Writely: could not delete the oversized download ({}): {delete_err}",
                        part.display()
                    );
                }
            }
            return Err(e);
        }
    }

    let damaged = || {
        format!(
            "The download of {name} was damaged (its fingerprint didn't match), so Writely deleted it. Try again."
        )
    };
    let part_len_after = std::fs::metadata(&part)
        .map_err(|e| format!("Could not save the download ({}): {e}", part.display()))?
        .len();
    let ok = if part_len_after == size {
        sha256_hex(&part)?.eq_ignore_ascii_case(sha256)
    } else {
        false
    };
    if !ok {
        if let Err(e) = std::fs::remove_file(&part) {
            eprintln!(
                "Writely: could not delete the damaged download ({}): {e}",
                part.display()
            );
        }
        return Err(damaged());
    }

    std::fs::rename(&part, dest)
        .map_err(|e| format!("Could not save the download ({}): {e}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("writely-download-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup(dir: &Path) {
        let result = std::fs::remove_dir_all(dir);
        assert!(result.is_ok(), "cleanup {}: {:?}", dir.display(), result);
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(508_000_000), "508 MB");
        assert_eq!(human_size(1_300_000_000), "1.3 GB");
        assert_eq!(human_size(1_329_851_808), "1.3 GB");
        assert_eq!(human_size(532_517_120), "533 MB");
        assert_eq!(human_size(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn range_plan_covers_edges() {
        assert_eq!(range_plan(0, 100), RangePlan::Fresh);
        assert_eq!(range_plan(1, 100), RangePlan::Resume(1));
        assert_eq!(range_plan(50, 100), RangePlan::Resume(50));
        assert_eq!(range_plan(99, 100), RangePlan::Resume(99));
        assert_eq!(range_plan(100, 100), RangePlan::Restart);
        assert_eq!(range_plan(101, 100), RangePlan::Restart);
    }

    #[test]
    fn sha256_hex_of_known_file() {
        let dir = temp_dir("sha");
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").expect("write");
        assert_eq!(
            sha256_hex(&path).expect("hash"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let empty = dir.join("empty");
        std::fs::write(&empty, b"").expect("write");
        assert_eq!(
            sha256_hex(&empty).expect("hash"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        cleanup(&dir);
    }

    #[test]
    fn copy_verified_full_copy() {
        let dir = temp_dir("full");
        let part = dir.join("f.part");
        let data = b"hello world".to_vec();
        let mut reader = Cursor::new(data.clone());
        let cancel = AtomicBool::new(false);
        let mut calls: Vec<(u64, u64)> = Vec::new();
        copy_verified(
            &mut reader,
            &part,
            0,
            data.len() as u64,
            &mut |done, total| calls.push((done, total)),
            &cancel,
        )
        .expect("copy");
        assert_eq!(std::fs::read(&part).expect("read part"), data);
        assert_eq!(calls.last(), Some(&(11, 11)));
        cleanup(&dir);
    }

    #[test]
    fn copy_verified_resume_appends() {
        let dir = temp_dir("resume");
        let part = dir.join("f.part");
        std::fs::write(&part, b"hello ").expect("write existing part");
        let mut reader = Cursor::new(b"world".to_vec());
        let cancel = AtomicBool::new(false);
        let mut calls: Vec<(u64, u64)> = Vec::new();
        copy_verified(
            &mut reader,
            &part,
            6,
            11,
            &mut |done, total| calls.push((done, total)),
            &cancel,
        )
        .expect("copy");
        assert_eq!(std::fs::read(&part).expect("read part"), b"hello world");
        assert_eq!(calls.last(), Some(&(11, 11)));
        cleanup(&dir);
    }

    #[test]
    fn copy_verified_cancel_keeps_part() {
        let dir = temp_dir("cancel");
        let part = dir.join("f.part");
        let data = vec![b'x'; 3 * 1024 * 1024];
        let total = data.len() as u64;
        let mut reader = Cursor::new(data);
        let cancel = AtomicBool::new(false);
        let err = copy_verified(
            &mut reader,
            &part,
            0,
            total,
            &mut |_done, _total| {
                cancel.store(true, Ordering::Relaxed);
            },
            &cancel,
        )
        .expect_err("should cancel");
        assert_eq!(err, "Download cancelled.");
        assert!(part.exists());
        let len = std::fs::metadata(&part).expect("metadata").len();
        assert!(len >= 1024 * 1024, "part len {len}");
        assert!(len < total, "part len {len} should be partial");
        cleanup(&dir);
    }

    #[test]
    fn copy_verified_rejects_oversized() {
        let dir = temp_dir("oversized");
        let part = dir.join("f.part");
        let mut reader = Cursor::new(vec![b'x'; 110]);
        let cancel = AtomicBool::new(false);
        let err = copy_verified(&mut reader, &part, 0, 100, &mut |_, _| {}, &cancel)
            .expect_err("should reject oversized");
        assert_eq!(
            err,
            "The download was larger than expected, so Writely stopped it."
        );
        cleanup(&dir);
    }

    #[test]
    fn copy_verified_cancel_before_start() {
        let dir = temp_dir("cancel-before");
        let part = dir.join("f.part");
        std::fs::write(&part, b"keep").expect("write");
        let mut reader = Cursor::new(b"more".to_vec());
        let cancel = AtomicBool::new(true);
        let mut calls: Vec<(u64, u64)> = Vec::new();
        let err = copy_verified(
            &mut reader,
            &part,
            4,
            8,
            &mut |done, total| calls.push((done, total)),
            &cancel,
        )
        .expect_err("should cancel");
        assert_eq!(err, "Download cancelled.");
        assert_eq!(std::fs::read(&part).expect("read"), b"keep");
        cleanup(&dir);
    }
}
