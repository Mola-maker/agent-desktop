//! The Android UI tree: parse a `uiautomator dump`, turn it into agent-desktop's node shape, and find nodes again.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiNode {
    pub class: String,
    pub package: String,
    pub text: String,
    pub resource_id: String,
    pub content_desc: String,
    /// left, top, right, bottom in device pixels
    pub bounds: (i32, i32, i32, i32),
    pub clickable: bool,
    pub long_clickable: bool,
    pub checkable: bool,
    pub checked: bool,
    pub enabled: bool,
    pub focused: bool,
    pub scrollable: bool,
    pub selected: bool,
    pub password: bool,
    /// child indexes from the root; stable while the screen's structure is
    pub path: Vec<u32>,
    pub children: Vec<UiNode>,
}

/// What a ref remembers about its element. A ref is resolved against a fresh dump, never trusted as coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Identity {
    pub class: String,
    pub resource_id: String,
    pub content_desc: String,
    pub text: String,
    pub path: Vec<u32>,
}

/// `uiautomator dump /dev/tty` prints the XML and then "UI hierchary dumped to: /dev/tty" on the same stream.
pub fn parse_dump(raw: &str) -> Result<UiNode, String> {
    let start = raw.find("<hierarchy").ok_or_else(|| {
        let first = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        format!("no <hierarchy> in the dump: {}", first.chars().take(160).collect::<String>())
    })?;
    let end = raw.rfind("</hierarchy>").map(|i| i + "</hierarchy>".len()).ok_or("the dump is cut off before </hierarchy>")?;
    let doc = roxmltree::Document::parse(&raw[start..end]).map_err(|e| format!("the dump is not valid XML: {e}"))?;
    let mut root = UiNode { class: "hierarchy".into(), enabled: true, ..UiNode::default() };
    for (i, child) in doc.root_element().children().filter(|n| n.is_element()).enumerate() {
        root.children.push(read_node(child, vec![i as u32]));
    }
    root.bounds = root.children.iter().fold((0, 0, 0, 0), |a, c| (a.0.min(c.bounds.0), a.1.min(c.bounds.1), a.2.max(c.bounds.2), a.3.max(c.bounds.3)));
    root.package = root.children.first().map(|c| c.package.clone()).unwrap_or_default();
    Ok(root)
}

fn read_node(n: roxmltree::Node, path: Vec<u32>) -> UiNode {
    let s = |k: &str| n.attribute(k).unwrap_or("").to_string();
    let b = |k: &str| n.attribute(k) == Some("true");
    let mut node = UiNode {
        class: s("class"),
        package: s("package"),
        text: s("text"),
        resource_id: s("resource-id"),
        content_desc: s("content-desc"),
        bounds: parse_bounds(n.attribute("bounds").unwrap_or("")),
        clickable: b("clickable"),
        long_clickable: b("long-clickable"),
        checkable: b("checkable"),
        checked: b("checked"),
        enabled: b("enabled"),
        focused: b("focused"),
        scrollable: b("scrollable"),
        selected: b("selected"),
        password: b("password"),
        path: path.clone(),
        children: Vec::new(),
    };
    for (i, child) in n.children().filter(|c| c.is_element()).enumerate() {
        let mut p = path.clone();
        p.push(i as u32);
        node.children.push(read_node(child, p));
    }
    node
}

/// `[x1,y1][x2,y2]` → (x1, y1, x2, y2); anything else is an empty rectangle.
pub fn parse_bounds(s: &str) -> (i32, i32, i32, i32) {
    let nums: Vec<i32> = s.split(|c: char| !(c.is_ascii_digit() || c == '-')).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    if nums.len() == 4 { (nums[0], nums[1], nums[2], nums[3]) } else { (0, 0, 0, 0) }
}

impl UiNode {
    pub fn role(&self) -> &'static str {
        let c = self.class.rsplit('.').next().unwrap_or("");
        match c {
            "hierarchy" => "window",
            "EditText" | "AutoCompleteTextView" | "MultiAutoCompleteTextView" => "textfield",
            "Button" | "ImageButton" | "FloatingActionButton" | "MaterialButton" | "AppCompatButton" => "button",
            "CheckBox" | "AppCompatCheckBox" => "checkbox",
            "Switch" | "SwitchCompat" | "ToggleButton" | "SwitchMaterial" => "switch",
            "RadioButton" | "AppCompatRadioButton" => "radiobutton",
            "TextView" | "AppCompatTextView" | "MaterialTextView" => "text",
            "ImageView" | "AppCompatImageView" => "image",
            "SeekBar" => "slider",
            "ProgressBar" => "progressindicator",
            "Spinner" => "combobox",
            "WebView" => "webarea",
            "RecyclerView" | "ListView" | "GridView" => "list",
            "ScrollView" | "HorizontalScrollView" | "NestedScrollView" | "ViewPager" | "ViewPager2" => "scrollarea",
            "TabWidget" | "TabLayout" => "tablist",
            _ if c.ends_with("Button") => "button",
            _ if c.ends_with("EditText") => "textfield",
            _ => "group",
        }
    }

    pub fn is_textfield(&self) -> bool {
        self.role() == "textfield"
    }

    pub fn interactive(&self) -> bool {
        self.clickable || self.long_clickable || self.checkable || self.scrollable || self.is_textfield()
    }

    /// What a person reads on the element. A password field never yields its content.
    pub fn name(&self) -> Option<String> {
        if self.is_textfield() {
            return (!self.content_desc.is_empty()).then(|| self.content_desc.clone());
        }
        if !self.text.is_empty() && !self.password {
            Some(self.text.clone())
        } else if !self.content_desc.is_empty() {
            Some(self.content_desc.clone())
        } else {
            None
        }
    }

    pub fn value(&self) -> Option<String> {
        if !self.is_textfield() {
            return None;
        }
        Some(if self.password { "•".repeat(self.text.chars().count()) } else { self.text.clone() })
    }

    pub fn visible(&self) -> bool {
        self.bounds.2 > self.bounds.0 && self.bounds.3 > self.bounds.1
    }

    pub fn center(&self) -> (i32, i32) {
        ((self.bounds.0 + self.bounds.2) / 2, (self.bounds.1 + self.bounds.3) / 2)
    }

    pub fn identity(&self) -> Identity {
        Identity {
            class: self.class.clone(),
            resource_id: self.resource_id.clone(),
            content_desc: self.content_desc.clone(),
            // A text field's content changes as it is used; it is not part of what the element is.
            text: if self.is_textfield() || self.password { String::new() } else { self.text.clone() },
            path: self.path.clone(),
        }
    }

    pub fn states(&self) -> Vec<&'static str> {
        let mut s = Vec::new();
        if !self.enabled { s.push("disabled"); }
        if self.focused { s.push("focused"); }
        if self.checked { s.push("checked"); }
        if self.selected { s.push("selected"); }
        if self.password { s.push("password"); }
        s
    }

    pub fn actions(&self) -> Vec<&'static str> {
        let mut a = Vec::new();
        if self.clickable { a.push("Click"); }
        if self.is_textfield() { a.push("SetValue"); }
        if self.checkable { a.push("Toggle"); }
        if self.scrollable { a.push("Scroll"); }
        if self.long_clickable { a.push("LongPress"); }
        a
    }

    /// Every node, depth first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a UiNode>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }

    /// One node as agent-desktop prints it (no children).
    pub fn to_json(&self, ref_id: Option<&str>) -> Map<String, Value> {
        let mut m = Map::new();
        if let Some(r) = ref_id { m.insert("ref_id".into(), json!(r)); }
        m.insert("role".into(), json!(self.role()));
        if let Some(n) = self.name() { m.insert("name".into(), json!(n)); }
        if let Some(v) = self.value() { m.insert("value".into(), json!(v)); }
        if !self.resource_id.is_empty() { m.insert("resource_id".into(), json!(self.resource_id)); }
        let st = self.states();
        if !st.is_empty() { m.insert("states".into(), json!(st)); }
        let ac = self.actions();
        if !ac.is_empty() { m.insert("available_actions".into(), json!(ac)); }
        m.insert("bounds".into(), json!({"x": self.bounds.0, "y": self.bounds.1, "width": self.bounds.2 - self.bounds.0, "height": self.bounds.3 - self.bounds.1}));
        m
    }
}

/// A ref is given to what an agent can act on or read: interactive elements and elements with a name.
pub fn wants_ref(n: &UiNode) -> bool {
    n.visible() && (n.interactive() || n.name().is_some())
}

/// Find the element an identity describes in a fresh tree. `Ok(None)` = gone (stale); `Err(n)` = n equal candidates.
pub fn resolve<'a>(root: &'a UiNode, id: &Identity) -> Result<Option<&'a UiNode>, usize> {
    let mut all = Vec::new();
    root.walk(&mut all);
    let same = |n: &&UiNode| {
        let i = n.identity();
        i.class == id.class && i.resource_id == id.resource_id && i.content_desc == id.content_desc && i.text == id.text
    };
    let hits: Vec<&UiNode> = all.into_iter().filter(same).collect();
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits[0])),
        n => match hits.iter().find(|h| h.path == id.path) {
            // Several twins (list rows): the one in the same place is the one the ref meant.
            Some(h) => Ok(Some(h)),
            None => Err(n),
        },
    }
}

#[cfg(test)]
pub const SAMPLE: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.example.app" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,0][1080,2400]"><node index="0" text="Sign in" resource-id="com.example.app:id/title" class="android.widget.TextView" package="com.example.app" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[40,200][1040,300]" /><node index="1" text="ada" resource-id="com.example.app:id/user" class="android.widget.EditText" package="com.example.app" content-desc="User name" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="true" scrollable="false" long-clickable="true" password="false" selected="false" bounds="[40,340][1040,460]" /><node index="2" text="hunter2" resource-id="com.example.app:id/pass" class="android.widget.EditText" package="com.example.app" content-desc="Password" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="true" password="true" selected="false" bounds="[40,500][1040,620]" /><node index="3" text="Log in" resource-id="com.example.app:id/login" class="android.widget.Button" package="com.example.app" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[40,700][1040,820]" /><node index="4" text="" resource-id="com.example.app:id/list" class="androidx.recyclerview.widget.RecyclerView" package="com.example.app" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="true" focused="false" scrollable="true" long-clickable="false" password="false" selected="false" bounds="[0,900][1080,2400]"><node index="0" text="Row" resource-id="com.example.app:id/row" class="android.widget.TextView" package="com.example.app" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,900][1080,1000]" /><node index="1" text="Row" resource-id="com.example.app:id/row" class="android.widget.TextView" package="com.example.app" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[0,1000][1080,1100]" /></node></node></hierarchy>
UI hierchary dumped to: /dev/tty
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn all(root: &UiNode) -> Vec<&UiNode> {
        let mut v = Vec::new();
        root.walk(&mut v);
        v
    }

    #[test]
    fn the_dump_parses_and_the_trailing_notice_is_ignored() {
        let root = parse_dump(SAMPLE).unwrap();
        assert_eq!(root.package, "com.example.app");
        assert_eq!(all(&root).len(), 9);
        let login = all(&root).into_iter().find(|n| n.text == "Log in").unwrap();
        assert_eq!((login.role(), login.center(), login.actions()), ("button", (540, 760), vec!["Click"]));
        assert_eq!(login.path, vec![0, 3]);
    }

    #[test]
    fn a_dump_without_a_hierarchy_reports_the_device_message() {
        let e = parse_dump("ERROR: could not get idle state.\n").unwrap_err();
        assert!(e.contains("could not get idle state"), "{e}");
        assert!(parse_dump("<hierarchy rotation=\"0\"><node").is_err());
    }

    #[test]
    fn a_password_field_never_shows_its_content() {
        let root = parse_dump(SAMPLE).unwrap();
        let pass = all(&root).into_iter().find(|n| n.password).unwrap();
        let j = Value::Object(pass.to_json(None)).to_string();
        assert!(!j.contains("hunter2"), "{j}");
        assert_eq!(pass.value().unwrap(), "•••••••");
        assert_eq!(pass.name().unwrap(), "Password");
        assert_eq!(pass.identity().text, "");
    }

    #[test]
    fn a_ref_finds_its_element_again_and_a_text_field_keeps_its_ref_when_its_content_changes() {
        let root = parse_dump(SAMPLE).unwrap();
        let user = all(&root).into_iter().find(|n| n.resource_id.ends_with("/user")).unwrap().identity();
        let later = parse_dump(&SAMPLE.replace("text=\"ada\"", "text=\"grace\"")).unwrap();
        assert_eq!(resolve(&later, &user).unwrap().unwrap().text, "grace");
    }

    #[test]
    fn twins_resolve_by_position_and_a_missing_element_is_stale() {
        let root = parse_dump(SAMPLE).unwrap();
        let rows: Vec<&UiNode> = all(&root).into_iter().filter(|n| n.text == "Row").collect();
        assert_eq!(resolve(&root, &rows[1].identity()).unwrap().unwrap().bounds.1, 1000);
        let mut moved = rows[1].identity();
        moved.path = vec![9, 9];
        assert_eq!(resolve(&root, &moved), Err(2));
        let gone = parse_dump(&SAMPLE.replace("text=\"Log in\"", "text=\"Loading\"")).unwrap();
        let login = all(&root).into_iter().find(|n| n.text == "Log in").unwrap().identity();
        assert_eq!(resolve(&gone, &login), Ok(None));
    }

    #[test]
    fn bounds_parse_or_are_empty() {
        assert_eq!(parse_bounds("[0,84][1080,2400]"), (0, 84, 1080, 2400));
        assert_eq!(parse_bounds("garbage"), (0, 0, 0, 0));
    }
}
