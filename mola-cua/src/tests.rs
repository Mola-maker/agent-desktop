//! Command tests against a scripted device: no adb, no emulator.

use super::*;
use std::cell::RefCell;

/// Answers adb calls from a script and records every call. The screen is whatever `screen` holds when a dump is asked for.
struct FakeAdb {
    devices: String,
    screen: RefCell<String>,
    calls: RefCell<Vec<String>>,
    /// replies for `shell <cmd>` by prefix
    shell: Vec<(&'static str, &'static str)>,
    /// when a shell command with this prefix runs, the screen becomes this
    on: RefCell<Vec<(&'static str, String)>>,
    serial: Option<String>,
    /// a call whose joined arguments start with this never comes back (adb's own timeout error)
    hangs_on: Option<&'static str>,
    /// exit status and output for calls starting with a prefix (default: 0 and the scripted reply)
    exits: Vec<(&'static str, i32, &'static str)>,
}

impl FakeAdb {
    fn new() -> Self {
        FakeAdb {
            devices: "List of devices attached\nemulator-5554\tdevice\n".into(),
            screen: RefCell::new(ui::SAMPLE.to_string()),
            calls: RefCell::new(Vec::new()),
            shell: vec![],
            on: RefCell::new(vec![]),
            serial: None,
            hangs_on: None,
            exits: vec![],
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
    fn shell_calls(&self) -> Vec<String> {
        self.calls().into_iter().filter_map(|c| c.strip_prefix("shell ").map(str::to_string)).collect()
    }
}

impl Adb for FakeAdb {
    fn run(&self, args: &[&str]) -> Result<adb::Output, String> {
        let line = args.join(" ");
        self.calls.borrow_mut().push(line.clone());
        let out = |s: &str| Ok(adb::Output { status: 0, stdout: s.as_bytes().to_vec(), stderr: String::new() });
        if self.hangs_on.is_some_and(|p| line.starts_with(p)) {
            return Err(format!("adb {} did not finish in 30000 ms", adb::brief(args)));
        }
        if let Some((_, status, text)) = self.exits.iter().find(|(p, _, _)| line.starts_with(p)) {
            return Ok(adb::Output { status: *status, stdout: text.as_bytes().to_vec(), stderr: String::new() });
        }
        if line == "devices" {
            return out(&self.devices);
        }
        if line.starts_with("exec-out uiautomator dump") {
            return out(&self.screen.borrow());
        }
        if let Some(cmd) = line.strip_prefix("shell ") {
            let next = self.on.borrow().iter().find(|(p, _)| cmd.starts_with(p)).map(|(_, s)| s.clone());
            if let Some(s) = next {
                *self.screen.borrow_mut() = s;
            }
            return out(self.shell.iter().find(|(p, _)| cmd.starts_with(p)).map(|(_, r)| *r).unwrap_or(""));
        }
        out("")
    }
    fn describe(&self) -> (String, Option<String>) {
        ("/fake/adb".into(), self.serial.clone())
    }
}

fn app<'a>(adb: &'a FakeAdb, tag: &str) -> App<'a> {
    // One directory per test run; the directories earlier runs left behind are removed here.
    let base = std::env::temp_dir().join("mola-cua-tests");
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a.as_secs() > 600);
            if old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let dir = base.join(format!("{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    App { adb, store: Store::new(dir), tick: Duration::ZERO }
}

fn refs(v: &Value, out: &mut Vec<(String, String)>) {
    if let Some(r) = v.get("ref_id").and_then(Value::as_str) {
        out.push((r.to_string(), v.get("name").and_then(Value::as_str).unwrap_or("").to_string()));
    }
    for c in v.get("children").and_then(Value::as_array).into_iter().flatten() {
        refs(c, out);
    }
}

fn ref_of(snapshot: &Value, name: &str) -> String {
    let mut all = Vec::new();
    refs(&snapshot["tree"], &mut all);
    all.into_iter().find(|(_, n)| n == name).unwrap_or_else(|| panic!("no element named {name}")).0
}

fn code(r: &Res) -> &'static str {
    r.as_ref().err().map(|f| f.code).unwrap_or("OK")
}

#[test]
fn snapshot_gives_qualified_refs_and_the_envelope_matches_agent_desktop() {
    let adb = FakeAdb::new();
    let a = app(&adb, "snap");
    let r = a.snapshot(Some("com.example.app"), false, false);
    let env = envelope("snapshot", &r);
    assert_eq!((env["version"].as_str(), env["ok"].as_bool(), env["command"].as_str()), (Some("2.6"), Some(true), Some("snapshot")));
    let d = &env["data"];
    assert_eq!(d["app"], "com.example.app");
    assert_eq!(d["device"], "emulator-5554");
    let id = d["snapshot_id"].as_str().unwrap();
    let mut all = Vec::new();
    refs(&d["tree"], &mut all);
    assert_eq!(all.len() as u64, d["ref_count"].as_u64().unwrap());
    assert!(all.iter().all(|(r, _)| r.starts_with(&format!("@{id}:e"))), "{all:?}");
    // parents are numbered before their children
    assert!(all.iter().map(|(r, _)| r.rsplit('e').next().unwrap().parse::<u32>().unwrap()).is_sorted(), "{all:?}");
    assert!(!env.to_string().contains("hunter2"), "a password reached the output");
}

#[test]
fn interactive_snapshot_drops_plain_text_and_compact_drops_wrappers() {
    let adb = FakeAdb::new();
    let a = app(&adb, "snap-i");
    let full = a.snapshot(None, false, false).unwrap();
    let inter = a.snapshot(None, true, true).unwrap();
    let (mut f, mut i) = (Vec::new(), Vec::new());
    refs(&full["tree"], &mut f);
    refs(&inter["tree"], &mut i);
    assert!(f.iter().any(|(_, n)| n == "Sign in"));
    assert!(!i.iter().any(|(_, n)| n == "Sign in"));
    assert!(i.iter().any(|(_, n)| n == "Log in"));
    assert!(i.len() < f.len());
}

#[test]
fn snapshot_of_an_app_that_is_not_in_front_names_what_is() {
    let adb = FakeAdb::new();
    let r = app(&adb, "front").snapshot(Some("com.other"), false, false);
    let f = r.as_ref().err().unwrap();
    assert_eq!(f.code, "APP_NOT_FOUND");
    assert_eq!(f.details.as_ref().unwrap()["foreground"], "com.example.app");
}

#[test]
fn click_taps_the_centre_of_where_the_element_is_now() {
    let adb = FakeAdb::new();
    let a = app(&adb, "click");
    let login = ref_of(&a.snapshot(None, true, false).unwrap(), "Log in");
    // the button moved 100 px down since the snapshot
    *adb.screen.borrow_mut() = ui::SAMPLE.replace("[40,700][1040,820]", "[40,800][1040,920]");
    let r = a.click(&login).unwrap();
    assert_eq!(r["tapped"], json!({"x": 540, "y": 860}));
    assert_eq!(adb.shell_calls(), vec!["input tap 540 860"]);
}

#[test]
fn a_ref_whose_element_is_gone_is_stale_and_nothing_is_tapped() {
    let adb = FakeAdb::new();
    let a = app(&adb, "stale");
    let login = ref_of(&a.snapshot(None, true, false).unwrap(), "Log in");
    *adb.screen.borrow_mut() = ui::SAMPLE.replace("text=\"Log in\"", "text=\"Loading\"");
    let r = a.click(&login);
    assert_eq!(code(&r), "STALE_REF");
    assert!(adb.shell_calls().is_empty());
    let env = envelope("click", &r);
    assert_eq!(env["error"]["disposition"], json!({"delivery": "not_delivered", "retry": "safe"}));
}

#[test]
fn bad_and_unknown_refs_are_told_apart() {
    let adb = FakeAdb::new();
    let a = app(&adb, "refs");
    assert_eq!(code(&a.click("e8")), "INVALID_ARGS");
    assert_eq!(code(&a.click("@../../etc:e1")), "INVALID_ARGS");
    assert_eq!(code(&a.click("@snever:e1")), "SNAPSHOT_NOT_FOUND");
    let login = ref_of(&a.snapshot(None, true, false).unwrap(), "Log in");
    let other = format!("{}:e999", login.split(':').next().unwrap());
    assert_eq!(code(&a.click(&other)), "ELEMENT_NOT_FOUND");
}

#[test]
fn set_value_clears_types_and_reads_the_field_back() {
    let adb = FakeAdb::new();
    adb.on.borrow_mut().push(("input text", ui::SAMPLE.replace("text=\"ada\"", "text=\"it's me\"")));
    let a = app(&adb, "setv");
    let user = a.find(None, None, None, Some("user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    let r = a.set_value(&user, "it's me").unwrap();
    assert_eq!((r["verified"].as_bool(), r["value"].as_str()), (Some(true), Some("it's me")));
    assert_eq!(adb.shell_calls(), vec!["input tap 540 400", "input keycombination 113 29", "input keyevent 67", r"input text 'it'\''s%sme'"]);
}

#[test]
fn set_value_fails_when_the_field_does_not_hold_the_text_and_says_input_was_sent() {
    let adb = FakeAdb::new();
    let a = app(&adb, "setv-bad");
    let user = a.find(None, None, None, Some("com.example.app:id/user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    let r = a.set_value(&user, "grace");
    assert_eq!(code(&r), "ACTION_FAILED");
    let env = envelope("set-value", &r);
    assert_eq!(env["error"]["disposition"]["retry"], "unsafe");
    assert_eq!(env["error"]["details"], json!({"expected": "grace", "found": "ada"}));
}

#[test]
fn set_value_refuses_what_adb_cannot_type_and_what_is_not_a_field() {
    let adb = FakeAdb::new();
    let a = app(&adb, "setv-no");
    let snap = a.snapshot(None, true, false).unwrap();
    let user = a.find(None, None, None, Some("user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    assert_eq!(code(&a.set_value(&user, "你好")), "ACTION_NOT_SUPPORTED");
    assert_eq!(code(&a.set_value(&ref_of(&snap, "Log in"), "x")), "ACTION_NOT_SUPPORTED");
    assert!(adb.shell_calls().is_empty());
}

#[test]
fn a_password_field_is_typed_but_reported_as_not_verified() {
    let adb = FakeAdb::new();
    let a = app(&adb, "pass");
    let pass = a.find(None, None, None, Some("pass"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    let r = a.set_value(&pass, "s3cret").unwrap();
    assert_eq!(r["verified"], false);
    assert!(!r.to_string().contains("s3cret") && !r.to_string().contains("hunter2"));
}

#[test]
fn find_filters_counts_and_does_not_search_password_content() {
    let adb = FakeAdb::new();
    let a = app(&adb, "find");
    assert_eq!(a.find(Some("button"), None, None, None, false, true).unwrap()["count"], 1);
    assert_eq!(a.find(None, Some("Row"), None, None, false, false).unwrap()["count"], 2);
    assert_eq!(a.find(None, None, Some("ad"), None, false, true).unwrap()["count"], 1);
    assert_eq!(a.find(None, None, Some("hunter"), None, false, true).unwrap()["count"], 0);
    assert!(a.find(None, Some("Nope"), None, None, true, false).unwrap()["match"].is_null());
    assert_eq!(code(&a.find(None, None, None, None, false, false)), "INVALID_ARGS");
}

#[test]
fn get_and_is_read_the_live_element() {
    let adb = FakeAdb::new();
    let a = app(&adb, "get");
    let user = a.find(None, None, None, Some("user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    *adb.screen.borrow_mut() = ui::SAMPLE.replace("text=\"ada\"", "text=\"grace\"");
    let get = |p: &str| a.run(&Cmd::Get { r#ref: user.clone(), property: p.into() });
    assert_eq!(get("text").unwrap()["value"], "grace");
    assert_eq!(get("role").unwrap()["value"], "textfield");
    assert_eq!(code(&get("colour")), "INVALID_ARGS");
    let is = a.run(&Cmd::Is { r#ref: user.clone(), property: "focused".into() }).unwrap();
    assert_eq!(is["result"], true);
}

#[test]
fn several_devices_need_a_choice_and_an_unauthorized_one_is_not_ready() {
    let mut adb = FakeAdb::new();
    adb.devices = "List of devices attached\nemulator-5554\tdevice\nR58M\tdevice\n".into();
    let r = app(&adb, "multi").snapshot(None, false, false);
    assert_eq!(code(&r), "AMBIGUOUS_TARGET");
    assert_eq!(r.as_ref().err().unwrap().details.as_ref().unwrap()["candidates"], json!(["emulator-5554", "R58M"]));
    adb.serial = Some("R58M".into());
    assert!(app(&adb, "multi2").snapshot(None, false, false).is_ok());
    adb.devices = "List of devices attached\nR58M\tunauthorized\n".into();
    assert_eq!(code(&app(&adb, "multi3").snapshot(None, false, false)), "PERM_DENIED");
    adb.serial = None;
    assert_eq!(code(&app(&adb, "multi4").snapshot(None, false, false)), "WINDOW_NOT_FOUND");
}

#[test]
fn a_ref_from_another_device_is_stale() {
    let mut adb = FakeAdb::new();
    let a = app(&adb, "dev");
    let login = ref_of(&a.snapshot(None, true, false).unwrap(), "Log in");
    let dir = a.store.dir.parent().unwrap().to_path_buf();
    adb.devices = "List of devices attached\nOTHER\tdevice\n".into();
    let b = App { adb: &adb, store: Store::new(dir), tick: Duration::ZERO };
    assert_eq!(code(&b.click(&login)), "STALE_REF");
}

#[test]
fn launch_resolves_the_activity_and_requires_status_ok() {
    let mut adb = FakeAdb::new();
    adb.shell = vec![("cmd package resolve-activity", "priority=0 preferredOrder=0\ncom.example.app/.MainActivity\n"), ("am start", "Starting: Intent\nStatus: ok\nLaunchState: COLD\nTotalTime: 412\nComplete\n")];
    let r = app(&adb, "launch").launch("com.example.app").unwrap();
    assert_eq!((r["component"].as_str(), r["launch_ms"].as_u64()), (Some("com.example.app/.MainActivity"), Some(412)));
    assert_eq!(adb.shell_calls()[1], "am start -W -n 'com.example.app/.MainActivity'");

    adb.shell = vec![("cmd package resolve-activity", "No activity found\n")];
    assert_eq!(code(&app(&adb, "launch2").launch("com.missing")), "APP_NOT_FOUND");
    adb.shell = vec![("am start", "Error type 3\nError: Activity class {x/y} does not exist.\n")];
    let r = app(&adb, "launch3").launch("x/y");
    assert_eq!(code(&r), "ACTION_FAILED");
    assert!(r.as_ref().err().unwrap().message.contains("does not exist"));
}

#[test]
fn wait_returns_when_the_condition_holds_and_times_out_with_what_is_in_front() {
    let adb = FakeAdb::new();
    let a = app(&adb, "wait");
    assert_eq!(a.wait(Some("Sign in"), None, false, 1000).unwrap()["condition"], "present");
    assert_eq!(a.wait(Some("Welcome"), None, true, 1000).unwrap()["condition"], "gone");
    let r = a.wait(Some("Welcome"), None, false, 0);
    assert_eq!(code(&r), "TIMEOUT");
    assert_eq!(r.as_ref().err().unwrap().details.as_ref().unwrap()["foreground"], "com.example.app");
    assert_eq!(code(&a.wait(None, None, false, 0)), "INVALID_ARGS");
}

#[test]
fn a_device_that_gives_no_tree_is_reported_after_three_tries() {
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = "ERROR: could not get idle state.\n".into();
    let r = app(&adb, "idle").snapshot(None, false, false);
    assert_eq!(code(&r), "APP_UNRESPONSIVE");
    assert_eq!(adb.calls().iter().filter(|c| c.starts_with("exec-out uiautomator")).count(), 3);
}

#[test]
fn health_reports_running_and_crash_lines_for_the_package_only() {
    struct Crashy(FakeAdb);
    impl Adb for Crashy {
        fn run(&self, args: &[&str]) -> Result<adb::Output, String> {
            if args.starts_with(&["logcat", "-d"]) {
                return Ok(adb::Output { status: 0, stdout: b"E AndroidRuntime: Process: com.example.app, PID: 77\nE AndroidRuntime: Process: com.other, PID: 9\n".to_vec(), stderr: String::new() });
            }
            self.0.run(args)
        }
        fn describe(&self) -> (String, Option<String>) {
            self.0.describe()
        }
    }
    let mut fake = FakeAdb::new();
    fake.shell = vec![("pidof", "4242\n")];
    let adb = Crashy(fake);
    let dir = std::env::temp_dir().join("mola-cua-tests").join(format!("{}-health", std::process::id()));
    let a = App { adb: &adb, store: Store::new(dir), tick: Duration::ZERO };
    let r = a.health("com.example.app", false).unwrap();
    assert_eq!((r["running"].as_bool(), r["crashed"].as_bool(), r["crash_lines"].as_array().unwrap().len()), (Some(true), Some(true), 1));
}

#[test]
fn press_maps_names_to_keycodes() {
    let adb = FakeAdb::new();
    let a = app(&adb, "press");
    a.run(&Cmd::Press { key: "Back".into() }).unwrap();
    a.run(&Cmd::Press { key: "66".into() }).unwrap();
    assert_eq!(adb.shell_calls(), vec!["input keyevent 4", "input keyevent 66"]);
    assert_eq!(code(&a.run(&Cmd::Press { key: "banana".into() })), "INVALID_ARGS");
}

// ---------------------------------------------------------------- review round 1

fn disposition(r: &Res) -> (String, String) {
    let e = envelope("x", r);
    (e["error"]["disposition"]["delivery"].as_str().unwrap().to_string(), e["error"]["disposition"]["retry"].as_str().unwrap().to_string())
}

#[test]
fn an_error_envelope_is_core_s_own_shape_in_core_s_key_order() {
    let adb = FakeAdb::new();
    let a = app(&adb, "shape");
    let stale = {
        let login = ref_of(&a.snapshot(None, true, false).unwrap(), "Log in");
        *adb.screen.borrow_mut() = ui::SAMPLE.replace("text=\"Log in\"", "text=\"Loading\"");
        envelope("click", &a.click(&login))
    };
    let keys: Vec<&String> = stale["error"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["code", "message", "suggestion", "recovery", "disposition"]);
    assert_eq!(stale["error"]["recovery"]["strategy"], "refresh_snapshot_then_retry_original");
    // every delivery value is one core defines
    let p: agent_desktop_core::DeliveryDisposition = serde_json::from_value(stale["error"]["disposition"]["delivery"].clone()).unwrap();
    assert_eq!(p, agent_desktop_core::DeliveryDisposition::NotDelivered);
}

#[test]
fn a_tap_that_does_not_come_back_is_not_reported_as_not_delivered() {
    let mut adb = FakeAdb::new();
    let login = ref_of(&app(&adb, "hang0").snapshot(None, true, false).unwrap(), "Log in");
    let dir = std::env::temp_dir().join("mola-cua-tests").join(format!("{}-hang0", std::process::id()));
    adb.hangs_on = Some("shell input tap");
    let a = App { adb: &adb, store: Store::new(dir.clone()), tick: Duration::ZERO };
    let r = a.click(&login);
    assert_eq!(code(&r), "TIMEOUT");
    assert_eq!(disposition(&r), ("delivery_uncertain".to_string(), "unsafe".to_string()));
    assert!(envelope("click", &r)["error"].get("recovery").is_none(), "no retry hint when input may have landed");

    // set-value: the tap landed, typing did not come back → input was delivered
    adb.hangs_on = Some("shell input text");
    let b = App { adb: &adb, store: Store::new(dir), tick: Duration::ZERO };
    let user = b.find(None, None, None, Some("user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    let r = b.set_value(&user, "s3cret");
    assert_eq!(disposition(&r).0, "delivered_unverified");
    assert!(!envelope("set-value", &r).to_string().contains("s3cret"), "the typed text reached the error output");
}

#[test]
fn a_key_press_that_does_not_come_back_is_uncertain() {
    let mut adb = FakeAdb::new();
    adb.hangs_on = Some("shell input keyevent");
    let r = app(&adb, "hangkey").run(&Cmd::Press { key: "back".into() });
    assert_eq!(disposition(&r).0, "delivery_uncertain");
}

#[test]
fn a_row_named_by_its_text_is_stale_once_another_row_is_there_and_nothing_is_tapped() {
    let row = |t: &str, y: i32| format!("<node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,{y}][100,{}]\"><node class=\"android.widget.TextView\" package=\"p\" text=\"{t}\" resource-id=\"android:id/title\" content-desc=\"\" enabled=\"true\" bounds=\"[0,{y}][100,{}]\" /></node>", y + 100, y + 50);
    let screen = |a: &str, b: &str| format!("<hierarchy rotation=\"0\"><node class=\"android.widget.ListView\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,200]\">{}{}</node></hierarchy>", row(a, 0), row(b, 100));
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = screen("Alpha", "Beta");
    let a = app(&adb, "rows");
    let alpha = ref_of(&a.snapshot(None, true, false).unwrap(), "Alpha");
    *adb.screen.borrow_mut() = screen("Gamma", "Delta");
    assert_eq!(code(&a.click(&alpha)), "STALE_REF");
    assert!(adb.shell_calls().is_empty());
}

#[test]
fn find_counts_a_row_and_the_text_that_names_it_once_and_get_reads_the_row_s_label() {
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = "<hierarchy rotation=\"0\"><node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,0][100,100]\"><node class=\"android.widget.TextView\" package=\"p\" text=\"Network\" resource-id=\"android:id/title\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,50]\" /></node></hierarchy>".into();
    let a = app(&adb, "dedupe");
    assert_eq!(a.find(None, None, Some("Network"), None, false, true).unwrap()["count"], 1);
    assert_eq!(a.find(None, None, Some("Network"), None, true, false).unwrap()["match"]["role"], "text");
    let row = ref_of(&a.snapshot(None, true, false).unwrap(), "Network");
    assert_eq!(a.run(&Cmd::Get { r#ref: row, property: "text".into() }).unwrap()["value"], "Network");
}

#[test]
fn health_counts_crashes_of_this_package_only() {
    assert!(names_package("E AndroidRuntime: Process: com.example.app, PID: 77", "com.example.app"));
    assert!(!names_package("E AndroidRuntime: Process: com.example.app2, PID: 9", "com.example.app"));
    assert!(!names_package("E AndroidRuntime: Process: xcom.example.app, PID: 9", "com.example.app"));
    assert!(!names_package("Process: com.example.app.debug", "com.example.app"));
    assert!(names_package("com.example.app", "com.example.app"));
}

#[test]
fn install_needs_a_clean_exit_and_the_word_success_on_its_own_line() {
    let apk = std::env::temp_dir().join("mola-cua-tests").join(format!("{}-SuccessStory.apk", std::process::id()));
    std::fs::create_dir_all(apk.parent().unwrap()).unwrap();
    std::fs::write(&apk, b"x").unwrap();
    let mut adb = FakeAdb::new();
    adb.exits = vec![("install", 1, "Performing Streamed Install\nadb: failed to install SuccessStory.apk: Failure [INSTALL_FAILED_INVALID_APK]\n")];
    let r = app(&adb, "inst").run(&Cmd::Install { apk: apk.clone() });
    assert_eq!(code(&r), "ACTION_FAILED");
    assert!(r.as_ref().err().unwrap().message.contains("INSTALL_FAILED_INVALID_APK"));
    adb.exits = vec![("install", 0, "Performing Streamed Install\nSuccess\n")];
    assert_eq!(app(&adb, "inst2").run(&Cmd::Install { apk }).unwrap()["installed"], true);
}

#[test]
fn text_equal_to_the_placeholder_is_reported_as_unverified_not_as_a_failure() {
    let hinted = ui::SAMPLE.replace("text=\"ada\" resource-id=\"com.example.app:id/user\"", "text=\"Search\" hint=\"Search\" resource-id=\"com.example.app:id/user\"");
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = hinted;
    let a = app(&adb, "hint");
    let user = a.find(None, None, None, Some("user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    let r = a.set_value(&user, "Search").unwrap();
    assert_eq!(r["verified"], false);
    assert_eq!(code(&a.set_value(&user, "other")), "ACTION_FAILED");
}

#[test]
fn wait_keeps_polling_while_the_device_gives_no_tree_and_says_so_when_time_runs_out() {
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = "ERROR: could not get idle state.\n".into();
    let r = app(&adb, "waitidle").wait(Some("Sign in"), None, false, 0);
    assert_eq!(code(&r), "TIMEOUT");
    assert!(r.as_ref().err().unwrap().details.as_ref().unwrap()["last_error"].as_str().unwrap().contains("idle state"));
}

#[test]
fn launch_does_not_take_a_shell_error_for_a_component_and_says_when_the_app_was_already_running() {
    let mut adb = FakeAdb::new();
    adb.shell = vec![("cmd package resolve-activity", "/system/bin/sh: cmd: not found\n")];
    assert_eq!(code(&app(&adb, "l1").launch("com.example.app")), "APP_NOT_FOUND");
    adb.shell = vec![("am start", "Starting: Intent\nWarning: Activity not started, its current task has been brought to the front\nStatus: ok\n")];
    assert_eq!(app(&adb, "l2").launch("com.example.app/.Main").unwrap()["already_running"], true);
}

#[test]
fn agent_desktop_is_resolved_to_an_executable_never_to_a_launcher_script() {
    let dir = std::env::temp_dir().join("mola-cua-tests").join(format!("{}-ad", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("agent-desktop.cmd");
    std::fs::write(&script, b"@echo off").unwrap();
    let s = script.to_string_lossy().to_string();
    let e = agent_desktop(&|k| (k == "MOLA_CUA_AGENT_DESKTOP").then(|| s.clone())).unwrap_err();
    assert!(e.contains("not a .cmd launcher"), "{e}");
    let path = dir.to_string_lossy().to_string();
    let on_path = |k: &str| (k == "PATH").then(|| path.clone());
    if cfg!(windows) {
        // a launcher with no native binary beside it is not started through a shell
        assert!(agent_desktop(&on_path).is_err());
        let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
        let native = dir.join("node_modules").join("agent-desktop").join("bin").join(format!("agent-desktop-win32-{arch}.exe"));
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&native, b"").unwrap();
        assert_eq!(agent_desktop(&on_path).unwrap(), native);
    } else {
        assert!(agent_desktop(&on_path).is_err());
        std::fs::write(dir.join("agent-desktop"), b"").unwrap();
        assert_eq!(agent_desktop(&on_path).unwrap(), dir.join("agent-desktop"));
    }
}

// ---------------------------------------------------------------- review round 2

#[test]
fn launch_install_and_close_that_do_not_come_back_are_uncertain_not_safe_to_retry() {
    let apk = std::env::temp_dir().join("mola-cua-tests").join(format!("{}-big.apk", std::process::id()));
    std::fs::create_dir_all(apk.parent().unwrap()).unwrap();
    std::fs::write(&apk, b"x").unwrap();
    for (hang, cmd) in [
        ("shell am start", Cmd::Launch { app: "com.example.app/.Main".into() }),
        ("install", Cmd::Install { apk: apk.clone() }),
        ("shell am force-stop", Cmd::CloseApp { package: "com.example.app".into() }),
    ] {
        let mut adb = FakeAdb::new();
        adb.hangs_on = Some(hang);
        let r = app(&adb, "hang-dev").run(&cmd);
        assert_eq!(code(&r), "TIMEOUT", "{hang}");
        assert_eq!(disposition(&r).0, "delivery_uncertain", "{hang}");
        assert!(envelope("x", &r)["error"].get("recovery").is_none(), "{hang}");
    }
}

#[test]
fn find_by_role_alone_returns_the_row_itself_even_when_a_wrapper_inside_it_has_the_same_role() {
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = "<hierarchy rotation=\"0\"><node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,0][100,100]\"><node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,100]\"><node class=\"android.widget.TextView\" package=\"p\" text=\"Alpha\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,50]\" /></node></node></hierarchy>".into();
    let a = app(&adb, "role");
    let found = a.find(Some("group"), None, None, None, false, false).unwrap();
    let rows: Vec<&Value> = found["matches"].as_array().unwrap().iter().filter(|m| m["name"] == "Alpha").collect();
    assert_eq!(rows.len(), 1, "{found}");
    assert_eq!(rows[0]["available_actions"], json!(["Click"]));
    // by words, the row and the text that names it are still one hit
    assert_eq!(a.find(None, None, Some("Alpha"), None, false, true).unwrap()["count"], 1);
}

#[test]
fn a_row_keeps_its_ref_when_only_its_second_line_changes() {
    let screen = |summary: &str| format!("<hierarchy rotation=\"0\"><node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,0][100,100]\"><node class=\"android.widget.TextView\" package=\"p\" text=\"Wi-Fi\" resource-id=\"android:id/title\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,50]\" /><node class=\"android.widget.TextView\" package=\"p\" text=\"{summary}\" resource-id=\"android:id/summary\" content-desc=\"\" enabled=\"true\" bounds=\"[0,50][100,100]\" /></node></hierarchy>");
    let adb = FakeAdb::new();
    *adb.screen.borrow_mut() = screen("Connected");
    let a = app(&adb, "summary");
    let row = ref_of(&a.snapshot(None, true, false).unwrap(), "Wi-Fi · Connected");
    *adb.screen.borrow_mut() = screen("Disconnected");
    assert_eq!(a.click(&row).unwrap()["tapped"], json!({"x": 50, "y": 50}));
}

#[test]
fn a_failed_typing_step_does_not_repeat_the_device_message() {
    let mut adb = FakeAdb::new();
    adb.exits = vec![("shell input text", 1, "")];
    let a = app(&adb, "echo");
    let user = a.find(None, None, None, Some("user"), true, false).unwrap()["match"]["ref_id"].as_str().unwrap().to_string();
    let r = a.set_value(&user, "s3cret");
    assert_eq!(code(&r), "ACTION_FAILED");
    assert_eq!(r.as_ref().err().unwrap().message, "`input text` failed with exit 1");
}
