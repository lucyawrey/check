#![cfg(feature = "check")]

use std::collections::HashMap;
use std::fmt::Debug;

use check_core::modules::MemFiles;
use check_serde::{Error, from_str, from_str_unchecked};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const SCHEMA: &str = "namespace ui

enum Align {
  Start
  Center
}

interface Element {}

class Panel: Element {
  let title: String = \"\"
  let padding: Int = 0
  let align: Align = Align.Start
  let children: Element[] = []
}

class Text: Element {
  let text: String
  let size: Int = 16
}

class Button: Element {
  let label: String
}

class Page {
  let title: String
  let header: Element? = None
  let tabs: Element[] = []
  let footer: Element[]? = None
}

class Friend(name: String)

class Settings(volume: Float, showFps: Bool = false, friends: Friend[] = [])

func double(n: Int) -> Int { n * 2 }
";

#[derive(Deserialize, Debug, PartialEq, Default)]
enum Align {
    #[default]
    Start,
    Center,
}

#[derive(Deserialize, Debug, PartialEq)]
enum El {
    Panel {
        #[serde(default)]
        title: String,
        #[serde(default)]
        padding: i64,
        #[serde(default)]
        align: Align,
        #[serde(default)]
        children: Vec<El>,
    },
    Text {
        text: String,
        #[serde(default = "default_size")]
        size: i64,
    },
    Button {
        label: String,
    },
}

fn default_size() -> i64 {
    16
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(default)]
#[derive(Default)]
struct Page {
    title: String,
    header: Option<El>,
    tabs: Vec<El>,
    footer: Option<Vec<El>>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Friend {
    name: String,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Settings {
    volume: f64,
    #[serde(default)]
    show_fps: bool,
    #[serde(default)]
    friends: Vec<Friend>,
}

fn read<T: DeserializeOwned>(path: &str, text: &str, ty: &str) -> Result<T, Error> {
    let files = HashMap::from([("ui/ui.check".to_string(), SCHEMA.to_string())]);
    from_str(&MemFiles(files), "ui/", "page", path, text, ty)
}

/// Checks that `cml` reads as its `.con` twin does, checked and unchecked, and returns it.
fn same<T: DeserializeOwned + PartialEq + Debug>(cml: &str, con: &str, ty: &str) -> T {
    let checked: T = read("page.cml", cml, ty).unwrap();
    assert_eq!(checked, read::<T>("page.con", con, ty).unwrap());
    assert_eq!(checked, from_str_unchecked::<T>("page.cml", cml).unwrap());
    checked
}

fn check_error(path: &str, text: &str, code: &str) -> String {
    match read::<Page>(path, text, "ui.Page") {
        Err(e @ Error::Check { .. }) => {
            let error = e.to_string();
            assert!(error.contains(&format!("error[{code}]")), "{error}");
            error
        }
        other => panic!("expected a check error, got {other:?}"),
    }
}

fn text(s: &str) -> El {
    El::Text {
        text: s.to_string(),
        size: 16,
    }
}

#[test]
fn an_element_reads_as_its_constructor_call() {
    let panel: El = same(
        "<Panel title=\"Main\" padding=16 align=Align.Center>
          <Text size=32>My Game</Text>
          <Button label=\"Play\"/>
        </Panel>",
        "Panel(title = \"Main\", padding = 16, align = Align.Center, children = [
          Text(size = 32, text = \"My Game\")
          Button(label = \"Play\")
        ])",
        "ui.Panel",
    );
    let El::Panel {
        align, children, ..
    } = panel
    else {
        panic!("{panel:?}")
    };
    assert_eq!(align, Align::Center);
    assert_eq!(
        children[1],
        El::Button {
            label: "Play".into()
        }
    );
}

#[test]
fn a_qualified_tag_names_its_class() {
    let button: El = same(
        "<ui.Button label=\"Play\"/>",
        "ui.Button(label = \"Play\")",
        "ui.Button",
    );
    assert_eq!(
        button,
        El::Button {
            label: "Play".into()
        }
    );
}

#[test]
fn a_nameless_tag_is_a_record() {
    let settings: Settings = same(
        "<volume=0.8 showFps=true>
          <friends>
            <name=\"Stef\"/>
          </friends>
        </>",
        "volume = 0.8\nshowFps = true\nfriends = [(name = \"Stef\")]",
        "ui.Settings",
    );
    assert_eq!(
        settings.friends,
        [Friend {
            name: "Stef".into()
        }]
    );
    same::<Settings>("<volume=0.5/>", "volume = 0.5", "ui.Settings");
}

#[test]
fn several_top_level_elements_make_a_list() {
    let buttons: Vec<El> =
        from_str_unchecked("page.cml", "<Button label=\"a\"/>\n<Button label=\"b\"/>").unwrap();
    assert_eq!(buttons.len(), 2);
}

#[test]
fn text_collapses_whitespace_and_takes_backslash_escapes() {
    let el: El = same(
        "<Text>\n   a  \\< b\n\n  c \\{ \\\\  </Text>",
        "Text(text = \"a < b c { \\\\\")",
        "ui.Text",
    );
    assert_eq!(el, text("a < b c { \\"));
    check_error("page.cml", "<Text>\\q</Text>", "unknown_escape");
}

#[test]
fn a_comment_is_a_line_of_its_own_and_a_url_is_text() {
    let el: El = same(
        "// the title\n<Text>\n  // not shown\n  see https://example.com\n</Text>",
        "Text(text = \"see https://example.com\")",
        "ui.Text",
    );
    assert_eq!(el, text("see https://example.com"));
}

#[test]
fn an_element_cant_hold_text_and_child_elements() {
    check_error(
        "page.cml",
        "<Page title=\"P\"><header><Panel>Hi <Text>x</Text></Panel></header></Page>",
        "mixed_text_and_children",
    );
}

#[test]
fn a_field_tag_follows_its_fields_type() {
    let page: Page = same(
        "<Page>
          <title>Home</title>
          <header><Text>Hi</Text></header>
          <tabs><Button label=\"a\"/></tabs>
          <footer><Button label=\"b\"/></footer>
        </Page>",
        "Page(title = \"Home\", header = Text(text = \"Hi\"), tabs = [Button(label = \"a\")], footer = [Button(label = \"b\")])",
        "ui.Page",
    );
    assert_eq!(page.title, "Home");
    assert_eq!(page.header, Some(text("Hi")));
    assert_eq!(page.footer, Some(vec![El::Button { label: "b".into() }]));
}

#[test]
fn a_field_tag_for_one_value_needs_exactly_one_element() {
    for header in ["", "<Text>a</Text><Text>b</Text>"] {
        let page = format!("<Page title=\"P\"><header>{header}</header></Page>");
        check_error("page.cml", &page, "field_tag_needs_one");
    }
}

#[test]
fn every_tag_needs_a_matching_close() {
    check_error(
        "page.cml",
        "<Page title=\"P\"></Panel>",
        "mismatched_closing_tag",
    );
    check_error("page.cml", "<Page title=\"P\">", "unclosed_tag");
    check_error("page.cml", "<Page title=\"P\"", "unclosed_tag");
}

#[test]
fn braces_are_text_in_cml_and_code_in_cmlx() {
    let el: El = same(
        "<Text>{1 + 1}</Text>",
        "Text(text = \"{1 + 1}\")",
        "ui.Text",
    );
    assert_eq!(el, text("{1 + 1}"));
    check_error("page.cml", "<Page title={\"P\"}/>", "code_in_pure_data");
    let el: El = read(
        "page.cmlx",
        "<Text size={double(16)}>{1 + 1} apples</Text>",
        "ui.Text",
    )
    .unwrap();
    assert_eq!(
        el,
        El::Text {
            text: "2 apples".into(),
            size: 32
        }
    );
}

#[test]
fn a_host_loads_a_conx_file_using_the_schema_units_names() {
    let el: El = read(
        "page.conx",
        "Text(text = \"x\", size = double(8))",
        "ui.Text",
    )
    .unwrap();
    assert_eq!(
        el,
        El::Text {
            text: "x".into(),
            size: 16
        }
    );
}

#[test]
fn a_panic_or_runaway_loop_in_a_loaded_x_file_is_an_error() {
    for (value, code) in [
        ("[1][5].orPanic()", "or_panic"),
        ("{ loop { } }", "out_of_steps"),
    ] {
        let file = format!("Text(text = \"x\", size = {value})");
        match read::<El>("page.conx", &file, "ui.Text") {
            Err(Error::Check {
                panic_text: Some(panic),
                ..
            }) => assert!(panic.contains(code), "{panic}"),
            other => panic!("expected a panic, got {other:?}"),
        }
    }
}

#[test]
fn unchecked_reading_refuses_files_that_can_run_code() {
    for path in ["page.conx", "page.cmlx"] {
        let error = from_str_unchecked::<El>(path, "<Button label=\"a\"/>").unwrap_err();
        assert!(error.to_string().contains("not_pure_data"), "{error}");
    }
}
