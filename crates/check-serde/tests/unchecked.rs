use std::collections::BTreeMap;

use check_serde::{Error, from_str_unchecked};
use serde::Deserialize;

#[derive(Deserialize, Debug, PartialEq)]
enum Kind {
    Gallery,
    Link(String),
    #[serde(rename_all = "camelCase")]
    Game {
        max_players: u8,
    },
}

#[derive(Deserialize, Debug, PartialEq)]
struct Portal {
    address: String,
    label: Option<String>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Place {
    title: String,
    kind: Kind,
    portals: Vec<Portal>,
    size: (i64, i64),
    scores: BTreeMap<String, i64>,
}

fn check_error(path: &str, text: &str, code: &str) {
    match from_str_unchecked::<Place>(path, text) {
        Err(e @ Error::Check { .. }) => {
            let error = e.to_string();
            assert!(error.starts_with(&format!("error[{code}]")), "{error}");
            assert!(error.contains(&format!("--> {path}:")), "{error}");
        }
        _ => panic!("expected a check error"),
    }
}

#[test]
fn reads_a_file_without_a_check_class() {
    let text = "title = \"Hub\"
kind = Kind.Game(maxPlayers = 8)
portals = [Portal(address = \"pfp://a.com\"), (address = \"b\", label = Some(\"B\"))]
size = (4, -2)
scores = [\"ana\" = 3]";
    let hub: Place = from_str_unchecked("hub.con", text).unwrap();
    assert_eq!(hub.kind, Kind::Game { max_players: 8 });
    assert_eq!(hub.portals[1].label.as_deref(), Some("B"));
    assert_eq!(hub.size, (4, -2));
    assert_eq!(hub.scores["ana"], 3);
}

#[test]
fn matches_variants_by_name_as_written() {
    let kind = |text: &str| {
        let text =
            format!("title = \"t\"\nkind = {text}\nportals = []\nsize = (0, 0)\nscores = [=]");
        from_str_unchecked::<Place>("p.con", &text).unwrap().kind
    };
    assert_eq!(kind("Gallery"), Kind::Gallery);
    assert_eq!(kind("Link(\"x\")"), Kind::Link("x".to_string()));
}

#[test]
fn a_missing_field_is_a_serde_error() {
    assert!(matches!(
        from_str_unchecked::<Place>("p.con", "title = \"t\""),
        Err(Error::Value(_))
    ));
}

#[test]
fn refuses_code_mixed_fields_and_files_that_arent_con() {
    check_error("p.con", "title = \"a\" + \"b\"", "code_in_pure_data");
    check_error("p.con", "kind = Game(8, maxPlayers = 8)", "mixed_fields");
    check_error("p.conx", "title = \"a\"", "not_pure_data");
    for text in [
        "title = foo",
        "kind = Kind.Link.Game",
        "kind = list.Link(\"x\")",
        "title = Set(\"x\")",
    ] {
        check_error("p.con", text, "code_in_pure_data");
    }
}

#[test]
fn empty_fields_leave_optional_fields_unset() {
    #[derive(Deserialize)]
    struct Options {
        name: Option<String>,
    }
    for text in ["", "()", "Options()"] {
        let options: Options = from_str_unchecked("o.con", text).unwrap();
        assert_eq!(options.name, None);
    }
}

#[derive(Deserialize, Debug, PartialEq)]
struct Versioned {
    version: String,
}

#[test]
fn reads_a_json_file_with_serde_json() {
    let v: Versioned =
        from_str_unchecked("place.json", "{\"version\": \"0.2.0\", \"scene\": 3}").unwrap();
    assert_eq!(v.version, "0.2.0");
    match from_str_unchecked::<Versioned>("place.json", "{\"version\": }") {
        Err(Error::Value(e)) => assert!(e.to_string().contains("line 1"), "{e}"),
        _ => panic!("expected a value error"),
    }
}
