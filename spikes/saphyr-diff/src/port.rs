//! Each of today's readers ported to serde-saphyr, line for line, under a `Flavor`:
//! which tree the schema-less walks use, how the typed value is read, and which
//! serde-saphyr `Options` are set. Everything a refusal says that is this
//! repository's own wording is copied verbatim from the reader it ports, so a
//! difference the harness reports is one the crate swap makes.

use std::path::Path;

use onevcs::rules::{BaseMerge, Checks, Drafts, RulesFile};
use onevcs::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as J};
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options};

/// How the typed value is read once the walks are done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    /// `serde_json::from_value` over the walked tree (today's `from_value` shape).
    Json,
    /// `serde_saphyr::from_str` over the raw text (the walks still use the tree).
    Direct,
}

#[derive(Clone, Copy)]
pub struct Flavor {
    pub name: &'static str,
    pub typed: Typed,
    pub strict_booleans: bool,
    pub merge_as_ordinary: bool,
    pub no_schema: bool,
    pub legacy_octal: bool,
    pub with_snippet: bool,
    pub reject_non_finite: bool,
    /// The small code changes that reshape what serde-saphyr and serde_json say into
    /// what serde_yaml_ng said (see `shape_tree`, `shape_error` and `spelled`).
    pub code_change: bool,
}

impl Flavor {
    pub fn options(&self) -> Options {
        serde_saphyr::options! {
            strict_booleans: self.strict_booleans,
            no_schema: self.no_schema,
            legacy_octal_numbers: self.legacy_octal,
            merge_keys: if self.merge_as_ordinary { MergeKeyPolicy::AsOrdinary } else { MergeKeyPolicy::Merge },
            duplicate_keys: DuplicateKeyPolicy::Error,
            with_snippet: self.with_snippet,
            reject_non_finite_typeless_float: self.reject_non_finite,
        }
    }

    /// The document as the walks see it: `serde_json::Value` with `preserve_order`.
    pub fn tree(&self, raw: &str) -> Result<J, String> {
        if self.code_change {
            // CODE CHANGE: today's refusal of a stream of several documents.
            if let Ok(docs) = serde_saphyr::from_multiple_with_options::<J>(raw, self.options()) {
                if docs.len() > 1 {
                    return Err("deserializing from YAML containing more than one document is not supported".to_owned());
                }
            }
        }
        serde_saphyr::from_str_with_options::<J>(raw, self.options()).map_err(|e| e.to_string())
    }

    /// CODE CHANGE: what the typed read is handed. serde_yaml_ng read an empty
    /// document as a struct with every field missing, and refused a sequence where
    /// a struct goes; `serde_json::from_value` reads `null` as `invalid type: null`
    /// and a sequence as a struct's fields in order.
    fn shape_tree<T>(&self, tree: J) -> Result<J, String> {
        if !self.code_change {
            return Ok(tree);
        }
        let name = std::any::type_name::<T>().rsplit("::").next().unwrap_or("");
        match tree {
            J::Null => Ok(J::Object(Map::new())),
            J::Array(_) => Err(format!("invalid type: sequence, expected struct {name}")),
            other => Ok(other),
        }
    }

    /// CODE CHANGE: serde_json names a null `null` where serde_yaml_ng said `unit value`.
    fn shape_error(&self, said: String) -> String {
        if self.code_change {
            said.replace("invalid type: null,", "invalid type: unit value,")
        } else {
            said
        }
    }

    fn typed<T: for<'de> Deserialize<'de>>(&self, raw: &str, tree: J) -> Result<T, String> {
        match self.typed {
            Typed::Json => serde_json::from_value(self.shape_tree::<T>(tree)?)
                .map_err(|e| self.shape_error(e.to_string())),
            Typed::Direct => serde_saphyr::from_str_with_options(raw, self.options())
                .map_err(|e| e.to_string()),
        }
    }

    /// One single-key mapping put to `T`, the way the drafts and checks walks do.
    fn alone<T: for<'de> Deserialize<'de>>(&self, key: &str, value: &J) -> Result<(), String> {
        let mut alone = Map::new();
        alone.insert(key.to_owned(), value.clone());
        self.value::<T>(&J::Object(alone))
    }

    fn value<T: for<'de> Deserialize<'de>>(&self, value: &J) -> Result<(), String> {
        match self.typed {
            Typed::Json => serde_json::from_value::<T>(value.clone())
                .map(|_| ())
                .map_err(|e| self.shape_error(e.to_string())),
            Typed::Direct => {
                let text = serde_saphyr::to_string(value).map_err(|e| e.to_string())?;
                serde_saphyr::from_str_with_options::<T>(&text, self.options())
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }
}

const BASE: Flavor = Flavor {
    name: "",
    typed: Typed::Json,
    strict_booleans: false,
    merge_as_ordinary: false,
    no_schema: false,
    legacy_octal: false,
    with_snippet: true,
    reject_non_finite: true,
    code_change: false,
};

/// Each flavor adds one knob to the one before it (the two typed paths each start
/// from serde-saphyr's defaults), so the first flavor to print "= today" for a
/// difference names the option that reproduces today's behaviour.
pub const FLAVORS: &[Flavor] = &[
    Flavor { name: "json", ..BASE },
    Flavor { name: "json+strict_booleans", strict_booleans: true, ..BASE },
    Flavor { name: "json+strict_booleans+merge_as_ordinary", strict_booleans: true, merge_as_ordinary: true, ..BASE },
    Flavor { name: "json+strict_booleans+merge_as_ordinary+non_finite_ok", strict_booleans: true, merge_as_ordinary: true, reject_non_finite: false, ..BASE },
    Flavor { name: "json+strict_booleans+merge_as_ordinary+non_finite_ok+no_snippet", strict_booleans: true, merge_as_ordinary: true, reject_non_finite: false, with_snippet: false, ..BASE },
    Flavor { name: "json+strict_booleans+merge_as_ordinary+non_finite_ok+no_snippet+code_change", strict_booleans: true, merge_as_ordinary: true, reject_non_finite: false, with_snippet: false, code_change: true, ..BASE },
    Flavor { name: "direct", typed: Typed::Direct, ..BASE },
    Flavor { name: "direct+strict_booleans+merge_as_ordinary+no_snippet", typed: Typed::Direct, strict_booleans: true, merge_as_ordinary: true, with_snippet: false, ..BASE },
    Flavor { name: "direct+strict_booleans+merge_as_ordinary+no_snippet+non_finite_ok", typed: Typed::Direct, strict_booleans: true, merge_as_ordinary: true, with_snippet: false, reject_non_finite: false, ..BASE },
    Flavor { name: "direct+strict_booleans+merge_as_ordinary+no_snippet+non_finite_ok+no_schema", typed: Typed::Direct, strict_booleans: true, merge_as_ordinary: true, with_snippet: false, reject_non_finite: false, no_schema: true, ..BASE },
];

fn invalid(reason: String) -> Error {
    Error::Invalid { reason }
}

/// One YAML value as the port spells it in a refusal: serde-saphyr's emission, trimmed.
pub fn spelled(value: &J) -> String {
    serde_saphyr::to_string(value)
        .map(|spelled| spelled.trim().to_owned())
        .unwrap_or_default()
}

/// CODE CHANGE: a number spelled as serde_json's `Display` writes it (ryu, as
/// serde_yaml_ng did: `1e300`, not serde-saphyr's `1.0e+300`), and a non-finite float
/// — which a typeless read with `reject_non_finite_typeless_float: false` hands back as
/// the string `.inf`/`-.inf`/`.nan` — written bare, as it was written.
pub fn spelled_shaped(value: &J) -> String {
    match value {
        J::Number(n) => n.to_string(),
        J::String(s) if matches!(s.as_str(), ".inf" | "-.inf" | ".nan") => s.clone(),
        other => spelled(other),
    }
}

impl Flavor {
    fn spell(&self, value: &J) -> String {
        if self.code_change {
            spelled_shaped(value)
        } else {
            spelled(value)
        }
    }
}

// ---------------------------------------------------------------- rules (policy.rs)

pub fn rules(path: &Path, raw: &str, f: &Flavor) -> Result<RulesFile, Error> {
    let malformed =
        |e: String| invalid(format!("the rules file at {} is malformed: {e}", path.display()));
    let mut document = f.tree(raw).map_err(malformed)?;
    if let Some(declared) = document.get("version").and_then(J::as_u64) {
        let oldest = onevcs::spike::RULES_OLDEST_VERSION;
        if declared < u64::from(oldest) {
            return Err(invalid(format!(
                "the rules file at {} declares version {declared}; this build reads version \
                 {oldest} and newer",
                path.display(),
            )));
        }
        if declared < u64::from(onevcs::spike::GATE_REMOVED_VERSION) {
            onevcs::spike::report_spent_gate(path, drop_gate(&mut document));
        } else {
            refuse_removed_gate(path, &document)?;
        }
    }
    refuse_malformed_drafts(path, &document, f)?;
    refuse_malformed_base_merge(path, &document, f)?;
    refuse_malformed_checks(path, &document, f)?;
    let file: RulesFile = f.typed(raw, document).map_err(malformed)?;
    onevcs::spike::rules_validate(path, &file)?;
    Ok(file)
}

fn drop_gate(document: &mut J) -> usize {
    let mut dropped = 0;
    let mut take = |from: Option<&mut J>| {
        if let Some(J::Object(fields)) = from {
            dropped += usize::from(fields.shift_remove("gate").is_some());
        }
    };
    take(document.get_mut("default"));
    if let Some(J::Array(rules)) = document.get_mut("rules") {
        for rule in rules {
            take(Some(rule));
        }
    }
    dropped
}

fn refuse_removed_gate(path: &Path, document: &J) -> Result<(), Error> {
    let named = |value: Option<&J>, where_: String| match value {
        Some(J::Object(fields)) if fields.contains_key("gate") => Some(where_),
        _ => None,
    };
    let found = named(document.get("default"), "default".to_owned()).or_else(|| {
        match document.get("rules") {
            Some(J::Array(rules)) => rules
                .iter()
                .enumerate()
                .find_map(|(index, rule)| named(Some(rule), format!("rule {}", index + 1))),
            _ => None,
        }
    });
    match found {
        None => Ok(()),
        Some(where_) => Err(invalid(format!(
            "the rules file at {} is malformed: unknown field `gate` in {where_}, which \
             version {} removed. What verifies a change is the \
             repository's own merge path — the host's required checks, or its pre-push \
             hook — and `onevcs repos --audit-gates` reports which of those each identity \
             has. Delete the gate keys",
            path.display(),
            onevcs::spike::GATE_REMOVED_VERSION
        ))),
    }
}

fn policy_keys<'d>(document: &'d J, key: &str) -> Vec<(String, &'d J)> {
    let mut found = Vec::new();
    if let Some(value) = document.get("default").and_then(|policy| policy.get(key)) {
        found.push(("default".to_owned(), value));
    }
    if let Some(J::Array(rules)) = document.get("rules") {
        for (index, rule) in rules.iter().enumerate() {
            if let Some(value) = rule.get(key) {
                found.push((format!("rule {}", index + 1), value));
            }
        }
    }
    found
}

fn refuse_malformed_drafts(path: &Path, document: &J, f: &Flavor) -> Result<(), Error> {
    let refused = |where_: &str, what: String| {
        invalid(format!(
            "the rules file at {} is malformed: {where_} drafts: {what}",
            path.display()
        ))
    };
    for (where_, drafts) in policy_keys(document, "drafts") {
        let J::Object(fields) = drafts else {
            return Err(refused(&where_, "is not a mapping".to_owned()));
        };
        for (key, value) in fields {
            let Err(said) = f.alone::<Drafts>(key, value) else {
                continue;
            };
            let key = f.spell(&J::String(key.clone()));
            return Err(refused(
                &where_,
                if value.is_boolean() {
                    format!("names {key:?}, which it does not have: {said}")
                } else {
                    format!(
                        "{key} is {}, which is not a boolean; write true or false",
                        f.spell(value)
                    )
                },
            ));
        }
    }
    Ok(())
}

fn refuse_malformed_base_merge(path: &Path, document: &J, f: &Flavor) -> Result<(), Error> {
    for (where_, value) in policy_keys(document, "base_merge") {
        if f.value::<BaseMerge>(value).is_err() {
            return Err(invalid(format!(
                "the rules file at {} is malformed: {where_} base_merge: {} is not a base \
                 merge; write always or on-conflict",
                path.display(),
                f.spell(value)
            )));
        }
    }
    Ok(())
}

fn refuse_malformed_checks(path: &Path, document: &J, f: &Flavor) -> Result<(), Error> {
    let refused = |where_: &str, what: String| {
        invalid(format!(
            "the rules file at {} is malformed: {where_} checks: {what}",
            path.display()
        ))
    };
    for (where_, checks) in policy_keys(document, "checks") {
        let J::Object(fields) = checks else {
            return Err(refused(&where_, "is not a mapping".to_owned()));
        };
        for (key, value) in fields {
            let Err(said) = f.alone::<Checks>(key, value) else {
                continue;
            };
            let key = f.spell(&J::String(key.clone()));
            return Err(refused(
                &where_,
                if key == "wait_seconds" {
                    format!(
                        "wait_seconds is {}, which is not a wait; write {}",
                        f.spell(value),
                        onevcs::spike::WAIT_SECONDS_RANGE
                    )
                } else {
                    format!("names {key:?}, which it does not have: {said}")
                },
            ));
        }
    }
    Ok(())
}

// ------------------------------------------- the three host files (version-first)

/// `release::load`, `workspaces::load` and `branches::load` share one shape: read the
/// tree, refuse a version below 1 by number, read the type, validate.
pub fn host_file<T: for<'de> Deserialize<'de>>(
    noun: &str,
    path: &Path,
    raw: &str,
    f: &Flavor,
    validate: impl Fn(&Path, &T) -> onevcs::Result<()>,
) -> Result<T, Error> {
    let malformed =
        |e: String| invalid(format!("the {noun} file at {} is malformed: {e}", path.display()));
    let document = f.tree(raw).map_err(malformed)?;
    if let Some(declared) = document.get("version").and_then(J::as_u64) {
        if declared < 1 {
            return Err(invalid(format!(
                "the {noun} file at {} declares version {declared}; this build reads \
                 version 1 and newer",
                path.display()
            )));
        }
    }
    let file: T = f.typed(raw, document).map_err(malformed)?;
    validate(path, &file)?;
    Ok(file)
}

// ------------------------------------------------- rules apply (rules_apply.rs)

/// `strictly` over `serde_json::Value`: keys are strings, so a non-string key is
/// whatever serde-saphyr's `deserialize_any` made of it (or a refusal).
pub fn strictly_json<T: Serialize + for<'de> Deserialize<'de>>(
    path: &Path,
    raw: &str,
    f: &Flavor,
) -> Result<T, Error> {
    let malformed =
        |e: String| invalid(format!("the rules file at {} is malformed: {e}", path.display()));
    let document = f.tree(raw).map_err(malformed)?;
    let typed: T = f.typed(raw, document.clone()).map_err(malformed)?;
    let understood = serde_json::to_value(&typed).map_err(|e| malformed(e.to_string()))?;
    if let Some(key) = unknown_key_json(&document, &understood, "") {
        return Err(unknown(path, &key));
    }
    Ok(typed)
}

fn unknown(path: &Path, key: &str) -> Error {
    invalid(format!(
        "the rules file at {} names `{key}`, which this build does not have; nothing \
         was installed",
        path.display()
    ))
}

fn unknown_key_json(given: &J, understood: &J, at: &str) -> Option<String> {
    match (given, understood) {
        (J::Object(given), J::Object(understood)) => {
            for (key, value) in given {
                let path = if at.is_empty() { key.clone() } else { format!("{at}.{key}") };
                match understood.get(key) {
                    None if !value.is_null() => return Some(path),
                    None => {}
                    Some(seen) => {
                        if let Some(found) = unknown_key_json(value, seen, &path) {
                            return Some(found);
                        }
                    }
                }
            }
            None
        }
        (J::Array(given), J::Array(understood)) if given.len() == understood.len() => given
            .iter()
            .zip(understood)
            .enumerate()
            .find_map(|(index, (value, seen))| {
                unknown_key_json(value, seen, &format!("{at}[{}]", index + 1))
            }),
        _ => None,
    }
}

/// `strictly` over `saphyr`'s node tree: the given document keeps every key's own
/// node, so a non-string key survives; the understood one is the typed value written
/// by serde-saphyr and loaded again. A key is named by its source text.
pub fn strictly_saphyr<T: Serialize + for<'de> Deserialize<'de>>(
    path: &Path,
    raw: &str,
    f: &Flavor,
) -> Result<T, Error> {
    use saphyr::{LoadableYamlNode, Yaml};
    let malformed =
        |e: String| invalid(format!("the rules file at {} is malformed: {e}", path.display()));
    let given = Yaml::load_from_str(raw).map_err(|e| malformed(e.to_string()))?;
    let given = given.into_iter().next().unwrap_or(Yaml::Value(saphyr::Scalar::Null));
    let tree = f.tree(raw).map_err(malformed)?;
    let typed: T = f.typed(raw, tree).map_err(malformed)?;
    let written = serde_saphyr::to_string(&typed).map_err(|e| malformed(e.to_string()))?;
    let understood = Yaml::load_from_str(&written).map_err(|e| malformed(e.to_string()))?;
    let understood = understood.into_iter().next().unwrap_or(Yaml::Value(saphyr::Scalar::Null));
    if let Some(key) = unknown_key_saphyr(&given, &understood, "") {
        return Err(unknown(path, &key));
    }
    Ok(typed)
}

pub fn saphyr_key_name(key: &saphyr::Yaml) -> String {
    use saphyr::{Scalar, Yaml};
    match key {
        Yaml::Value(Scalar::String(s)) => s.to_string(),
        Yaml::Value(Scalar::Integer(i)) => i.to_string(),
        Yaml::Value(Scalar::FloatingPoint(x)) => x.to_string(),
        Yaml::Value(Scalar::Boolean(b)) => b.to_string(),
        Yaml::Value(Scalar::Null) => "null".to_owned(),
        Yaml::Representation(text, _, _) => text.to_string(),
        other => format!("{other:?}"),
    }
}

fn unknown_key_saphyr(given: &saphyr::Yaml, understood: &saphyr::Yaml, at: &str) -> Option<String> {
    use saphyr::Yaml;
    match (given, understood) {
        (Yaml::Mapping(given), Yaml::Mapping(understood)) => {
            for (key, value) in given {
                let name = saphyr_key_name(key);
                let path = if at.is_empty() { name } else { format!("{at}.{name}") };
                match understood.get(key) {
                    None if !value.is_null() => return Some(path),
                    None => {}
                    Some(seen) => {
                        if let Some(found) = unknown_key_saphyr(value, seen, &path) {
                            return Some(found);
                        }
                    }
                }
            }
            None
        }
        (Yaml::Sequence(given), Yaml::Sequence(understood)) if given.len() == understood.len() => {
            given.iter().zip(understood).enumerate().find_map(|(index, (value, seen))| {
                unknown_key_saphyr(value, seen, &format!("{at}[{}]", index + 1))
            })
        }
        _ => None,
    }
}
