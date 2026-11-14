//! mola-cua: computer use on Android over adb, in agent-desktop's snapshot / ref / envelope model.
//! Commands this binary does not know are handed to `agent-desktop`, so one entry point covers desktop and Android.

// An error carries its details as JSON so it can be printed as agent-desktop prints it; errors are rare and
// built once per command, so their size is not worth boxing every call site for.
#![allow(clippy::result_large_err)]

mod adb;
mod ui;

use adb::{Adb, RealAdb, locate_adb, parse_devices};
use agent_desktop_core::response::output::ENVELOPE_VERSION;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use ui::{Identity, UiNode, parse_dump, resolve, wants_ref};

#[derive(Parser)]
#[command(name = "mola-cua", version, about = "Computer use for AI agents on Android (adb); desktop commands pass through to agent-desktop")]
struct Cli {
    /// Device serial from `mola-cua devices` (or ANDROID_SERIAL). Needed when more than one device is online.
    #[arg(long, global = true)]
    device: Option<String>,
    /// Upper bound for one adb call, in milliseconds.
    #[arg(long, global = true, default_value_t = 30_000)]
    adb_timeout_ms: u64,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// adb location and version, devices, what this tool can do
    Status,
    /// Devices adb sees, with their state
    Devices,
    /// Install an APK, replacing an older install (L3: installed)
    Install { apk: PathBuf },
    /// Start an app by package (or package/activity) and wait until it is drawn (L3: launched)
    Launch { app: String },
    /// Force-stop an app
    CloseApp { package: String },
    /// Is the app running, and did it crash since the crash log was last cleared
    Health {
        package: String,
        /// Clear the crash log first (do this before `launch`)
        #[arg(long)]
        clear: bool,
    },
    /// The screen as a tree; every element an agent can act on or read gets a ref
    Snapshot {
        /// Fail with APP_NOT_FOUND unless this package is in the foreground
        #[arg(long)]
        app: Option<String>,
        /// Interactive elements only (with the containers that hold them)
        #[arg(short, long)]
        interactive: bool,
        /// Drop unnamed wrapper layouts
        #[arg(long)]
        compact: bool,
    },
    /// Search the screen by role, name, text or resource id; matches come with fresh refs
    Find {
        #[arg(long)]
        role: Option<String>,
        /// Exact accessible name (text or content description)
        #[arg(long)]
        name: Option<String>,
        /// Substring of name or value
        #[arg(long)]
        text: Option<String>,
        /// Resource id, full (`com.x:id/login`) or the part after `id/`
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        first: bool,
        #[arg(long)]
        count: bool,
    },
    /// Read a property of an element: text, value, bounds, role, states
    Get {
        r#ref: String,
        #[arg(long, default_value = "text")]
        property: String,
    },
    /// Check a state of an element: visible, enabled, checked, focused, selected
    Is {
        r#ref: String,
        #[arg(long, default_value = "visible")]
        property: String,
    },
    /// Tap the element a ref points at
    Click { r#ref: String },
    /// Replace the content of a text field and read it back
    SetValue {
        r#ref: String,
        text: Option<String>,
        /// Read the text from stdin (keeps it out of the process list)
        #[arg(long)]
        stdin: bool,
    },
    /// Press a key: back, home, enter, tab, delete, menu, app-switch, or a keycode number
    Press { key: String },
    /// Wait until text or a resource id is on screen (or gone)
    Wait {
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        gone: bool,
        #[arg(long, default_value_t = 30_000)]
        timeout: u64,
    },
    /// Save a PNG of the screen (may hold private content: keep it local)
    Screenshot { path: PathBuf },
    /// Anything else goes to agent-desktop unchanged
    #[command(external_subcommand)]
    Desktop(Vec<String>),
}

/// An error in agent-desktop's shape. `delivered` says whether input may have reached the device.
#[derive(Debug)]
struct Fail {
    code: &'static str,
    message: String,
    suggestion: Option<String>,
    details: Option<Value>,
    delivered: bool,
}

impl Fail {
    fn new(code: agent_desktop_core::ErrorCode, message: impl Into<String>) -> Self {
        Fail { code: code.as_str(), message: message.into(), suggestion: None, details: None, delivered: false }
    }
    fn hint(mut self, s: impl Into<String>) -> Self {
        self.suggestion = Some(s.into());
        self
    }
    fn details(mut self, d: Value) -> Self {
        self.details = Some(d);
        self
    }
    fn after_input(mut self) -> Self {
        self.delivered = true;
        self
    }
}

use agent_desktop_core::ErrorCode as E;
type Res = Result<Value, Fail>;

fn envelope(command: &str, r: &Res) -> Value {
    match r {
        Ok(data) => json!({"version": ENVELOPE_VERSION, "ok": true, "command": command, "data": data}),
        Err(f) => {
            let mut e = json!({"code": f.code, "message": f.message,
                "disposition": {"delivery": if f.delivered { "delivered" } else { "not_delivered" }, "retry": if f.delivered { "unsafe" } else { "safe" }}});
            if let Some(s) = &f.suggestion { e["suggestion"] = json!(s); }
            if let Some(d) = &f.details { e["details"] = d.clone(); }
            json!({"version": ENVELOPE_VERSION, "ok": false, "command": command, "error": e})
        }
    }
}

/// Snapshots live on disk between calls, as agent-desktop's do: a ref is `@<snapshot_id>:e<N>`.
struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(home: PathBuf) -> Self {
        Store { dir: home.join("snapshots") }
    }

    fn save(&self, serial: &str, entries: &BTreeMap<String, Identity>) -> Result<String, Fail> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Fail::new(E::Internal, format!("cannot create {}: {e}", self.dir.display())))?;
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ ((std::process::id() as u128) << 64);
        let id = format!("s{}", to_base36(seed));
        let body = json!({"serial": serial, "entries": entries}).to_string();
        std::fs::write(self.dir.join(format!("{id}.json")), body).map_err(|e| Fail::new(E::Internal, format!("cannot write the snapshot: {e}")))?;
        self.prune(40);
        Ok(id)
    }

    fn prune(&self, keep: usize) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
        files.sort();
        let extra = files.len().saturating_sub(keep);
        for (_, p) in files.into_iter().take(extra) {
            let _ = std::fs::remove_file(p);
        }
    }

    fn load(&self, r: &str) -> Result<(String, Identity), Fail> {
        let bad = || Fail::new(E::InvalidArgs, format!("`{r}` is not a ref")).hint("A ref looks like @s8f3k2p9:e8 and comes from `snapshot` or `find`.");
        let (snap, entry) = r.strip_prefix('@').and_then(|x| x.split_once(':')).ok_or_else(bad)?;
        if snap.is_empty() || !snap.chars().all(|c| c.is_ascii_alphanumeric()) || !entry.starts_with('e') {
            return Err(bad());
        }
        let body = std::fs::read_to_string(self.dir.join(format!("{snap}.json")))
            .map_err(|_| Fail::new(E::SnapshotNotFound, format!("snapshot {snap} is not stored")).hint("Take a new snapshot and use its refs."))?;
        let v: Value = serde_json::from_str(&body).map_err(|e| Fail::new(E::Internal, format!("snapshot {snap} is unreadable: {e}")))?;
        let id = v["entries"].get(entry).cloned().ok_or_else(|| Fail::new(E::ElementNotFound, format!("snapshot {snap} has no {entry}")))?;
        let id: Identity = serde_json::from_value(id).map_err(|e| Fail::new(E::Internal, format!("snapshot {snap} is unreadable: {e}")))?;
        Ok((v["serial"].as_str().unwrap_or("").to_string(), id))
    }
}

fn to_base36(mut n: u128) -> String {
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while out.len() < 12 {
        out.push(digits[(n % 36) as usize]);
        n /= 36;
    }
    String::from_utf8(out).expect("ascii")
}

struct App<'a> {
    adb: &'a dyn Adb,
    store: Store,
    /// pause between polls and retries; zero in tests
    tick: Duration,
}

impl App<'_> {
    fn adb(&self, args: &[&str]) -> Result<adb::Output, Fail> {
        self.adb.run(args).map_err(|e| Fail::new(E::Timeout, e).hint("Check the device with `mola-cua devices`."))
    }

    fn shell(&self, cmd: &str) -> Result<adb::Output, Fail> {
        self.adb(&["shell", cmd])
    }

    /// The serial every command acts on. More than one online device and no choice made is an error, never a guess.
    fn serial(&self) -> Result<String, Fail> {
        let (_, chosen) = self.adb.describe();
        let devices = parse_devices(&self.adb.run(&["devices"]).map_err(|e| Fail::new(E::Internal, e))?.text());
        let online: Vec<&(String, String)> = devices.iter().filter(|d| d.1 == "device").collect();
        if let Some(s) = chosen {
            return match devices.iter().find(|d| d.0 == s) {
                Some(d) if d.1 == "device" => Ok(s),
                Some(d) => Err(Fail::new(E::PermDenied, format!("device {s} is {}", d.1)).hint("Unlock the device and accept the USB debugging prompt.")),
                None => Err(Fail::new(E::WindowNotFound, format!("device {s} is not attached")).details(json!({"devices": devices}))),
            };
        }
        match online.len() {
            1 => Ok(online[0].0.clone()),
            0 => Err(Fail::new(E::WindowNotFound, if devices.is_empty() { "no device or emulator attached".to_string() } else { "no device is ready".to_string() })
                .hint("Start an emulator (<Android SDK>/emulator/emulator -avd <name>) or plug in a device with USB debugging authorised.")
                .details(json!({"devices": devices}))),
            _ => Err(Fail::new(E::AmbiguousTarget, "more than one device is online").hint("Pass --device <serial>.").details(json!({"candidates": online.iter().map(|d| &d.0).collect::<Vec<_>>()}))),
        }
    }

    /// One UI dump. uiautomator refuses while an animation runs ("could not get idle state"): try three times.
    fn dump(&self) -> Result<UiNode, Fail> {
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(self.tick * 2);
            }
            let out = self.adb(&["exec-out", "uiautomator", "dump", "/dev/tty"])?;
            match parse_dump(&out.text()) {
                Ok(root) => return Ok(root),
                Err(e) => last = if out.stderr.trim().is_empty() { e } else { format!("{e} ({})", out.stderr.trim()) },
            }
        }
        Err(Fail::new(E::AppUnresponsive, format!("the device gave no UI tree after 3 tries: {last}")).hint("The screen may be animating, locked, or showing a secure window."))
    }

    fn element(&self, r: &str) -> Result<UiNode, Fail> {
        let (serial, id) = self.store.load(r)?;
        let now = self.serial()?;
        if serial != now {
            return Err(Fail::new(E::StaleRef, format!("{r} belongs to device {serial}, the current device is {now}")).hint("Take a new snapshot on this device."));
        }
        let root = self.dump()?;
        match resolve(&root, &id) {
            Ok(Some(n)) => Ok(n.clone()),
            Ok(None) => Err(Fail::new(E::StaleRef, format!("the element of {r} is no longer on screen")).hint("Take a new snapshot and use its refs.")),
            Err(n) => Err(Fail::new(E::AmbiguousTarget, format!("{n} elements now match {r}")).hint("Take a new snapshot and use its refs.")),
        }
    }

    fn snapshot(&self, app: Option<&str>, interactive: bool, compact: bool) -> Res {
        let serial = self.serial()?;
        let root = self.dump()?;
        if let Some(pkg) = app
            && root.package != pkg
        {
            return Err(Fail::new(E::AppNotFound, format!("{pkg} is not in the foreground")).hint(format!("mola-cua launch {pkg}")).details(json!({"foreground": root.package})));
        }
        let mut entries = BTreeMap::new();
        let mut counter = 0u32;
        let tree = render(&root, interactive, compact, &mut counter, &mut entries).unwrap_or_else(|| Value::Object(root.to_json(None)));
        let id = self.store.save(&serial, &entries)?;
        let tree = with_snapshot_id(tree, &id);
        Ok(json!({"app": root.package, "device": serial, "snapshot_id": id, "ref_count": entries.len(), "complete": true, "tree": tree}))
    }

    fn find(&self, role: Option<&str>, name: Option<&str>, text: Option<&str>, rid: Option<&str>, first: bool, count: bool) -> Res {
        if role.is_none() && name.is_none() && text.is_none() && rid.is_none() {
            return Err(Fail::new(E::InvalidArgs, "find needs at least one of --role, --name, --text, --id"));
        }
        let serial = self.serial()?;
        let root = self.dump()?;
        let mut all = Vec::new();
        root.walk(&mut all);
        let hits: Vec<&UiNode> = all.into_iter().filter(|n| n.visible() && matches(n, role, name, text, rid)).collect();
        if count {
            return Ok(json!({"count": hits.len()}));
        }
        let mut entries = BTreeMap::new();
        for (i, n) in hits.iter().enumerate() {
            entries.insert(format!("e{}", i + 1), n.identity());
        }
        let id = self.store.save(&serial, &entries)?;
        let out: Vec<Value> = hits.iter().enumerate().map(|(i, n)| Value::Object(n.to_json(Some(&format!("@{id}:e{}", i + 1))))).collect();
        if first {
            return Ok(json!({"snapshot_id": id, "match": out.into_iter().next()}));
        }
        Ok(json!({"snapshot_id": id, "count": out.len(), "matches": out}))
    }

    fn click(&self, r: &str) -> Res {
        let n = self.element(r)?;
        if !n.enabled {
            return Err(Fail::new(E::ActionFailed, format!("the element of {r} is disabled")));
        }
        let (x, y) = n.center();
        let out = self.shell(&format!("input tap {x} {y}"))?;
        if !out.ok() {
            return Err(Fail::new(E::ActionFailed, format!("input tap failed: {}", out.stderr.trim())).after_input());
        }
        Ok(json!({"ref_id": r, "action": "click", "tapped": {"x": x, "y": y}}))
    }

    fn set_value(&self, r: &str, text: &str) -> Res {
        let n = self.element(r)?;
        if !n.is_textfield() {
            return Err(Fail::new(E::ActionNotSupported, format!("the element of {r} is a {}, not a text field", n.role())));
        }
        if !text.chars().all(|c| c == ' ' || c.is_ascii_graphic()) {
            return Err(Fail::new(E::ActionNotSupported, "adb `input text` only takes printable ASCII").hint("For other text, set the field through the app's own test hook or the clipboard."));
        }
        let (x, y) = n.center();
        // Focus, select all (Ctrl+A), delete, then type. `input text` reads %s as a space.
        let typed = text.replace('\'', "'\\''").replace(' ', "%s");
        let mut steps = vec![format!("input tap {x} {y}"), "input keycombination 113 29".to_string(), "input keyevent 67".to_string()];
        if !text.is_empty() {
            steps.push(format!("input text '{typed}'"));
        }
        for step in &steps {
            let out = self.shell(step)?;
            if !out.ok() {
                return Err(Fail::new(E::ActionFailed, format!("`{}` failed: {}", step.split(' ').take(2).collect::<Vec<_>>().join(" "), out.stderr.trim())).after_input());
            }
        }
        // The field itself is the evidence. A password field cannot be read back, so say that instead of claiming success.
        let after = self.element(r).map_err(Fail::after_input)?;
        if after.password {
            return Ok(json!({"ref_id": r, "action": "set-value", "verified": false, "reason": "password fields do not expose their content"}));
        }
        if after.content() != text {
            return Err(Fail::new(E::ActionFailed, "the field does not hold the text that was typed").details(json!({"expected": text, "found": after.content()})).after_input());
        }
        Ok(json!({"ref_id": r, "action": "set-value", "verified": true, "value": after.content()}))
    }

    fn wait(&self, text: Option<&str>, rid: Option<&str>, gone: bool, timeout_ms: u64) -> Res {
        if text.is_none() && rid.is_none() {
            return Err(Fail::new(E::InvalidArgs, "wait needs --text or --id"));
        }
        self.serial()?;
        let started = Instant::now();
        let mut polls = 0u32;
        loop {
            polls += 1;
            let root = self.dump()?;
            let mut all = Vec::new();
            root.walk(&mut all);
            let present = all.iter().any(|n| n.visible() && matches(n, None, None, text, rid));
            if present != gone {
                return Ok(json!({"waited_ms": started.elapsed().as_millis() as u64, "polls": polls, "condition": if gone { "gone" } else { "present" }}));
            }
            if started.elapsed() >= Duration::from_millis(timeout_ms) {
                return Err(Fail::new(E::Timeout, format!("still {} after {timeout_ms} ms", if gone { "present" } else { "absent" })).details(json!({"text": text, "id": rid, "foreground": root.package, "polls": polls})));
            }
            std::thread::sleep(self.tick);
        }
    }

    fn launch(&self, app: &str) -> Res {
        self.serial()?;
        let component = if app.contains('/') {
            app.to_string()
        } else {
            let out = self.shell(&format!("cmd package resolve-activity --brief {}", quote(app)))?;
            match out.text().lines().map(str::trim).filter(|l| l.contains('/')).next_back() {
                Some(c) => c.to_string(),
                None => return Err(Fail::new(E::AppNotFound, format!("{app} has no launchable activity or is not installed")).hint("mola-cua install <apk>")),
            }
        };
        let out = self.shell(&format!("am start -W -n {}", quote(&component)))?;
        let t = out.text();
        if !t.contains("Status: ok") {
            // `am` prints "Error type 3" and then the sentence that says what is wrong; prefer the sentence.
            let why = t.lines().find(|l| l.trim_start().starts_with("Error:")).or_else(|| t.lines().find(|l| l.contains("Error") || l.contains("Exception"))).unwrap_or("no `Status: ok` in the output").trim().to_string();
            return Err(Fail::new(E::ActionFailed, format!("{component} did not start: {why}")).after_input());
        }
        let ms = t.lines().find_map(|l| l.trim().strip_prefix("TotalTime:")).and_then(|v| v.trim().parse::<u64>().ok());
        Ok(json!({"package": component.split('/').next(), "component": component, "status": "ok", "launch_ms": ms}))
    }

    fn health(&self, package: &str, clear: bool) -> Res {
        self.serial()?;
        if clear {
            self.adb(&["logcat", "-b", "crash", "-c"])?;
            return Ok(json!({"package": package, "crash_log": "cleared"}));
        }
        let pid = self.shell(&format!("pidof {}", quote(package)))?.text().trim().to_string();
        let log = self.adb(&["logcat", "-d", "-b", "crash"])?.text();
        let lines: Vec<&str> = log.lines().filter(|l| l.contains(package)).collect();
        Ok(json!({"package": package, "running": !pid.is_empty(), "pid": pid.split_whitespace().next(), "crashed": !lines.is_empty(), "crash_lines": lines.iter().take(20).collect::<Vec<_>>()}))
    }

    fn run(&self, cmd: &Cmd) -> Res {
        match cmd {
            Cmd::Status => {
                let (bin, serial) = self.adb.describe();
                let version = self.adb(&["version"]).map(|o| o.text().lines().next().unwrap_or("").to_string()).unwrap_or_default();
                let devices = parse_devices(&self.adb(&["devices"])?.text());
                Ok(json!({"platform": "android", "version": env!("CARGO_PKG_VERSION"), "adb": {"path": bin, "version": version},
                    "device": serial, "devices": devices.iter().map(|d| json!({"serial": d.0, "state": d.1})).collect::<Vec<_>>(),
                    "ready": devices.iter().any(|d| d.1 == "device"),
                    "commands": ["status", "devices", "install", "launch", "close-app", "health", "snapshot", "find", "get", "is", "click", "set-value", "press", "wait", "screenshot"]}))
            }
            Cmd::Devices => Ok(json!({"devices": parse_devices(&self.adb(&["devices"])?.text()).iter().map(|d| json!({"serial": d.0, "state": d.1})).collect::<Vec<_>>()})),
            Cmd::Install { apk } => {
                if !apk.is_file() {
                    return Err(Fail::new(E::InvalidArgs, format!("{} is not a file", apk.display())));
                }
                self.serial()?;
                let out = self.adb(&["install", "-r", &apk.to_string_lossy()])?;
                let t = format!("{}{}", out.text(), out.stderr);
                if !t.contains("Success") {
                    return Err(Fail::new(E::ActionFailed, format!("install failed: {}", t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim())).after_input());
                }
                Ok(json!({"apk": apk, "installed": true}))
            }
            Cmd::Launch { app } => self.launch(app),
            Cmd::CloseApp { package } => {
                self.serial()?;
                self.shell(&format!("am force-stop {}", quote(package)))?;
                Ok(json!({"package": package, "stopped": true}))
            }
            Cmd::Health { package, clear } => self.health(package, *clear),
            Cmd::Snapshot { app, interactive, compact } => self.snapshot(app.as_deref(), *interactive, *compact),
            Cmd::Find { role, name, text, id, first, count } => self.find(role.as_deref(), name.as_deref(), text.as_deref(), id.as_deref(), *first, *count),
            Cmd::Get { r#ref, property } => {
                let n = self.element(r#ref)?;
                let v = match property.as_str() {
                    "text" => json!(n.value().or_else(|| n.name())),
                    "value" => json!(n.value()),
                    "role" => json!(n.role()),
                    "states" => json!(n.states()),
                    "bounds" => n.to_json(None)["bounds"].clone(),
                    other => return Err(Fail::new(E::InvalidArgs, format!("unknown property `{other}`")).hint("text, value, role, states, bounds")),
                };
                Ok(json!({"ref_id": r#ref, "property": property, "value": v}))
            }
            Cmd::Is { r#ref, property } => {
                let n = self.element(r#ref)?;
                let v = match property.as_str() {
                    "visible" => n.visible(),
                    "enabled" => n.enabled,
                    "checked" => n.checked,
                    "focused" => n.focused,
                    "selected" => n.selected,
                    other => return Err(Fail::new(E::InvalidArgs, format!("unknown state `{other}`")).hint("visible, enabled, checked, focused, selected")),
                };
                Ok(json!({"ref_id": r#ref, "property": property, "result": v}))
            }
            Cmd::Click { r#ref } => self.click(r#ref),
            Cmd::SetValue { r#ref, text, stdin } => {
                let text = match (text, stdin) {
                    (Some(t), false) => t.clone(),
                    (None, true) => std::io::read_to_string(std::io::stdin()).map_err(|e| Fail::new(E::InvalidArgs, e.to_string()))?.trim_end_matches(['\n', '\r']).to_string(),
                    _ => return Err(Fail::new(E::InvalidArgs, "give the text as an argument or pass --stdin, not both")),
                };
                self.set_value(r#ref, &text)
            }
            Cmd::Press { key } => {
                let code = match key.to_ascii_lowercase().as_str() {
                    "back" => 4,
                    "home" => 3,
                    "enter" | "return" => 66,
                    "tab" => 61,
                    "delete" | "backspace" => 67,
                    "menu" => 82,
                    "app-switch" | "recents" => 187,
                    other => other.parse::<u32>().map_err(|_| Fail::new(E::InvalidArgs, format!("unknown key `{key}`")).hint("back, home, enter, tab, delete, menu, app-switch, or a keycode number"))?,
                };
                self.serial()?;
                let out = self.shell(&format!("input keyevent {code}"))?;
                if !out.ok() {
                    return Err(Fail::new(E::ActionFailed, out.stderr.trim().to_string()).after_input());
                }
                Ok(json!({"key": key, "keycode": code}))
            }
            Cmd::Wait { text, id, gone, timeout } => self.wait(text.as_deref(), id.as_deref(), *gone, *timeout),
            Cmd::Screenshot { path } => {
                self.serial()?;
                let out = self.adb(&["exec-out", "screencap", "-p"])?;
                if !out.stdout.starts_with(b"\x89PNG") {
                    return Err(Fail::new(E::ActionFailed, "the device did not return a PNG").hint("A secure window (FLAG_SECURE) cannot be captured."));
                }
                std::fs::write(path, &out.stdout).map_err(|e| Fail::new(E::Internal, format!("cannot write {}: {e}", path.display())))?;
                Ok(json!({"path": path, "bytes": out.stdout.len()}))
            }
            Cmd::Desktop(_) => unreachable!("handled before an adb is needed"),
        }
    }
}

/// Single-quote for the device shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn matches(n: &UiNode, role: Option<&str>, name: Option<&str>, text: Option<&str>, rid: Option<&str>) -> bool {
    role.is_none_or(|r| n.role() == r)
        && name.is_none_or(|x| n.label().as_deref() == Some(x))
        && text.is_none_or(|x| n.label().is_some_and(|v| v.contains(x)) || (!n.password && n.is_textfield() && n.content().contains(x)))
        && rid.is_none_or(|x| n.resource_id == x || n.resource_id.rsplit_once(":id/").is_some_and(|(_, tail)| tail == x))
}

/// The tree as JSON with provisional refs `e<N>`; `None` when the node and everything under it is filtered out.
fn render(n: &UiNode, interactive_only: bool, compact: bool, counter: &mut u32, entries: &mut BTreeMap<String, Identity>) -> Option<Value> {
    let keep_self = if interactive_only { n.visible() && n.interactive() } else { wants_ref(n) };
    // Number the parent before its children, so refs read in screen order.
    let key = keep_self.then(|| {
        *counter += 1;
        format!("e{counter}")
    });
    let children: Vec<Value> = n.children.iter().filter_map(|c| render(c, interactive_only, compact, counter, entries)).collect();
    if !keep_self && children.is_empty() {
        return None;
    }
    // An unnamed wrapper with one child adds depth and nothing else.
    if compact && !keep_self && children.len() == 1 {
        return children.into_iter().next();
    }
    let mut m = match &key {
        Some(k) => {
            entries.insert(k.clone(), n.identity());
            n.to_json(Some(k))
        }
        None => n.to_json(None),
    };
    if !children.is_empty() {
        m.insert("children".into(), Value::Array(children));
    }
    Some(Value::Object(m))
}

/// Refs are numbered in output order, parents before children, once the snapshot id is known.
fn with_snapshot_id(mut v: Value, id: &str) -> Value {
    fn go(v: &mut Value, id: &str) {
        if let Some(r) = v.get("ref_id").and_then(Value::as_str).map(str::to_string) {
            v["ref_id"] = json!(format!("@{id}:{r}"));
        }
        if let Some(children) = v.get_mut("children").and_then(Value::as_array_mut) {
            for c in children {
                go(c, id);
            }
        }
    }
    go(&mut v, id);
    v
}

fn command_name(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::Status => "status",
        Cmd::Devices => "devices",
        Cmd::Install { .. } => "install",
        Cmd::Launch { .. } => "launch",
        Cmd::CloseApp { .. } => "close-app",
        Cmd::Health { .. } => "health",
        Cmd::Snapshot { .. } => "snapshot",
        Cmd::Find { .. } => "find",
        Cmd::Get { .. } => "get",
        Cmd::Is { .. } => "is",
        Cmd::Click { .. } => "click",
        Cmd::SetValue { .. } => "set-value",
        Cmd::Press { .. } => "press",
        Cmd::Wait { .. } => "wait",
        Cmd::Screenshot { .. } => "screenshot",
        Cmd::Desktop(_) => "desktop",
    }
}

fn home() -> PathBuf {
    std::env::var_os("MOLA_CUA_HOME").map(PathBuf::from).unwrap_or_else(|| {
        let base = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join(".mola-cua")
    })
}

/// Hand a command to agent-desktop with our stdio; its exit code becomes ours.
fn desktop(args: &[String]) -> std::process::ExitCode {
    let bin = std::env::var("MOLA_CUA_AGENT_DESKTOP").unwrap_or_else(|_| "agent-desktop".into());
    // On Windows npm installs a .cmd launcher, which only a shell can start.
    let status = if cfg!(windows) {
        std::process::Command::new("cmd").arg("/c").arg(&bin).args(args).status()
    } else {
        std::process::Command::new(&bin).args(args).status()
    };
    match status {
        Ok(s) => std::process::ExitCode::from(s.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(e) => {
            let r: Res = Err(Fail::new(E::PlatformNotSupported, format!("`{}` is a desktop command and agent-desktop could not be started: {e}", args.first().map(String::as_str).unwrap_or("")))
                .hint("npm install -g agent-desktop"));
            println!("{}", envelope(args.first().map(String::as_str).unwrap_or("desktop"), &r));
            std::process::ExitCode::from(1)
        }
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    if let Cmd::Desktop(args) = &cli.cmd {
        return desktop(args);
    }
    let name = command_name(&cli.cmd);
    let env = |k: &str| std::env::var(k).ok();
    let Some(bin) = locate_adb(&env) else {
        let r: Res = Err(Fail::new(E::PlatformNotSupported, "adb was not found on PATH or in the Android SDK locations")
            .hint("Install Android SDK Platform-Tools, or set MOLA_CUA_ADB to the adb executable."));
        println!("{}", envelope(name, &r));
        return std::process::ExitCode::from(1);
    };
    let real = RealAdb { bin, serial: cli.device.clone().or_else(|| env("ANDROID_SERIAL")), timeout: Duration::from_millis(cli.adb_timeout_ms) };
    let app = App { adb: &real, store: Store::new(home()), tick: Duration::from_millis(500) };
    let r = app.run(&cli.cmd);
    println!("{}", envelope(name, &r));
    std::process::ExitCode::from(if r.is_ok() { 0 } else { 1 })
}

#[cfg(test)]
mod tests;
