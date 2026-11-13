//! The one place that talks to `adb`. Everything else goes through the `Adb` trait, so tests script a fake device.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Output {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).replace("\r\n", "\n")
    }
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

pub trait Adb {
    /// Run `adb [-s serial] <args>`. `Err` means adb itself could not be run or did not finish in time.
    fn run(&self, args: &[&str]) -> Result<Output, String>;
    /// Where adb is and which device it targets, for `status`.
    fn describe(&self) -> (String, Option<String>);
}

pub struct RealAdb {
    pub bin: PathBuf,
    pub serial: Option<String>,
    pub timeout: Duration,
}

/// `adb` from `MOLA_CUA_ADB`, then PATH, then the SDK locations an Android Studio install uses.
/// Many machines have the SDK without `platform-tools` on PATH; that must not read as "not installed".
pub fn locate_adb(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
    if let Some(p) = env("MOLA_CUA_ADB").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let mut dirs: Vec<PathBuf> = env("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(sdk) = env(var) {
            dirs.push(Path::new(&sdk).join("platform-tools"));
        }
    }
    if let Some(local) = env("LOCALAPPDATA") {
        dirs.push(Path::new(&local).join("Android").join("Sdk").join("platform-tools"));
    }
    if let Some(home) = env("HOME") {
        dirs.push(Path::new(&home).join("Android").join("Sdk").join("platform-tools"));
        dirs.push(Path::new(&home).join("Library").join("Android").join("sdk").join("platform-tools"));
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

impl Adb for RealAdb {
    fn run(&self, args: &[&str]) -> Result<Output, String> {
        let mut cmd = Command::new(&self.bin);
        if let Some(s) = &self.serial {
            cmd.args(["-s", s]);
        }
        let mut child = cmd
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.bin.display()))?;
        // Drain both pipes on threads: a full pipe would block adb and look like a hang.
        let mut out = child.stdout.take().expect("piped stdout");
        let mut err = child.stderr.take().expect("piped stderr");
        let out_t = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = out.read_to_end(&mut b);
            b
        });
        let err_t = std::thread::spawn(move || {
            let mut b = String::new();
            let _ = err.read_to_string(&mut b);
            b
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(s) => break s,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("adb {} did not finish in {} ms", args.join(" "), self.timeout.as_millis()));
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        Ok(Output {
            status: status.code().unwrap_or(-1),
            stdout: out_t.join().unwrap_or_default(),
            stderr: err_t.join().unwrap_or_default(),
        })
    }

    fn describe(&self) -> (String, Option<String>) {
        (self.bin.display().to_string(), self.serial.clone())
    }
}

/// Rows of `adb devices`: `<serial><TAB><state>`. Notices ("* daemon started", "adb server version … doesn't match")
/// have no tab and are not devices.
pub fn parse_devices(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.trim().split_once('\t'))
        .map(|(s, st)| (s.trim().to_string(), st.trim().to_string()))
        .filter(|(s, _)| !s.is_empty() && !s.contains(' '))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_rows_need_a_tab_and_notices_are_skipped() {
        let t = "* daemon not running; starting now at tcp:5037\nadb server version (41) doesn't match this client (39); killing...\nList of devices attached\nemulator-5554\tdevice\nR58M\tunauthorized\n0123\tno permissions (udev)\n\n";
        assert_eq!(
            parse_devices(t),
            vec![
                ("emulator-5554".into(), "device".into()),
                ("R58M".into(), "unauthorized".into()),
                ("0123".into(), "no permissions (udev)".into())
            ]
        );
    }

    #[test]
    fn adb_is_found_in_the_sdk_when_it_is_not_on_path() {
        let dir = std::env::temp_dir().join(format!("mola-cua-adb-{}", std::process::id()));
        let pt = dir.join("platform-tools");
        std::fs::create_dir_all(&pt).unwrap();
        let exe = pt.join(if cfg!(windows) { "adb.exe" } else { "adb" });
        std::fs::write(&exe, b"").unwrap();
        let sdk = dir.to_string_lossy().to_string();
        let env = |k: &str| (k == "ANDROID_HOME").then(|| sdk.clone());
        assert_eq!(locate_adb(&env), Some(exe));
        let none = |_: &str| None;
        assert_eq!(locate_adb(&none), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
