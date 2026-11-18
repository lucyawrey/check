#![cfg(feature = "check")]

use std::collections::HashMap;

use check_core::modules::MemFiles;
use check_serde::{Error, from_str};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const SCHEMA: &str = "namespace place

enum Kind {
  Gallery
  Link(String)
  Game(maxPlayers: Int)
}

class Portal(address: String, label: String? = None)

ref class Counter(count: Int)

class Place {
  let title: String
  let kind: Kind = Kind.Gallery
  let portals: Portal[] = []
  let size: (Int, Int) = (1, 1)
  let tags: Set<String> = Set([])
  let scores: Map<String, Int> = [=]
  var greeting = \"\"
  init { this.greeting = \"Welcome to {this.title}\" }
}
";

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
    greeting: String,
}

fn read_with<T: DeserializeOwned>(
    schema: &[(&str, &str)],
    path: &str,
    text: &str,
    ty: &str,
) -> Result<T, Error> {
    let files = schema
        .iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect::<HashMap<_, _>>();
    from_str(&MemFiles(files), "place/", "place", path, text, ty)
}

fn read<T: DeserializeOwned>(path: &str, text: &str, ty: &str) -> Result<T, Error> {
    read_with(&[("place/place.check", SCHEMA)], path, text, ty)
}

fn place(text: &str) -> Place {
    read("place.con", text, "place.Place").unwrap()
}

/// The rendered check error, which must have `code`.
fn check_error(path: &str, text: &str, code: &str) -> String {
    match read::<Place>(path, text, "place.Place") {
        Err(e @ Error::Check { .. }) => {
            let error = e.to_string();
            assert!(error.starts_with(&format!("error[{code}]")), "{error}");
            error
        }
        _ => panic!("expected a check error"),
    }
}

fn value_error<T: DeserializeOwned>(text: &str, ty: &str) -> String {
    match read::<T>("place.con", text, ty) {
        Err(Error::Value(e)) => e.to_string(),
        Err(e) => panic!("expected a value error: {e}"),
        Ok(_) => panic!("expected a value error"),
    }
}

#[test]
fn reads_a_bare_record_file_with_defaults_and_init() {
    assert_eq!(
        place("title = \"Gallery\""),
        Place {
            title: "Gallery".to_string(),
            kind: Kind::Gallery,
            portals: vec![],
            size: (1, 1),
            greeting: "Welcome to Gallery".to_string(),
        }
    );
}

#[test]
fn reads_variants_options_lists_and_positional_records() {
    let text = "title = \"Hub\"
kind = Game(maxPlayers = 8)
portals = [(address = \"pfp://a.com\"), (address = \"pfp://b.com\", label = Some(\"B\"))]
size = (4, 2)";
    let hub = place(text);
    assert_eq!(hub.kind, Kind::Game { max_players: 8 });
    assert_eq!(
        hub.portals,
        vec![
            Portal {
                address: "pfp://a.com".to_string(),
                label: None
            },
            Portal {
                address: "pfp://b.com".to_string(),
                label: Some("B".to_string())
            },
        ]
    );
    assert_eq!(hub.size, (4, 2));
    assert_eq!(
        place("title = \"L\"\nkind = Link(\"pfp://c.com\")").kind,
        Kind::Link("pfp://c.com".to_string())
    );
}

#[test]
fn any_file_name_works() {
    let place: Place = read("my-place.con", "title = \"A\"", "place.Place").unwrap();
    assert_eq!(place.title, "A");
}

#[test]
fn reads_the_hosts_file_not_other_data_in_the_schema_unit() {
    let schema = [
        ("place/package.con", "namespace place"),
        ("place/defaults.con", "(title = \"Default\")"),
        ("place/place.check", SCHEMA),
    ];
    let place: Place = read_with(&schema, "place.con", "title = \"A\"", "place.Place").unwrap();
    assert_eq!(place.title, "A");
}

#[test]
fn reads_a_ref_class_like_a_class() {
    #[derive(Deserialize)]
    struct Counter {
        count: i64,
    }
    let counter: Counter = read("c.con", "count = 3", "place.Counter").unwrap();
    assert_eq!(counter.count, 3);
}

#[test]
fn a_wrong_type_is_a_check_error_in_the_file() {
    let error = check_error("place.con", "title = 3", "type_mismatch");
    assert!(error.contains("--> place.con:1"), "{error}");
}

#[test]
fn a_constructor_of_the_wrong_class_is_a_check_error() {
    check_error("place.con", "Portal(address = \"x\")", "type_mismatch");
}

#[test]
fn refuses_code_and_files_that_arent_con() {
    check_error("place.con", "title = \"a\" + \"b\"", "code_in_pure_data");
    let error = check_error("place.conx", "title = \"a\"", "not_pure_data");
    assert!(error.contains("--> place.conx:1"), "{error}");
}

#[test]
fn an_int_too_big_for_its_field_is_an_error() {
    let error = value_error::<Place>(
        "title = \"G\"\nkind = Game(maxPlayers = 300)",
        "place.Place",
    );
    assert!(error.starts_with("kind.maxPlayers: "), "{error}");
}

#[test]
fn a_mismatched_rust_type_names_the_nested_field() {
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Portal {
        address: i64,
    }
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Place {
        portals: Vec<Portal>,
    }
    let text = "title = \"G\"\nportals = [(address = \"a\"), (address = \"b\")]";
    let error = value_error::<Place>(text, "place.Place");
    assert!(error.starts_with("portals[0].address: "), "{error}");
}

#[test]
fn reads_a_file_inside_the_schema_folder_once() {
    let schema = [("place/place.check", SCHEMA)];
    let place: Place = read_with(
        &schema,
        "place/my-place.con",
        "title = \"A\"",
        "place.Place",
    )
    .unwrap();
    assert_eq!(place.title, "A");
}

#[test]
fn reads_maps_and_sets() {
    #[derive(Deserialize)]
    struct Place {
        tags: std::collections::BTreeSet<String>,
        scores: std::collections::BTreeMap<String, u8>,
    }
    let text = "title = \"G\"\ntags = Set([\"b\", \"a\"])\nscores = [\"ana\" = 3, \"bo\" = 300]";
    let error = value_error::<Place>(text, "place.Place");
    assert!(error.starts_with("scores[\"bo\"]: "), "{error}");
    let place: Place = read("place.con", &text.replace("300", "4"), "place.Place").unwrap();
    assert_eq!(place.tags.into_iter().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(place.scores["bo"], 4);
}
