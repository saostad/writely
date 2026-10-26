#![allow(dead_code)] // Wired up in a later round (local-model task).

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::catalog::ArchiveKind;

fn io_error(archive: &Path, e: &dyn std::fmt::Display) -> String {
    format!("Could not unpack llama.cpp ({}): {e}", archive.display())
}

fn unsafe_error(path: &Path) -> String {
    format!(
        "The llama.cpp download contains an unsafe file path ({}), so Writely stopped. Try again later.",
        path.display()
    )
}

fn tmp_path(into: &Path) -> PathBuf {
    let mut name = into.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Removes a file, symlink or directory tree; a missing path is fine.
fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn cleanup_tmp(tmp: &Path) {
    if let Err(e) = remove_path(tmp) {
        eprintln!(
            "Writely: could not delete the temporary unpack folder ({}): {e}",
            tmp.display()
        );
    }
}

/// True if the archive path is absolute or contains a `..` (or root/prefix)
/// component. Also checks `/`- and `\`-separated segments in the raw text so
/// Windows-style separators can't slip past on Unix and vice versa.
fn is_unsafe_path(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
            Component::Normal(_) | Component::CurDir => {}
        }
    }
    let text = path.to_string_lossy();
    if text.starts_with('/') || text.starts_with('\\') {
        return true;
    }
    // Drive prefix such as `C:`.
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    text.split(['/', '\\']).any(|segment| segment == "..")
}

/// The normal components of `path`'s parent, used as the lexical base a
/// relative symlink target is resolved against.
fn parent_components(path: &Path) -> Vec<std::ffi::OsString> {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    if let Some(parent) = path.parent() {
        for component in parent.components() {
            if let Component::Normal(part) = component {
                parts.push(part.to_os_string());
            }
        }
    }
    parts
}

/// True if `target`, resolved lexically against `base` (components relative
/// to the unpack root), stays inside the root. `target` must be relative.
fn target_stays_inside(base: &[std::ffi::OsString], target: &Path) -> bool {
    if target.is_absolute() || is_unsafe_absolute_text(target) {
        return false;
    }
    let mut depth = base.len();
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn is_unsafe_absolute_text(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.starts_with('/') || text.starts_with('\\') {
        return true;
    }
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn unpack_targz(archive: &Path, tmp: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| io_error(archive, &e))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    let entries = tar.entries().map_err(|e| io_error(archive, &e))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| io_error(archive, &e))?;
        let path = entry
            .path()
            .map_err(|e| io_error(archive, &e))?
            .into_owned();
        if is_unsafe_path(&path) {
            return Err(unsafe_error(&path));
        }
        let entry_type = entry.header().entry_type();
        match entry_type {
            tar::EntryType::Char | tar::EntryType::Block | tar::EntryType::Fifo => {
                return Err(unsafe_error(&path));
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(|e| io_error(archive, &e))?
                    .map(|t| t.into_owned());
                match target {
                    Some(target) if target_stays_inside(&parent_components(&path), &target) => {
                        // also require '..' segments to not escape is handled
                        // by target_stays_inside; absolute is rejected there
                    }
                    _ => return Err(unsafe_error(&path)),
                }
            }
            tar::EntryType::Link => {
                let target = entry
                    .link_name()
                    .map_err(|e| io_error(archive, &e))?
                    .map(|t| t.into_owned());
                match target {
                    Some(target) if !is_unsafe_path(&target) => {}
                    _ => return Err(unsafe_error(&path)),
                }
            }
            _ => {}
        }
        entry.unpack_in(tmp).map_err(|e| io_error(archive, &e))?;
    }
    Ok(())
}

fn unpack_zip(archive: &Path, tmp: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| io_error(archive, &e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| io_error(archive, &e))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| io_error(archive, &e))?;
        let raw_name = entry
            .name()
            .map(|n| n.into_owned())
            .map_err(|e| io_error(archive, &e))?;
        let raw_path = PathBuf::from(&raw_name);
        if is_unsafe_path(&raw_path) {
            return Err(unsafe_error(&raw_path));
        }
        let enclosed = match entry.enclosed_name() {
            Some(p) => p,
            None => return Err(unsafe_error(&raw_path)),
        };
        if enclosed.as_os_str().is_empty() {
            continue;
        }
        let dest = tmp.join(&enclosed);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| io_error(archive, &e))?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(archive, &e))?;
        }
        if entry.is_symlink() {
            let mut target_text = String::new();
            entry
                .read_to_string(&mut target_text)
                .map_err(|e| io_error(archive, &e))?;
            let target = PathBuf::from(&target_text);
            if !target_stays_inside(&parent_components(&enclosed), &target) {
                return Err(unsafe_error(&raw_path));
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &dest).map_err(|e| io_error(archive, &e))?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&dest, &target_text).map_err(|e| io_error(archive, &e))?;
            }
            continue;
        }
        let mode = entry.unix_mode();
        {
            let mut out = File::create(&dest).map_err(|e| io_error(archive, &e))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| io_error(archive, &e))?;
        }
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|e| io_error(archive, &e))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
    }
    Ok(())
}

pub fn unpack(archive: &Path, kind: ArchiveKind, into: &Path) -> Result<(), String> {
    let tmp = tmp_path(into);
    remove_path(&tmp).map_err(|e| io_error(archive, &e))?;
    // The tmp dir is a sibling of `into`; make sure its parent exists.
    if let Some(parent) = tmp.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(archive, &e))?;
        }
    }
    std::fs::create_dir_all(&tmp).map_err(|e| io_error(archive, &e))?;

    let result = match kind {
        ArchiveKind::TarGz => unpack_targz(archive, &tmp),
        ArchiveKind::Zip => unpack_zip(archive, &tmp),
    };
    if let Err(e) = result {
        cleanup_tmp(&tmp);
        return Err(e);
    }

    if let Err(e) = remove_path(into) {
        cleanup_tmp(&tmp);
        return Err(io_error(archive, &e));
    }
    if let Err(e) = std::fs::rename(&tmp, into) {
        cleanup_tmp(&tmp);
        return Err(io_error(archive, &e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("writely-unpack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup(dir: &Path) {
        let result = std::fs::remove_dir_all(dir);
        assert!(result.is_ok(), "cleanup {}: {:?}", dir.display(), result);
    }

    fn add_tar_file(
        builder: &mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(contents.len() as u64);
        header.set_mode(mode);
        header.set_path(path).expect("set tar path");
        header.set_cksum();
        builder.append(&header, contents).expect("append tar file");
    }

    fn add_tar_symlink(
        builder: &mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>,
        path: &str,
        target: &str,
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_path(path).expect("set tar path");
        header.set_link_name(target).expect("set tar link name");
        header.set_cksum();
        builder
            .append(&header, std::io::empty())
            .expect("append tar symlink");
    }

    fn finish_targz(builder: tar::Builder<flate2::write::GzEncoder<Vec<u8>>>, archive: &Path) {
        let encoder = builder.into_inner().expect("finish tar");
        let bytes = encoder.finish().expect("finish gzip");
        std::fs::write(archive, bytes).expect("write archive");
    }

    fn make_targz_builder() -> tar::Builder<flate2::write::GzEncoder<Vec<u8>>> {
        tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ))
    }

    #[test]
    fn targz_with_file_and_inside_symlink_unpacks() {
        let dir = temp_dir("targz-ok");
        let archive = dir.join("runtime.tar.gz");
        let mut builder = make_targz_builder();
        add_tar_file(&mut builder, "llama-b11900/llama-server", b"bin", 0o755);
        add_tar_file(&mut builder, "llama-b11900/libggml.so.0", b"lib", 0o644);
        add_tar_symlink(&mut builder, "llama-b11900/libggml.so", "libggml.so.0");
        finish_targz(builder, &archive);

        let into = dir.join("runtime");
        // A pre-existing `into` is replaced.
        std::fs::create_dir_all(&into).expect("create old into");
        std::fs::write(into.join("stale.txt"), b"old").expect("write stale");

        unpack(&archive, ArchiveKind::TarGz, &into).expect("unpack should succeed");

        let server = into.join("llama-b11900").join("llama-server");
        assert_eq!(std::fs::read(&server).expect("read server"), b"bin");
        assert!(!into.join("stale.txt").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&server)
                .expect("metadata")
                .permissions()
                .mode();
            assert_ne!(
                mode & 0o111,
                0,
                "server should be executable, mode {mode:o}"
            );
            let link = into.join("llama-b11900").join("libggml.so");
            assert_eq!(
                std::fs::read_link(&link).expect("read link"),
                PathBuf::from("libggml.so.0")
            );
            assert_eq!(std::fs::read(&link).expect("read through link"), b"lib");
        }
        cleanup(&dir);
    }

    /// Writes a one-file tar.gz with a hand-built header, for entry names
    /// `tar::Header::set_path` rightly refuses to create (such as `../evil`).
    fn write_raw_targz(archive: &Path, name: &str, contents: &[u8]) {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", contents.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        for byte in &mut header[148..156] {
            *byte = b' ';
        }
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|b| *b as u32).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());

        let mut tar_bytes = Vec::new();
        tar_bytes.extend_from_slice(&header);
        tar_bytes.extend_from_slice(contents);
        let padding = (512 - contents.len() % 512) % 512;
        tar_bytes.extend(std::iter::repeat_n(0u8, padding));
        tar_bytes.extend(std::iter::repeat_n(0u8, 1024));

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).expect("gzip write");
        let bytes = encoder.finish().expect("finish gzip");
        std::fs::write(archive, bytes).expect("write archive");
    }

    #[test]
    fn targz_with_dotdot_entry_fails_and_leaves_nothing() {
        let dir = temp_dir("targz-evil");
        let archive = dir.join("evil.tar.gz");
        write_raw_targz(&archive, "../evil", b"evil");

        let into = dir.join("runtime");
        let tmp = tmp_path(&into);
        let err = unpack(&archive, ArchiveKind::TarGz, &into).expect_err("should fail");
        assert_eq!(
            err,
            "The llama.cpp download contains an unsafe file path (../evil), so Writely stopped. Try again later."
        );
        assert!(!tmp.exists(), "tmp should be removed");
        assert!(!into.exists(), "into should not exist");
        assert!(!dir.join("evil").exists());
        cleanup(&dir);
    }

    #[test]
    fn targz_with_absolute_symlink_fails() {
        let dir = temp_dir("targz-symlink-evil");
        let archive = dir.join("evil.tar.gz");
        let mut builder = make_targz_builder();
        add_tar_symlink(&mut builder, "llama-b11900/link", "/etc/passwd");
        finish_targz(builder, &archive);

        let into = dir.join("runtime");
        let err = unpack(&archive, ArchiveKind::TarGz, &into).expect_err("should fail");
        assert!(err.contains("unsafe file path"), "unexpected error: {err}");
        assert!(!tmp_path(&into).exists());
        assert!(!into.exists());
        cleanup(&dir);
    }

    #[test]
    fn targz_with_escaping_symlink_fails() {
        let dir = temp_dir("targz-symlink-escape");
        let archive = dir.join("evil.tar.gz");
        let mut builder = make_targz_builder();
        add_tar_symlink(&mut builder, "link", "../../etc/passwd");
        finish_targz(builder, &archive);

        let into = dir.join("runtime");
        let err = unpack(&archive, ArchiveKind::TarGz, &into).expect_err("should fail");
        assert!(err.contains("unsafe file path"), "unexpected error: {err}");
        assert!(!tmp_path(&into).exists());
        assert!(!into.exists());
        cleanup(&dir);
    }

    fn zip_options() -> zip::write::SimpleFileOptions {
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
    }

    #[test]
    fn zip_with_nested_file_unpacks() {
        let dir = temp_dir("zip-ok");
        let archive = dir.join("runtime.zip");
        {
            let file = File::create(&archive).expect("create zip");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("a/b.txt", zip_options())
                .expect("start file");
            writer.write_all(b"hello").expect("write zip entry");
            writer.finish().expect("finish zip");
        }
        let into = dir.join("runtime");
        unpack(&archive, ArchiveKind::Zip, &into).expect("unpack should succeed");
        assert_eq!(
            std::fs::read(into.join("a").join("b.txt")).expect("read unpacked"),
            b"hello"
        );
        cleanup(&dir);
    }

    #[test]
    fn zip_with_dotdot_fails_and_leaves_nothing() {
        let dir = temp_dir("zip-evil");
        let archive = dir.join("evil.zip");
        {
            let file = File::create(&archive).expect("create zip");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("../x", zip_options())
                .expect("start file");
            writer.write_all(b"evil").expect("write zip entry");
            writer.finish().expect("finish zip");
        }
        let into = dir.join("runtime");
        let err = unpack(&archive, ArchiveKind::Zip, &into).expect_err("should fail");
        assert_eq!(
            err,
            "The llama.cpp download contains an unsafe file path (../x), so Writely stopped. Try again later."
        );
        assert!(!tmp_path(&into).exists());
        assert!(!into.exists());
        assert!(!dir.join("x").exists());
        cleanup(&dir);
    }
}
