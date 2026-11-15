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
    /// placeholder of a text field; uiautomator also reports it as `text` while the field is empty
    pub hint: String,
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
    /// placeholder of a text field: what tells one unnamed field from another
    #[serde(default)]
    pub hint: String,
    /// the label a nameless container borrows from the texts inside it: what tells one list row from another
    #[serde(default)]
    pub label: String,
    /// every text the label was built from: what tells apart two rows that share a first line (two "John"s)
    #[serde(default)]
    pub label_full: String,
    /// when the ref was made, another element on the screen had the same class, id, description and first line
    #[serde(default)]
    pub shared_title: bool,
    pub path: Vec<u32>,
}

impl Identity {
    /// Does anything besides the class and the position say which element this is?
    fn anchored(&self) -> bool {
        [&self.resource_id, &self.content_desc, &self.text, &self.hint, &self.label].into_iter().any(|s| !s.is_empty())
    }
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
        hint: s("hint"),
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

    /// What the user typed into a text field: empty while only the placeholder shows.
    pub fn content(&self) -> &str {
        if !self.hint.is_empty() && self.text == self.hint { "" } else { &self.text }
    }

    /// The element's own name, or for an unnamed thing you can act on (a clickable row), the first texts inside it.
    pub fn label(&self) -> Option<String> {
        if let Some(n) = self.name() {
            return Some(n);
        }
        if !self.interactive() || self.scrollable {
            return None;
        }
        let mut all = Vec::new();
        self.walk(&mut all);
        let texts: Vec<String> = all.iter().skip(1).filter(|c| !c.is_textfield()).filter_map(|c| c.name()).take(2).collect();
        (!texts.is_empty()).then(|| texts.join(" · "))
    }

    /// What a person reads on the element. A password field never yields its content.
    pub fn name(&self) -> Option<String> {
        if self.is_textfield() {
            return [&self.content_desc, &self.hint].into_iter().find(|s| !s.is_empty()).cloned();
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
        Some(if self.password { "•".repeat(self.content().chars().count()) } else { self.content().to_string() })
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
            hint: self.hint.clone(),
            // The first text only: a row's second line (a status, a count, a time) changes while the row stays the row.
            label: if self.name().is_none() { self.label().map(|l| l.split(" · ").next().unwrap_or("").to_string()).unwrap_or_default() } else { String::new() },
            label_full: if self.name().is_none() { self.label().unwrap_or_default() } else { String::new() },
            shared_title: false,
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
        if let Some(n) = self.label() { m.insert("name".into(), json!(n)); }
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

/// Note on each stored identity whether the screen it came from held another element with the same first line.
/// Counted over every node on the screen, not only the ones that got a ref.
pub fn mark_shared<'a>(root: &UiNode, entries: impl Iterator<Item = &'a mut Identity>) {
    let key = |i: &Identity| (i.class.clone(), i.resource_id.clone(), i.content_desc.clone(), i.text.clone(), i.hint.clone(), i.label.clone());
    let mut all = Vec::new();
    root.walk(&mut all);
    let mut count = std::collections::HashMap::new();
    for n in all {
        *count.entry(key(&n.identity())).or_insert(0u32) += 1;
    }
    for id in entries {
        id.shared_title = count.get(&key(id)).copied().unwrap_or(0) > 1;
    }
}

/// Find the element an identity describes in a fresh tree. `Ok(None)` = gone (stale); `Err(n)` = n equal candidates.
pub fn resolve<'a>(root: &'a UiNode, id: &Identity) -> Result<Option<&'a UiNode>, usize> {
    let mut all = Vec::new();
    root.walk(&mut all);
    let same = |n: &&UiNode| {
        let i = n.identity();
        i.class == id.class && i.resource_id == id.resource_id && i.content_desc == id.content_desc && i.text == id.text && i.hint == id.hint && i.label == id.label
    };
    let hits: Vec<&UiNode> = all.into_iter().filter(same).collect();
    match hits.len() {
        0 => Ok(None),
        // An element with nothing but a class is only itself in the same place: elsewhere it is some other element.
        1 if !id.anchored() && hits[0].path != id.path => Ok(None),
        // The ref had a twin when it was made. One candidate left may be that twin: it must match in every text.
        1 if id.shared_title && hits[0].identity().label_full != id.label_full => Ok(None),
        1 => Ok(Some(hits[0])),
        n => {
            // Several candidates share the first line. The second line may change on a row that is alone with its
            // title, but among twins it is what says which one the ref meant: only a twin with every text equal counts.
            let exact: Vec<&&UiNode> = hits.iter().filter(|h| h.identity().label_full == id.label_full).collect();
            match exact.len() {
                1 => Ok(Some(exact[0])),
                0 => Err(n),
                // identical in every text (plain list rows): the one in the same place
                _ => exact.iter().find(|h| h.path == id.path).map(|h| Some(**h)).ok_or(n),
            }
        }
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
    fn an_empty_field_reports_its_placeholder_as_name_not_as_content() {
        let xml = SAMPLE.replace("text=\"ada\" resource-id=\"com.example.app:id/user\"", "text=\"Search settings\" hint=\"Search settings\" resource-id=\"com.example.app:id/user\"").replace("content-desc=\"User name\"", "content-desc=\"\"");
        let root = parse_dump(&xml).unwrap();
        let f = all(&root).into_iter().find(|n| n.resource_id.ends_with("/user")).unwrap();
        assert_eq!((f.content(), f.value().unwrap().as_str(), f.name().unwrap().as_str()), ("", "", "Search settings"));
        let typed = parse_dump(&xml.replace("text=\"Search settings\" hint", "text=\"wifi\" hint")).unwrap();
        let f = all(&typed).into_iter().find(|n| n.resource_id.ends_with("/user")).unwrap();
        assert_eq!(f.value().unwrap(), "wifi");
    }

    #[test]
    fn a_clickable_row_without_a_name_is_labelled_by_the_texts_inside_it() {
        let xml = r#"<hierarchy rotation="0"><node class="android.widget.LinearLayout" package="p" text="" resource-id="" content-desc="" clickable="true" enabled="true" bounds="[0,0][100,100]"><node class="android.widget.TextView" package="p" text="Network &amp; internet" resource-id="android:id/title" content-desc="" enabled="true" bounds="[0,0][100,40]" /><node class="android.widget.TextView" package="p" text="Mobile, Wi-Fi, hotspot" resource-id="android:id/summary" content-desc="" enabled="true" bounds="[0,40][100,80]" /><node class="android.widget.TextView" package="p" text="third" resource-id="" content-desc="" enabled="true" bounds="[0,80][100,100]" /></node></hierarchy>"#;
        let root = parse_dump(xml).unwrap();
        let row = &root.children[0];
        assert_eq!(row.name(), None);
        assert_eq!(row.label().unwrap(), "Network & internet · Mobile, Wi-Fi, hotspot");
        assert_eq!(row.to_json(None)["name"], "Network & internet · Mobile, Wi-Fi, hotspot");
        assert_eq!(root.label(), None);
    }

    #[test]
    fn a_row_ref_is_stale_when_another_row_sits_in_its_place() {
        let row = |t: &str, y: i32| {
            format!(
                "<node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,{y}][100,{}]\"><node class=\"android.widget.TextView\" package=\"p\" text=\"{t}\" resource-id=\"android:id/title\" content-desc=\"\" enabled=\"true\" bounds=\"[0,{y}][100,{}]\" /></node>",
                y + 100,
                y + 50
            )
        };
        let screen = |a: &str, b: &str| {
            format!(
                "<hierarchy rotation=\"0\"><node class=\"android.widget.ListView\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,200]\">{}{}</node></hierarchy>",
                row(a, 0),
                row(b, 100)
            )
        };
        let before = parse_dump(&screen("Alpha", "Beta")).unwrap();
        let alpha = before.children[0].children[0].identity();
        assert_eq!(alpha.label, "Alpha");
        let scrolled = parse_dump(&screen("Gamma", "Delta")).unwrap();
        assert_eq!(resolve(&scrolled, &alpha), Ok(None), "the ref for Alpha resolved to the row now in its place");
        let moved = parse_dump(&screen("Zed", "Alpha")).unwrap();
        assert_eq!(resolve(&moved, &alpha).unwrap().unwrap().bounds.1, 100, "Alpha moved down and is still Alpha");
    }

    #[test]
    fn rows_that_share_a_title_are_told_apart_by_their_second_line() {
        let row = |title: &str, sub: &str, y: i32| {
            format!(
                "<node class=\"android.widget.LinearLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,{y}][100,{}]\"><node class=\"android.widget.TextView\" package=\"p\" text=\"{title}\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,{y}][100,{}]\" /><node class=\"android.widget.TextView\" package=\"p\" text=\"{sub}\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,{}][100,{}]\" /></node>",
                y + 100,
                y + 50,
                y + 50,
                y + 100
            )
        };
        let screen = |rows: &[(&str, &str)]| {
            let body: String = rows.iter().enumerate().map(|(i, (t, s))| row(t, s, i as i32 * 100)).collect();
            format!("<hierarchy rotation=\"0\"><node class=\"android.widget.ListView\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,300]\">{body}</node></hierarchy>")
        };
        let before = parse_dump(&screen(&[("John", "mobile"), ("John", "work")])).unwrap();
        let mut mobile = before.children[0].children[0].identity();
        mark_shared(&before, std::iter::once(&mut mobile));
        assert!(mobile.shared_title);
        // the list refreshed: only the other John is left, alone. It is not the John the ref meant.
        let other_alone = parse_dump(&screen(&[("John", "work"), ("Zed", "x")])).unwrap();
        assert_eq!(resolve(&other_alone, &mobile), Ok(None), "tapped the twin that was left");
        // the same John left alone is still found
        let same_alone = parse_dump(&screen(&[("Zed", "x"), ("John", "mobile")])).unwrap();
        assert_eq!(resolve(&same_alone, &mobile).unwrap().unwrap().bounds.1, 100);
        // scrolled: the first slot now holds the other John
        let scrolled = parse_dump(&screen(&[("John", "work"), ("John", "home")])).unwrap();
        assert_eq!(resolve(&scrolled, &mobile), Err(2), "tapped another John");
        // moved down one slot: still found, by its texts
        let moved = parse_dump(&screen(&[("Zed", "x"), ("John", "work"), ("John", "mobile")])).unwrap();
        assert_eq!(resolve(&moved, &mobile).unwrap().unwrap().bounds.1, 200);
        // a ref made when its title was unique: a changed second line does not matter
        let solo_screen = parse_dump(&screen(&[("John", "mobile"), ("Mary", "work")])).unwrap();
        let mut solo = solo_screen.children[0].children[0].identity();
        mark_shared(&solo_screen, std::iter::once(&mut solo));
        assert!(!solo.shared_title);
        let alone = parse_dump(&screen(&[("John", "busy"), ("Mary", "work")])).unwrap();
        assert_eq!(resolve(&alone, &solo).unwrap().unwrap().bounds.1, 0);
    }

    #[test]
    fn an_unnamed_field_is_not_some_other_unnamed_field() {
        let field = |hint: &str| {
            format!(
                "<hierarchy rotation=\"0\"><node class=\"android.widget.FrameLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,300]\"><node class=\"android.widget.EditText\" package=\"p\" text=\"{hint}\" hint=\"{hint}\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,200][100,300]\" /></node></hierarchy>"
            )
        };
        let login = parse_dump(&field("Username")).unwrap();
        let user = login.children[0].children[0].identity();
        assert_eq!(resolve(&parse_dump(&field("Search")).unwrap(), &user), Ok(None));
        // no hint, no id, no description: only the same place counts
        let bare = |extra: &str| {
            format!(
                "<hierarchy rotation=\"0\"><node class=\"android.widget.FrameLayout\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,300]\">{extra}<node class=\"android.widget.EditText\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" clickable=\"true\" enabled=\"true\" bounds=\"[0,200][100,300]\" /></node></hierarchy>"
            )
        };
        let a = parse_dump(&bare("")).unwrap();
        let id = a.children[0].children[0].identity();
        assert!(resolve(&a, &id).unwrap().is_some());
        let shifted = parse_dump(&bare("<node class=\"android.widget.ImageView\" package=\"p\" text=\"\" resource-id=\"\" content-desc=\"\" enabled=\"true\" bounds=\"[0,0][100,100]\" />")).unwrap();
        assert_eq!(resolve(&shifted, &id), Ok(None));
    }

    #[test]
    fn bounds_parse_or_are_empty() {
        assert_eq!(parse_bounds("[0,84][1080,2400]"), (0, 84, 1080, 2400));
        assert_eq!(parse_bounds("garbage"), (0, 0, 0, 0));
    }
}
