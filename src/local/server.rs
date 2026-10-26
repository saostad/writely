#![allow(dead_code)] // Wired up in a later round (local-model task).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::config::{AppSettings, LocalModelId};

use super::{catalog, install};

#[derive(Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ServerConfig {
    model: LocalModelId,
    threads: u32,
    context: u32,
    max_reply: u32,
}

impl ServerConfig {
    fn from_settings(settings: &AppSettings) -> Self {
        ServerConfig {
            model: settings.local_model,
            threads: settings.local_threads,
            context: settings.local_context_tokens,
            max_reply: settings.local_max_reply_tokens,
        }
    }
}

struct Running {
    child: Child,
    config: ServerConfig,
    endpoint: Endpoint,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static CRASH_NOTICE: Mutex<Option<String>> = Mutex::new(None);

fn lock_running() -> MutexGuard<'static, Option<Running>> {
    match RUNNING.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_crash_notice() -> MutexGuard<'static, Option<String>> {
    match CRASH_NOTICE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub fn take_crash_notice() -> Option<String> {
    lock_crash_notice().take()
}

fn server_args(model_path: &Path, port: u16, settings: &AppSettings) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".to_string(),
        model_path.to_string_lossy().into_owned(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
        "-t".to_string(),
        settings.local_threads.to_string(),
        "-c".to_string(),
        settings.local_context_tokens.to_string(),
        "-np".to_string(),
        "1".to_string(),
        "--cache-ram".to_string(),
        "0".to_string(),
        "--reasoning".to_string(),
        "off".to_string(),
        "--no-webui".to_string(),
    ];
    for arg in catalog::model_file(settings.local_model).server_args {
        args.push(arg.to_string());
    }
    args
}

fn log_tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return "no details".to_string();
    }
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

fn read_log_tail(path: &Path, n: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => log_tail(&text, n),
        Err(e) => {
            eprintln!(
                "Writely: could not read the local model log ({}): {e}",
                path.display()
            );
            "no details".to_string()
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum WaitError {
    Exited(String),
    TimedOut,
}

fn wait_until_ready(
    probe: &mut dyn FnMut() -> bool,
    exited: &mut dyn FnMut() -> Option<String>,
    timeout: Duration,
    poll: Duration,
) -> Result<(), WaitError> {
    let start = Instant::now();
    loop {
        if let Some(detail) = exited() {
            return Err(WaitError::Exited(detail));
        }
        if probe() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(WaitError::TimedOut);
        }
        std::thread::sleep(poll);
    }
}

fn kill_and_wait(child: &mut Child) {
    if let Err(e) = child.kill() {
        eprintln!("Writely: could not stop the local model helper: {e}");
    }
    if let Err(e) = child.wait() {
        eprintln!("Writely: could not wait for the local model helper to stop: {e}");
    }
}

fn pid_file_path() -> PathBuf {
    catalog::local_data_dir().join("server.pid")
}

fn write_pid_file(pid: u32) {
    let path = pid_file_path();
    if let Err(e) = std::fs::write(&path, pid.to_string()) {
        eprintln!(
            "Writely: could not save the local model helper's process id ({}): {e}",
            path.display()
        );
    }
}

fn remove_pid_file() {
    let path = pid_file_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "Writely: could not delete the local model helper's process id file ({}): {e}",
            path.display()
        ),
    }
}

fn exit_code_text(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "unknown".to_string(),
    }
}

#[cfg(target_os = "windows")]
static JOB_HANDLE: Mutex<Option<isize>> = Mutex::new(None);

#[cfg(target_os = "windows")]
fn assign_child_to_job(child: &Child) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    fn job_handle() -> Result<isize, windows::core::Error> {
        let mut guard = match JOB_HANDLE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(handle) = *guard {
            return Ok(handle);
        }
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null())?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            let raw = job.0 as isize;
            *guard = Some(raw);
            Ok(raw)
        }
    }

    let result = job_handle().and_then(|raw| unsafe {
        let job = HANDLE(raw as *mut c_void);
        let process = HANDLE(child.as_raw_handle() as *mut c_void);
        AssignProcessToJobObject(job, process)
    });
    if let Err(e) = result {
        eprintln!(
            "Writely: could not put the local model helper in a job object ({e}); the helper is still running, but it may keep running if Writely quits unexpectedly."
        );
    }
}

pub fn ensure_running(settings: &AppSettings) -> Result<Endpoint, String> {
    crate::config::validate_local_settings(settings)?;
    let wanted = ServerConfig::from_settings(settings);
    let mut guard = lock_running();

    let mut stop_old = false;
    let mut crashed: Option<String> = None;
    if let Some(running) = guard.as_mut() {
        match running.child.try_wait() {
            Ok(None) => {
                if running.config == wanted {
                    return Ok(running.endpoint.clone());
                }
                stop_old = true;
            }
            Ok(Some(status)) => {
                crashed = Some(format!(
                    "The local model stopped unexpectedly (exit code {}), so Writely restarted it.",
                    exit_code_text(status)
                ));
            }
            Err(e) => {
                return Err(format!(
                    "Could not check whether the local model is running: {e}"
                ));
            }
        }
    }
    if stop_old {
        let mut old = guard.take().expect("running checked above");
        kill_and_wait(&mut old.child);
        remove_pid_file();
    }
    if let Some(notice) = crashed {
        eprintln!("Writely: {notice}");
        *lock_crash_notice() = Some(notice);
        *guard = None;
        remove_pid_file();
    }

    if !install::runtime_ready() || !install::model_ready(settings.local_model) {
        return Err("The local model isn't downloaded yet. Open Settings, choose On this computer, and click Download.".to_string());
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("Could not start the local model: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Could not start the local model: {e}"))?
        .port();
    drop(listener);

    let mut key_bytes = [0u8; 32];
    getrandom::fill(&mut key_bytes).map_err(|e| format!("Could not start the local model: {e}"))?;
    let mut api_key = String::with_capacity(64);
    for byte in key_bytes.iter() {
        api_key.push_str(&format!("{byte:02x}"));
    }

    let binary = install::server_binary()?;
    let model_path = install::model_path(settings.local_model);
    let data_dir = catalog::local_data_dir();
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        format!(
            "Could not start the local model: could not create {}: {e}",
            data_dir.display()
        )
    })?;
    let log_path = data_dir.join("server.log");
    let log_file = std::fs::File::create(&log_path).map_err(|e| {
        format!(
            "Could not start the local model: could not create the log file {}: {e}",
            log_path.display()
        )
    })?;
    let log_file_err = log_file.try_clone().map_err(|e| {
        format!(
            "Could not start the local model: could not create the log file {}: {e}",
            log_path.display()
        )
    })?;

    let mut command = Command::new(&binary);
    command
        .args(server_args(&model_path, port, settings))
        .env("LLAMA_API_KEY", &api_key)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));
    if let Some(parent) = binary.parent() {
        command.current_dir(parent);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("Could not start the local model: {e}"))?;
    #[cfg(target_os = "windows")]
    assign_child_to_job(&child);
    write_pid_file(child.id());

    let base_url = format!("http://127.0.0.1:{port}");
    let endpoint = Endpoint {
        base_url: base_url.clone(),
        api_key: api_key.clone(),
    };
    *guard = Some(Running {
        child,
        config: wanted,
        endpoint,
    });

    let agent_config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(2)))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(agent_config);
    let health_url = format!("{base_url}/health");
    let wait_result = {
        let running = guard.as_mut().expect("running just stored");
        let mut probe = || match agent
            .get(&health_url)
            .header("Authorization", &format!("Bearer {api_key}"))
            .call()
        {
            Ok(response) => response.status().as_u16() == 200,
            Err(_) => false,
        };
        let mut exited = || match running.child.try_wait() {
            Ok(Some(status)) => Some(format!("exit code {}", exit_code_text(status))),
            Ok(None) => None,
            Err(e) => Some(format!("could not check whether it is running: {e}")),
        };
        wait_until_ready(
            &mut probe,
            &mut exited,
            Duration::from_secs(settings.local_startup_timeout_secs as u64),
            Duration::from_millis(100),
        )
    };

    match wait_result {
        Ok(()) => {
            let running = guard.as_ref().expect("running just stored");
            Ok(running.endpoint.clone())
        }
        Err(WaitError::Exited(detail)) => {
            *guard = None;
            remove_pid_file();
            let tail = read_log_tail(&log_path, 5);
            Err(format!(
                "The local model stopped while starting ({detail}). Details: {tail}"
            ))
        }
        Err(WaitError::TimedOut) => {
            if let Some(mut running) = guard.take() {
                kill_and_wait(&mut running.child);
            }
            remove_pid_file();
            let tail = read_log_tail(&log_path, 5);
            Err(format!(
                "The local model didn't start within {} seconds. You can raise the startup time limit in Settings. Details: {tail}",
                settings.local_startup_timeout_secs
            ))
        }
    }
}

pub fn stop(reason: &str) {
    let mut guard = lock_running();
    let had_child = guard.is_some();
    if let Some(mut running) = guard.take() {
        kill_and_wait(&mut running.child);
    }
    drop(guard);
    remove_pid_file();
    if had_child {
        eprintln!("Writely: stopped the local model helper ({reason}).");
    }
}

pub fn is_running() -> bool {
    let mut guard = lock_running();
    match guard.as_mut() {
        Some(running) => match running.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(e) => {
                eprintln!("Writely: could not check whether the local model is running: {e}");
                false
            }
        },
        None => false,
    }
}

#[cfg(target_os = "linux")]
fn pid_is_llama_server(pid: u32) -> bool {
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(exe) => exe.ends_with("llama-server"),
        Err(e) => {
            eprintln!(
                "Writely: could not check what process {pid} is, so it was left running: {e}"
            );
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn pid_is_llama_server(pid: u32) -> bool {
    match Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .trim()
            .ends_with("llama-server"),
        Err(e) => {
            eprintln!(
                "Writely: could not check what process {pid} is, so it was left running: {e}"
            );
            false
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn pid_is_llama_server(pid: u32) -> bool {
    eprintln!(
        "Writely: cannot check what process {pid} is on this system, so it was left running."
    );
    false
}

#[cfg(target_os = "windows")]
pub fn cleanup_stale_helper() {}

#[cfg(not(target_os = "windows"))]
pub fn cleanup_stale_helper() {
    let path = pid_file_path();
    if !path.exists() {
        return;
    }
    let pid: Option<u32> = match std::fs::read_to_string(&path) {
        Ok(text) => match text.trim().parse::<u32>() {
            Ok(pid) => Some(pid),
            Err(e) => {
                eprintln!(
                    "Writely: the saved local model helper process id ({}) was unreadable, so it was ignored: {e}",
                    path.display()
                );
                None
            }
        },
        Err(e) => {
            eprintln!(
                "Writely: could not read the local model helper's process id file ({}): {e}",
                path.display()
            );
            None
        }
    };
    if let Some(pid) = pid {
        if pid_is_llama_server(pid) {
            eprintln!(
                "Writely: stopping a leftover local model helper (process {pid}) from an earlier run."
            );
            match Command::new("kill").arg(pid.to_string()).status() {
                Ok(status) if status.success() => {
                    eprintln!("Writely: stopped the leftover local model helper (process {pid}).");
                }
                Ok(status) => {
                    eprintln!(
                        "Writely: could not stop the leftover local model helper (process {pid}): kill finished with {status}."
                    );
                }
                Err(e) => {
                    eprintln!(
                        "Writely: could not stop the leftover local model helper (process {pid}): {e}"
                    );
                }
            }
        } else {
            eprintln!(
                "Writely: process {pid} from the saved process id file is not the local model helper, so it was left running."
            );
        }
    }
    remove_pid_file();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_for(id: LocalModelId) -> AppSettings {
        AppSettings {
            local_model: id,
            local_threads: 3,
            local_context_tokens: 8192,
            ..AppSettings::default()
        }
    }

    #[test]
    fn server_args_for_0_8b_have_no_mtp_and_no_key() {
        let settings = settings_for(LocalModelId::Qwen35_0_8B);
        let args = server_args(Path::new("/tmp/model.gguf"), 1234, &settings);
        assert_eq!(args[0], "-m");
        assert_eq!(args[1], "/tmp/model.gguf");
        assert!(args.contains(&"--reasoning".to_string()));
        assert!(args.contains(&"off".to_string()));
        assert!(args.contains(&"--no-webui".to_string()));
        let np = args.iter().position(|a| a == "-np").expect("-np");
        assert_eq!(args[np + 1], "1");
        let t = args.iter().position(|a| a == "-t").expect("-t");
        assert_eq!(args[t + 1], "3");
        let c = args.iter().position(|a| a == "-c").expect("-c");
        assert_eq!(args[c + 1], "8192");
        let port = args.iter().position(|a| a == "--port").expect("--port");
        assert_eq!(args[port + 1], "1234");
        assert!(!args.contains(&"--spec-type".to_string()));
        assert!(!args.contains(&"draft-mtp".to_string()));
        let key = "0123456789abcdef0123456789abcdef";
        assert!(args.iter().all(|a| !a.contains(key)));
    }

    #[test]
    fn server_args_for_2b_end_with_mtp() {
        let settings = settings_for(LocalModelId::Qwen35_2B);
        let args = server_args(Path::new("/tmp/model.gguf"), 9, &settings);
        assert_eq!(
            &args[args.len() - 2..],
            &["--spec-type".to_string(), "draft-mtp".to_string()]
        );
    }

    #[test]
    fn wait_ready_on_third_probe() {
        let mut probes = 0;
        let mut probe = || {
            probes += 1;
            probes >= 3
        };
        let mut exited = || None;
        let result = wait_until_ready(
            &mut probe,
            &mut exited,
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(probes, 3);
    }

    #[test]
    fn wait_reports_exit() {
        let mut probe = || false;
        let mut exited = || Some("exit code 1".to_string());
        let result = wait_until_ready(
            &mut probe,
            &mut exited,
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(result, Err(WaitError::Exited("exit code 1".to_string())));
    }

    #[test]
    fn wait_times_out() {
        let mut probe = || false;
        let mut exited = || None;
        let result = wait_until_ready(
            &mut probe,
            &mut exited,
            Duration::from_millis(50),
            Duration::from_millis(5),
        );
        assert_eq!(result, Err(WaitError::TimedOut));
    }

    #[test]
    fn log_tail_joins_last_non_empty_lines() {
        assert_eq!(log_tail("a\n\nb\n   \nc\n", 5), "a | b | c");
        assert_eq!(log_tail("a\nb\nc\nd\n", 2), "c | d");
        assert_eq!(log_tail("only\n", 5), "only");
        assert_eq!(log_tail("", 5), "no details");
        assert_eq!(log_tail("\n\n  \n", 5), "no details");
    }

    #[test]
    fn endpoint_debug_hides_key() {
        let endpoint = Endpoint {
            base_url: "http://127.0.0.1:42".to_string(),
            api_key: "super-secret-key".to_string(),
        };
        let text = format!("{endpoint:?}");
        assert!(text.contains("http://127.0.0.1:42"));
        assert!(!text.contains("super-secret-key"));
    }
}
