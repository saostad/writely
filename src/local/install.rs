#![allow(dead_code)] // Wired up in a later round (local-model task).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::config::{AppSettings, LocalModelId};

use super::catalog::{self, ArchiveKind};
use super::{download, unpack};

const ALREADY_RUNNING: &str = "A download is already running. Wait for it to finish or cancel it.";
const RUNTIME_WHAT: &str = "the llama.cpp helper";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallPhase {
    Idle,
    Downloading { what: String, done: u64, total: u64 },
    Unpacking,
    Failed(String),
}

static PHASE: Mutex<InstallPhase> = Mutex::new(InstallPhase::Idle);
static CANCEL: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

fn lock_phase() -> MutexGuard<'static, InstallPhase> {
    match PHASE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub fn install_phase() -> InstallPhase {
    lock_phase().clone()
}

fn set_phase_and_notify(phase: InstallPhase, on_change: &Arc<dyn Fn() + Send + Sync>) {
    {
        let mut guard = lock_phase();
        *guard = phase;
    }
    on_change();
}

fn runtime_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime").join(catalog::RUNTIME_VERSION)
}

fn models_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

fn server_binary_in(data_dir: &Path) -> Result<PathBuf, String> {
    Ok(runtime_dir_in(data_dir).join(catalog::runtime_file()?.server_path))
}

pub fn server_binary() -> Result<PathBuf, String> {
    server_binary_in(&catalog::local_data_dir())
}

fn runtime_ready_in(data_dir: &Path) -> bool {
    match server_binary_in(data_dir) {
        Ok(path) => path.exists(),
        Err(_) => false,
    }
}

pub fn runtime_ready() -> bool {
    runtime_ready_in(&catalog::local_data_dir())
}

fn model_path_in(data_dir: &Path, id: LocalModelId) -> PathBuf {
    models_dir_in(data_dir).join(catalog::model_file(id).file_name)
}

pub fn model_path(id: LocalModelId) -> PathBuf {
    model_path_in(&catalog::local_data_dir(), id)
}

fn model_ready_in(data_dir: &Path, id: LocalModelId) -> bool {
    match std::fs::metadata(model_path_in(data_dir, id)) {
        Ok(m) => m.len() == catalog::model_file(id).size,
        Err(_) => false,
    }
}

pub fn model_ready(id: LocalModelId) -> bool {
    model_ready_in(&catalog::local_data_dir(), id)
}

pub fn local_ready(settings: &AppSettings) -> bool {
    runtime_ready() && model_ready(settings.local_model)
}

fn asset_file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

type DownloadFn<'a> = dyn FnMut(&str, &Path, &str, u64, &str, &mut dyn FnMut(u64, u64), &AtomicBool) -> Result<(), String>
    + 'a;
type UnpackFn<'a> = dyn FnMut(&Path, ArchiveKind, &Path) -> Result<(), String> + 'a;

fn run_install_steps(
    data_dir: &Path,
    id: LocalModelId,
    cancel: &AtomicBool,
    set_phase: &mut dyn FnMut(InstallPhase),
    download_fn: &mut DownloadFn<'_>,
    unpack_fn: &mut UnpackFn<'_>,
) -> Result<(), String> {
    if !runtime_ready_in(data_dir) {
        let runtime = catalog::runtime_file()?;
        let archive = data_dir.join("runtime").join(asset_file_name(runtime.url));
        set_phase(InstallPhase::Downloading {
            what: RUNTIME_WHAT.to_string(),
            done: 0,
            total: runtime.size,
        });
        {
            let mut progress = |done: u64, total: u64| {
                set_phase(InstallPhase::Downloading {
                    what: RUNTIME_WHAT.to_string(),
                    done,
                    total,
                });
            };
            download_fn(
                runtime.url,
                &archive,
                runtime.sha256,
                runtime.size,
                RUNTIME_WHAT,
                &mut progress,
                cancel,
            )?;
        }
        set_phase(InstallPhase::Unpacking);
        unpack_fn(&archive, runtime.kind, &runtime_dir_in(data_dir))?;
        if let Err(e) = std::fs::remove_file(&archive) {
            eprintln!(
                "Writely: could not delete the downloaded llama.cpp archive ({}): {e}",
                archive.display()
            );
        }
    }
    if !model_ready_in(data_dir, id) {
        let model = catalog::model_file(id);
        set_phase(InstallPhase::Downloading {
            what: model.label.to_string(),
            done: 0,
            total: model.size,
        });
        let mut progress = |done: u64, total: u64| {
            set_phase(InstallPhase::Downloading {
                what: model.label.to_string(),
                done,
                total,
            });
        };
        download_fn(
            model.url,
            &model_path_in(data_dir, id),
            model.sha256,
            model.size,
            model.label,
            &mut progress,
            cancel,
        )?;
    }
    Ok(())
}

fn perform_install(
    data_dir: &Path,
    id: LocalModelId,
    cancel: &AtomicBool,
    set_phase: &mut dyn FnMut(InstallPhase),
    download_fn: &mut DownloadFn<'_>,
    unpack_fn: &mut UnpackFn<'_>,
) {
    match run_install_steps(data_dir, id, cancel, set_phase, download_fn, unpack_fn) {
        Ok(()) => set_phase(InstallPhase::Idle),
        Err(msg) => {
            eprintln!("Writely: the local model download failed: {msg}");
            set_phase(InstallPhase::Failed(msg));
        }
    }
}

pub fn start_install(
    id: LocalModelId,
    on_change: Arc<dyn Fn() + Send + Sync>,
) -> Result<(), String> {
    if RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(ALREADY_RUNNING.to_string());
    }
    CANCEL.store(false, Ordering::SeqCst);
    let spawn = std::thread::Builder::new()
        .name("writely-local-install".to_string())
        .spawn(move || {
            let data_dir = catalog::local_data_dir();
            let mut set_phase = |phase: InstallPhase| {
                set_phase_and_notify(phase, &on_change);
            };
            let mut real_download = |url: &str,
                                     dest: &Path,
                                     sha256: &str,
                                     size: u64,
                                     name: &str,
                                     progress: &mut dyn FnMut(u64, u64),
                                     cancel: &AtomicBool| {
                download::download_file(url, dest, sha256, size, name, progress, cancel)
            };
            let mut real_unpack = |archive: &Path, kind: ArchiveKind, into: &Path| {
                unpack::unpack(archive, kind, into)
            };
            perform_install(
                &data_dir,
                id,
                &CANCEL,
                &mut set_phase,
                &mut real_download,
                &mut real_unpack,
            );
            RUNNING.store(false, Ordering::SeqCst);
        });
    match spawn {
        Ok(_) => Ok(()),
        Err(e) => {
            RUNNING.store(false, Ordering::SeqCst);
            Err(format!("Could not start the download: {e}"))
        }
    }
}

pub fn cancel_install() {
    CANCEL.store(true, Ordering::SeqCst);
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

fn delete_model_in(data_dir: &Path, id: LocalModelId, running: bool) -> Result<(), String> {
    if running {
        return Err(ALREADY_RUNNING.to_string());
    }
    let label = catalog::model_file(id).label;
    let path = model_path_in(data_dir, id);
    for target in [path.clone(), part_path(&path)] {
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "Could not delete {label} ({}): {e}",
                    target.display()
                ));
            }
        }
    }
    Ok(())
}

pub fn delete_model(id: LocalModelId) -> Result<(), String> {
    delete_model_in(
        &catalog::local_data_dir(),
        id,
        RUNNING.load(Ordering::SeqCst),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("writely-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup(dir: &Path) {
        let result = std::fs::remove_dir_all(dir);
        assert!(result.is_ok(), "cleanup {}: {:?}", dir.display(), result);
    }

    fn make_runtime_ready(data_dir: &Path) {
        let binary = server_binary_in(data_dir).expect("server binary path");
        std::fs::create_dir_all(binary.parent().expect("parent")).expect("mkdir");
        std::fs::write(&binary, b"fake").expect("write binary");
    }

    #[test]
    fn model_ready_checks_length() {
        let dir = temp_dir("ready");
        let id = LocalModelId::Qwen35_0_8B;
        assert!(!model_ready_in(&dir, id));
        let path = model_path_in(&dir, id);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"short").expect("write");
        assert!(!model_ready_in(&dir, id));
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(catalog::model_file(id).size).expect("set_len");
        drop(file);
        assert!(model_ready_in(&dir, id));
        cleanup(&dir);
    }

    #[test]
    fn delete_model_removes_file_and_part() {
        let dir = temp_dir("delete");
        let id = LocalModelId::Qwen35_2B;
        let path = model_path_in(&dir, id);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"model").expect("write");
        std::fs::write(part_path(&path), b"part").expect("write part");
        assert_eq!(
            delete_model_in(&dir, id, true).expect_err("refuses while running"),
            ALREADY_RUNNING
        );
        assert!(path.exists());
        delete_model_in(&dir, id, false).expect("delete");
        assert!(!path.exists());
        assert!(!part_path(&path).exists());
        // Missing files are fine.
        delete_model_in(&dir, id, false).expect("delete again");
        cleanup(&dir);
    }

    #[test]
    fn install_only_downloads_model_when_runtime_ready() {
        let dir = temp_dir("model-only");
        make_runtime_ready(&dir);
        let events: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let phases: RefCell<Vec<InstallPhase>> = RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let label = catalog::model_file(LocalModelId::Qwen35_0_8B).label;
        {
            let mut set_phase = |p: InstallPhase| phases.borrow_mut().push(p);
            let mut download = |_url: &str,
                                dest: &Path,
                                _sha: &str,
                                _size: u64,
                                name: &str,
                                progress: &mut dyn FnMut(u64, u64),
                                _cancel: &AtomicBool|
             -> Result<(), String> {
                events.borrow_mut().push(format!("download:{name}"));
                std::fs::create_dir_all(dest.parent().expect("parent"))
                    .map_err(|e| e.to_string())?;
                std::fs::write(dest, b"fake").map_err(|e| e.to_string())?;
                progress(1, 2);
                Ok(())
            };
            let mut unpack_fake = |_a: &Path, _k: ArchiveKind, _i: &Path| -> Result<(), String> {
                events.borrow_mut().push("unpack".to_string());
                Ok(())
            };
            perform_install(
                &dir,
                LocalModelId::Qwen35_0_8B,
                &cancel,
                &mut set_phase,
                &mut download,
                &mut unpack_fake,
            );
        }
        assert_eq!(*events.borrow(), vec![format!("download:{label}")]);
        assert_eq!(phases.borrow().last(), Some(&InstallPhase::Idle));
        cleanup(&dir);
    }

    #[test]
    fn install_downloads_runtime_unpacks_then_model() {
        let dir = temp_dir("both");
        let events: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let phases: RefCell<Vec<InstallPhase>> = RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let label = catalog::model_file(LocalModelId::Qwen35_2B).label;
        let runtime = catalog::runtime_file().expect("runtime");
        let archive = dir.join("runtime").join(asset_file_name(runtime.url));
        {
            let mut set_phase = |p: InstallPhase| phases.borrow_mut().push(p);
            let mut download = |_url: &str,
                                dest: &Path,
                                _sha: &str,
                                _size: u64,
                                name: &str,
                                progress: &mut dyn FnMut(u64, u64),
                                _cancel: &AtomicBool|
             -> Result<(), String> {
                events.borrow_mut().push(format!("download:{name}"));
                std::fs::create_dir_all(dest.parent().expect("parent"))
                    .map_err(|e| e.to_string())?;
                std::fs::write(dest, b"fake").map_err(|e| e.to_string())?;
                progress(1, 2);
                Ok(())
            };
            let mut unpack_fake = |_a: &Path, _k: ArchiveKind, _i: &Path| -> Result<(), String> {
                events.borrow_mut().push("unpack".to_string());
                Ok(())
            };
            perform_install(
                &dir,
                LocalModelId::Qwen35_2B,
                &cancel,
                &mut set_phase,
                &mut download,
                &mut unpack_fake,
            );
        }
        assert_eq!(
            *events.borrow(),
            vec![
                format!("download:{RUNTIME_WHAT}"),
                "unpack".to_string(),
                format!("download:{label}"),
            ]
        );
        assert!(!archive.exists(), "archive deleted after unpack");
        let phases = phases.borrow();
        assert!(phases.contains(&InstallPhase::Unpacking));
        assert_eq!(phases.last(), Some(&InstallPhase::Idle));
        drop(phases);
        cleanup(&dir);
    }

    #[test]
    fn install_failure_lands_in_failed_and_skips_later_steps() {
        let dir = temp_dir("fails");
        let events: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let phases: RefCell<Vec<InstallPhase>> = RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        {
            let mut set_phase = |p: InstallPhase| phases.borrow_mut().push(p);
            let mut download = |_url: &str,
                                _dest: &Path,
                                _sha: &str,
                                _size: u64,
                                name: &str,
                                _progress: &mut dyn FnMut(u64, u64),
                                _cancel: &AtomicBool|
             -> Result<(), String> {
                events.borrow_mut().push(format!("download:{name}"));
                Err("Download cancelled.".to_string())
            };
            let mut unpack_fake = |_a: &Path, _k: ArchiveKind, _i: &Path| -> Result<(), String> {
                events.borrow_mut().push("unpack".to_string());
                Ok(())
            };
            perform_install(
                &dir,
                LocalModelId::Qwen35_0_8B,
                &cancel,
                &mut set_phase,
                &mut download,
                &mut unpack_fake,
            );
        }
        assert_eq!(*events.borrow(), vec![format!("download:{RUNTIME_WHAT}")]);
        assert_eq!(
            phases.borrow().last(),
            Some(&InstallPhase::Failed("Download cancelled.".to_string()))
        );
        cleanup(&dir);
    }
}
