#![cfg(feature = "check")]

use std::collections::HashMap;

use check_core::modules::MemFiles;
use check_serde::{Error, from_str};
use serde::Deserialize;

const SCHEMA: &str = "namespace place

enum Sky {
  Atmosphere
  Gradient
  None
}

enum Shape {
  Square
  Circle(radius: Float)
}

class Spawn(center: (Float, Float, Float), yaw: Float = 0.0)

class Place {
  let name: String
  let label: String? = None
  let spawn: Spawn
  let sky: Sky = Sky.Atmosphere
  let shape: Shape = Shape.Square
  let ids: Int[] = []
  let count: Int = 0
  var greeting = \"\"
  init { this.greeting = \"Hi {this.name}\" }
}
";

#[derive(Deserialize, Debug, PartialEq)]
enum Sky {
    Atmosphere,
    Gradient,
    None,
}

#[derive(Deserialize, Debug, PartialEq)]
enum Shape {
    Square,
    Circle { radius: f64 },
}

#[derive(Deserialize, Debug, PartialEq)]
struct Spawn {
    center: (f64, f64, f64),
    yaw: f64,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Place {
    name: String,
    label: Option<String>,
    spawn: Spawn,
    sky: Sky,
    shape: Shape,
    ids: Vec<i64>,
    count: i64,
    greeting: String,
}

fn read(path: &str, text: &str) -> Result<Place, Error> {
    let files = HashMap::from([("place/place.check".to_string(), SCHEMA.to_string())]);
    from_str(
        &MemFiles(files),
        "place/",
        "place",
        path,
        text,
        "place.Place",
    )
}

fn place(fields: &str) -> Place {
    let text = format!("{{\"name\": \"Meadow\", \"spawn\": {{\"center\": [0, 1, 0]}}{fields}}}");
    read("place.json", &text).unwrap()
}

fn check_error(text: &str, code: &str) -> String {
    match read("place.json", text) {
        Err(e @ Error::Check { .. }) => {
            let error = e.to_string();
            assert!(error.starts_with(&format!("error[{code}]")), "{error}");
            error
        }
        Err(e) => panic!("expected a check error: {e}"),
        Ok(_) => panic!("expected a check error"),
    }
}

fn field_error(fields: &str, code: &str) -> String {
    check_error(
        &format!("{{\"name\": \"M\", \"spawn\": {{\"center\": [0, 1, 0]}}{fields}}}"),
        code,
    )
}

#[test]
fn reads_an_object_as_the_class_with_defaults_and_init() {
    assert_eq!(
        place(""),
        Place {
            name: "Meadow".to_string(),
            label: None,
            spawn: Spawn {
                center: (0.0, 1.0, 0.0),
                yaw: 0.0
            },
            sky: Sky::Atmosphere,
            shape: Shape::Square,
            ids: vec![],
            count: 0,
            greeting: "Hi Meadow".to_string(),
        }
    );
}

#[test]
fn reads_strings_as_variants_tagged_objects_as_data_variants_and_null_as_none() {
    let p = place(
        ", \"sky\": \"None\", \"shape\": {\"type\": \"Circle\", \"radius\": 2}, \"ids\": [1, 2], \"label\": null",
    );
    assert_eq!(p.sky, Sky::None);
    assert_eq!(p.shape, Shape::Circle { radius: 2.0 });
    assert_eq!(p.ids, vec![1, 2]);
    assert_eq!(p.label, None);
    assert_eq!(
        place(", \"shape\": {\"type\": \"Square\"}").shape,
        Shape::Square
    );
    assert_eq!(place(", \"label\": \"L\"").label, Some("L".to_string()));
}

#[test]
fn a_type_error_names_the_values_path() {
    let error = field_error(
        ", \"spawn\": {\"center\": [0, 1, 0], \"yaw\": \"left\"}",
        "type_mismatch",
    );
    assert!(error.contains("--> place.json: spawn.yaw\n"), "{error}");
    let error = field_error(", \"spawn\": {\"center\": [0, 1]}", "type_mismatch");
    assert!(error.contains("--> place.json: spawn.center\n"), "{error}");
}

#[test]
fn unknown_names_and_variants_are_errors() {
    let error = field_error(", \"colour\": 1", "wrong_arguments");
    assert!(error.contains("--> place.json: colour\n"), "{error}");
    let error = field_error(
        ", \"spawn\": {\"center\": [0, 1, 0], \"bad\": 1}",
        "wrong_arguments",
    );
    assert!(error.contains("--> place.json: spawn.bad\n"), "{error}");
    let error = field_error(", \"sky\": \"Night\"", "unknown_variant");
    assert!(error.contains("--> place.json: sky\n"), "{error}");
    let error = field_error(", \"shape\": {\"type\": \"Hexagon\"}", "unknown_variant");
    assert!(error.contains("--> place.json: shape.type\n"), "{error}");
    field_error(", \"shape\": {\"radius\": 2}", "missing_tag");
}

#[test]
fn null_and_big_numbers_dont_fit_an_int_field() {
    field_error(", \"count\": null", "unhandled_maybe");
    field_error(", \"count\": 9999999999999999999", "type_mismatch");
}

#[test]
fn bad_json_is_a_syntax_error_at_its_line() {
    let error = check_error("{\n  \"name\": }", "json_syntax");
    assert!(error.contains("--> place.json:2:"), "{error}");
    check_error("", "json_syntax");
}

#[test]
fn a_bare_none_in_a_con_file_is_the_expected_enums_variant() {
    let p = read(
        "place.con",
        "name = \"M\"\nspawn = (center = (0.0, 1.0, 0.0))\nsky = None",
    )
    .unwrap();
    assert_eq!(p.sky, Sky::None);
}
