//! The shape GitHub answers the standings and open-listing queries in, read off the
//! recordings under `tests/fixtures/graphql-recordings/`, and the check every answer
//! the substituted host serves to those queries is held to.
//!
//! A scenario the recorded repository was never in — a check still running, a head
//! that moved, a closed release change request beside two open ones — is served by
//! `standings_host.rs` from a journey's own records. What keeps those answers GitHub's
//! is this check: every object carries exactly the keys GitHub's answer to the same
//! selection carried at the same place, every value is the kind GitHub's schema gives
//! that field, and `null` appears only where `introspection.json` — GitHub's own
//! introspection of each type — says the field may be null. So a scenario varies the
//! values and nothing else, and an answer with an invented or a missing field is
//! refused before the binary sees it.
//!
//! The one way past it is a journey stating that it serves an answer GitHub would not
//! give (`Model::departs_from_github`), which is how a reader's refusals are proved.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

/// The recordings whose shape an answer is held to.
const RECORDINGS: [&str; 7] = [
    "printobserver-holdup-listing",
    "printobserver-holdup-standings",
    "printobserver-standings-without-closes",
    "printobserver-follow-51-first",
    "printobserver-follow-51-second",
    "vscode-standings-rulesets",
    "pydantic-standings-status-context",
];

/// What the recordings say: each place's admitted key sets, and the schema's types.
struct Recorded {
    /// Every key set an object at one place carried, the place spelled by [`place`].
    keys: BTreeMap<String, BTreeSet<Vec<String>>>,
    /// GitHub's introspection of each type, by name.
    types: BTreeMap<String, Value>,
}

fn read(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graphql-recordings")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the recording {}: {e}", path.display()));
    serde_json::from_str(&text).expect("a recording is JSON")
}

fn recorded() -> &'static Recorded {
    static RECORDED: OnceLock<Recorded> = OnceLock::new();
    RECORDED.get_or_init(|| {
        let mut keys = BTreeMap::new();
        for name in RECORDINGS {
            collect(&read(&format!("{name}.json")), "", &mut keys);
        }
        let types = read("introspection.json")
            .as_object()
            .expect("introspection by type name")
            .iter()
            .map(|(name, answer)| (name.clone(), answer["data"]["__type"].clone()))
            .collect();
        Recorded { keys, types }
    })
}

/// An alias of the standings query: `c` and its index.
fn alias(key: &str) -> bool {
    key.len() > 1 && key.starts_with('c') && key[1..].bytes().all(|b| b.is_ascii_digit())
}

/// One object's key set, every alias spelled `c*` once.
fn key_set(object: &serde_json::Map<String, Value>) -> Vec<String> {
    let set: BTreeSet<String> = object
        .keys()
        .map(|key| {
            if alias(key) {
                "c*".to_owned()
            } else {
                key.clone()
            }
        })
        .collect();
    set.into_iter().collect()
}

/// Where a value sits: the keys leading to it, aliases as `c*`, array elements as
/// `[]`, and an object naming its `__typename` suffixed with it.
fn place(at: &str, key: &str) -> String {
    format!("{at}/{}", if alias(key) { "c*" } else { key })
}

fn typed(at: &str, object: &serde_json::Map<String, Value>) -> String {
    match object.get("__typename").and_then(Value::as_str) {
        Some(name) => format!("{at}<{name}>"),
        None => at.to_owned(),
    }
}

fn collect(value: &Value, at: &str, keys: &mut BTreeMap<String, BTreeSet<Vec<String>>>) {
    match value {
        Value::Object(object) => {
            let here = typed(at, object);
            keys.entry(here.clone())
                .or_default()
                .insert(key_set(object));
            for (key, inner) in object {
                collect(inner, &place(&here, key), keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, &format!("{at}/[]"), keys);
            }
        }
        _ => {}
    }
}

/// A field's type as introspection spells it: whether it may be null, and what it is.
enum Shape<'a> {
    List(Box<(bool, Shape<'a>)>),
    Named(&'a str, &'a str),
}

fn shape_of(kind: &Value) -> (bool, Shape<'_>) {
    match kind["kind"].as_str() {
        Some("NON_NULL") => {
            let (_, inner) = shape_of(&kind["ofType"]);
            (false, inner)
        }
        Some("LIST") => (true, Shape::List(Box::new(shape_of(&kind["ofType"])))),
        Some(category) => (
            true,
            Shape::Named(category, kind["name"].as_str().expect("a named type")),
        ),
        None => panic!("an introspected type without a kind: {kind}"),
    }
}

impl Recorded {
    fn introspected(&self, name: &str) -> Result<&Value, String> {
        self.types
            .get(name)
            .ok_or_else(|| format!("introspection.json does not hold the type {name}"))
    }

    /// The type of `field` on the object type `owner` — through the member `__typename`
    /// names, or the first member declaring it, where `owner` is abstract.
    fn field(&self, owner: &str, typename: Option<&str>, field: &str) -> Result<&Value, String> {
        let introspected = self.introspected(owner)?;
        fn declared<'v>(of: &'v Value, field: &str) -> Option<&'v Value> {
            of["fields"].as_array().and_then(|fields| {
                fields
                    .iter()
                    .find(|candidate| candidate["name"] == field)
                    .map(|found| &found["type"])
            })
        }
        if let Some(found) = declared(introspected, field) {
            return Ok(found);
        }
        let members: Vec<&str> = introspected["possibleTypes"]
            .as_array()
            .map(|members| members.iter().filter_map(|m| m["name"].as_str()).collect())
            .unwrap_or_default();
        for member in members {
            if typename.is_some_and(|wanted| wanted != member) {
                continue;
            }
            if let Ok(of) = self.introspected(member) {
                if let Some(found) = declared(of, field) {
                    return Ok(found);
                }
            }
        }
        Err(format!("GitHub's {owner} has no field {field}"))
    }

    fn check(&self, value: &Value, owner: &str, at: &str) -> Result<(), String> {
        let object = value.as_object().ok_or_else(|| {
            format!("{at}: GitHub answers an object of {owner} here, not {value}")
        })?;
        let here = typed(at, object);
        let admitted = self
            .keys
            .get(&here)
            .ok_or_else(|| format!("{here}: no recorded answer carries an object here"))?;
        let keys = key_set(object);
        if !admitted.contains(&keys) {
            return Err(format!(
                "{here}: GitHub answered this selection with the keys {admitted:?}, not {keys:?}"
            ));
        }
        let typename = object.get("__typename").and_then(Value::as_str);
        for (key, inner) in object {
            let at = place(&here, key);
            if key == "__typename" {
                if !inner.is_string() {
                    return Err(format!("{at}: a type name is a string, not {inner}"));
                }
                continue;
            }
            let field = if alias(key) { "pullRequest" } else { key };
            let kind = self.field(owner, typename, field)?;
            self.value(inner, &shape_of(kind), &at)?;
        }
        Ok(())
    }

    fn value(
        &self,
        value: &Value,
        (nullable, shape): &(bool, Shape<'_>),
        at: &str,
    ) -> Result<(), String> {
        if value.is_null() {
            return if *nullable {
                Ok(())
            } else {
                Err(format!("{at}: GitHub's schema never answers null here"))
            };
        }
        match shape {
            Shape::List(inner) => value
                .as_array()
                .ok_or_else(|| format!("{at}: GitHub answers a list here, not {value}"))?
                .iter()
                .try_for_each(|item| self.value(item, inner, &format!("{at}/[]"))),
            Shape::Named("OBJECT" | "INTERFACE" | "UNION", name) => self.check(value, name, at),
            Shape::Named(_, "Int") if value.is_i64() || value.is_u64() => Ok(()),
            Shape::Named(_, "Boolean") if value.is_boolean() => Ok(()),
            Shape::Named(_, "Int" | "Boolean") => Err(format!(
                "{at}: not the kind GitHub's schema answers: {value}"
            )),
            Shape::Named(_, _) if value.is_string() => Ok(()),
            Shape::Named(_, name) => Err(format!(
                "{at}: GitHub answers a {name} as a string, not {value}"
            )),
        }
    }
}

/// Whether `answer` — a whole `gh api graphql` answer to the standings or the
/// open-listing query — is in the shape GitHub answers those queries in; why not if not.
pub fn conforms(answer: &Value) -> Result<(), String> {
    let recorded = recorded();
    let object = answer
        .as_object()
        .ok_or_else(|| "an answer is an object".to_owned())?;
    let keys = key_set(object);
    if keys != ["data"] {
        return Err(format!(
            "GitHub answered these queries with `data` alone, not {keys:?}"
        ));
    }
    recorded.check(&answer["data"], "Query", "/data")
}

#[test]
fn every_recorded_answer_conforms_to_the_shape_it_was_read_from() {
    for name in RECORDINGS {
        let answer = read(&format!("{name}.json"));
        conforms(&answer).unwrap_or_else(|why| panic!("{name}: {why}"));
    }
}

#[test]
fn an_answer_with_a_field_invented_omitted_or_null_where_the_schema_forbids_is_refused() {
    let mut answer = read("printobserver-holdup-standings.json");
    let context = "/data/repository/c0/headRef/target/statusCheckRollup/contexts/nodes/0";
    answer
        .pointer_mut(context)
        .and_then(Value::as_object_mut)
        .expect("a recorded context")
        .remove("detailsUrl");
    let why = conforms(&answer).expect_err("an omitted detailsUrl");
    assert!(why.contains("detailsUrl"), "{why}");

    let mut answer = read("printobserver-holdup-standings.json");
    answer.pointer_mut(context).expect("a context")["detailsUrl"] = Value::Null;
    conforms(&answer).expect("detailsUrl is a nullable URI");
    answer.pointer_mut(context).expect("a context")["isRequired"] = Value::Null;
    let why = conforms(&answer).expect_err("isRequired is Boolean!");
    assert!(why.contains("never answers null"), "{why}");

    let mut answer = read("printobserver-holdup-listing.json");
    answer["data"]["repository"]["pullRequests"]["nodes"][0]["url"] =
        "https://github.com/nickderobertis/printobserver/pull/298".into();
    let why = conforms(&answer).expect_err("a field the listing does not select");
    assert!(why.contains("url"), "{why}");

    let mut answer = read("printobserver-holdup-listing.json");
    answer["data"]["repository"]["pullRequests"]["nodes"][0]["number"] = "298".into();
    let why = conforms(&answer).expect_err("a number GitHub answers as an Int");
    assert!(why.contains("not the kind"), "{why}");
}
