//! Start-at-login registration, per platform, with no extra crates:
//! macOS writes a LaunchAgent plist, Linux an XDG autostart `.desktop`
//! file, and Windows the per-user `Run` registry value. All three are
//! per-user, so no administrator rights are needed, and they take effect
//! at the next login (a reboot logs the user back in).

use std::path::PathBuf;

#[cfg(any(target_os = "macos", test))]
pub const LABEL: &str = "dev.writely.Writely";

/// XML-escapes a string for use inside the LaunchAgent plist.
#[cfg(any(target_os = "macos", test))]
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The LaunchAgent plist that starts Writely at login (macOS).
#[cfg(any(target_os = "macos", test))]
pub fn launch_agent_plist(executable: &std::path::Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{}</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        escape_xml(&executable.to_string_lossy())
    )
}

/// Quotes an executable path for a `.desktop` `Exec=` line when it needs it.
#[cfg(any(target_os = "linux", test))]
fn desktop_exec_value(executable: &std::path::Path) -> String {
    let text = executable.to_string_lossy();
    if text
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text.into_owned()
    }
}

/// The XDG autostart entry that starts Writely at login (Linux).
#[cfg(any(target_os = "linux", test))]
pub fn desktop_entry(executable: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Writely\n\
         Exec={}\n\
         X-GNOME-Autostart-enabled=true\n\
         NoDisplay=false\n",
        desktop_exec_value(executable)
    )
}

/// Where the autostart file lives on macOS/Linux (`None` on Windows, which
/// uses the registry instead).
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn autostart_file_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").ok()?;
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("autostart").join("writely.desktop"));
            }
        }
        let home = std::env::var("HOME").ok()?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("autostart")
                .join("writely.desktop"),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

fn current_executable() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| format!("Could not register Writely to start at login: {e}"))
}

#[cfg(not(target_os = "windows"))]
fn file_contents_for(executable: &std::path::Path) -> String {
    #[cfg(target_os = "linux")]
    {
        desktop_entry(executable)
    }
    #[cfg(not(target_os = "linux"))]
    {
        launch_agent_plist(executable)
    }
}

/// Writes or removes the autostart file at `path`. Removing a file that is
/// already gone is not an error.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn set_enabled_at(path: &std::path::Path, enabled: bool, contents: &str) -> Result<(), String> {
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Could not register Writely to start at login ({}): {e}",
                    path.display()
                )
            })?;
        }
        std::fs::write(path, contents).map_err(|e| {
            format!(
                "Could not register Writely to start at login ({}): {e}",
                path.display()
            )
        })
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!(
                "Could not stop Writely from starting at login ({}): {e}",
                path.display()
            )),
        }
    }
}

#[cfg(target_os = "windows")]
fn set_enabled_windows(enabled: bool) -> Result<(), String> {
    use std::process::Command;
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let output = if enabled {
        let exe = current_executable()?;
        Command::new("reg")
            .args([
                "add",
                key,
                "/v",
                "Writely",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", exe.to_string_lossy()),
                "/f",
            ])
            .output()
    } else {
        Command::new("reg")
            .args(["delete", key, "/v", "Writely", "/f"])
            .output()
    };
    let output = output.map_err(|e| format!("Could not update the start-at-login setting: {e}"))?;
    // `reg delete` on a missing value exits non-zero; that is already the
    // desired state when disabling.
    if !enabled && !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("unable to find") || stderr.contains("not find") {
            return Ok(());
        }
    }
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            Err("Could not update the start-at-login setting: reg.exe failed.".to_string())
        } else {
            Err(format!(
                "Could not update the start-at-login setting: {detail}"
            ))
        }
    }
}

/// Registers or unregisters Writely to start at login, pointing at the
/// currently running executable so a moved app heals itself on next launch.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        set_enabled_windows(enabled)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let Some(path) = autostart_file_path() else {
            return Err(
                "Could not update the start-at-login setting: no home directory.".to_string(),
            );
        };
        let exe = current_executable()?;
        set_enabled_at(&path, enabled, &file_contents_for(&exe))
    }
}

/// Applies the saved setting at startup: re-registering on every launch
/// keeps the entry pointing at the current executable.
pub fn sync(launch_at_login: bool) -> Result<(), String> {
    set_enabled(launch_at_login)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn plist_starts_writely_at_login() {
        let plist = launch_agent_plist(Path::new(
            "/Applications/Writely.app/Contents/MacOS/Writely",
        ));
        assert!(plist.contains("<string>dev.writely.Writely</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<string>/Applications/Writely.app/Contents/MacOS/Writely</string>"));
    }

    #[test]
    fn plist_escapes_the_executable_path() {
        let plist = launch_agent_plist(Path::new("/tmp/A&B/Writely"));
        assert!(plist.contains("<string>/tmp/A&amp;B/Writely</string>"));
        assert!(!plist.contains("A&B"));
    }

    #[test]
    fn desktop_entry_starts_writely_at_login() {
        let entry = desktop_entry(Path::new("/usr/bin/writely"));
        assert!(entry.contains("Exec=/usr/bin/writely"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
        let spaced = desktop_entry(Path::new("/opt/my apps/writely"));
        assert!(spaced.contains("Exec=\"/opt/my apps/writely\""));
    }

    #[test]
    fn enabling_writes_and_disabling_removes_the_file() {
        let dir = std::env::temp_dir().join(format!("writely-autostart-{}", std::process::id()));
        let path = dir.join("nested").join("writely.plist");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!path.exists());
        set_enabled_at(&path, true, "contents").expect("enable should write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "contents");
        set_enabled_at(&path, false, "contents").expect("disable should remove");
        assert!(!path.exists());
        // Disabling again (already off) is not an error.
        set_enabled_at(&path, false, "contents").expect("disable should be a no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn autostart_path_ends_with_the_entry_file() {
        if let Some(path) = autostart_file_path() {
            #[cfg_attr(target_os = "windows", allow(unused_variables))]
            let text = path.to_string_lossy();
            #[cfg(target_os = "macos")]
            assert!(text.ends_with("Library/LaunchAgents/dev.writely.Writely.plist"));
            #[cfg(target_os = "linux")]
            assert!(text.ends_with("autostart/writely.desktop"));
        }
    }
}
