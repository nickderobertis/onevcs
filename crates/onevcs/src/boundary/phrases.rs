//! The host's public phrases: `$ONEVCS_HOME/public-phrases.yml`, read and held to its
//! one version.
//!
//! Unlike the other host files this one is read **strictly**. It narrows what the public
//! boundary refuses, so a file this build cannot read exactly is never read as some
//! smaller list: an unknown key, a version other than [`PUBLIC_PHRASES_VERSION`], an
//! empty phrase, YAML that does not parse, a file that cannot be read and a path that is
//! not a regular file are each refused, and the check that asked is unavailable. Only a
//! **missing** file is an answer — no phrases.
//!
//! A refusal names the file and what is wrong with it, and never a phrase: a phrase is
//! text that carries a private term, and a refusal is printed.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::matcher::{normalize, term_key};
use super::{PublicPhrases, PUBLIC_PHRASES_FILE, PUBLIC_PHRASES_VERSION};
use crate::error::{self, Result};
use crate::{home, yaml};

/// Where this host keeps its public phrases.
fn path() -> Result<PathBuf> {
    Ok(home::root()?.join(PUBLIC_PHRASES_FILE))
}

/// This host's public phrases, or none where it has no file.
pub fn load() -> Result<PublicPhrases> {
    let path = path()?;
    let refused =
        |problem: String| error::invalid(format!("{} is refused: {problem}", display(&path)));
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PublicPhrases::default())
        }
        Err(failure) => return Err(refused(format!("it cannot be read: {failure}"))),
    };
    if !metadata.is_file() {
        return Err(refused("it is not a regular file".to_owned()));
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|failure| refused(format!("it cannot be read: {failure}")))?;
    parse(&raw).map_err(refused)
}

fn display(path: &Path) -> String {
    format!("the public phrase file at {}", path.display())
}

/// One document's phrases, or what is wrong with it in words that quote none of it.
pub(crate) fn parse(raw: &str) -> std::result::Result<PublicPhrases, String> {
    // What the YAML reader says about a document it could not parse can quote the
    // document, so it is not repeated.
    let document: Value =
        yaml::from_str(raw).map_err(|_| "it does not parse as YAML".to_owned())?;
    let Value::Object(document) = document else {
        return Err("it is not a mapping of `version` and `phrases`".to_owned());
    };
    if document
        .keys()
        .any(|key| key != "version" && key != "phrases")
    {
        return Err("it carries a key other than `version` and `phrases`".to_owned());
    }
    match document.get("version") {
        None => return Err("it declares no `version`".to_owned()),
        Some(version) => match version.as_u64() {
            Some(v) if v == u64::from(PUBLIC_PHRASES_VERSION) => {}
            Some(v) => {
                return Err(format!(
                "it declares version {v}; this build reads version {PUBLIC_PHRASES_VERSION} only"
            ))
            }
            None => return Err("its `version` is not a whole number".to_owned()),
        },
    }
    let Some(listed) = document.get("phrases") else {
        return Err("it declares no `phrases` list".to_owned());
    };
    let Value::Array(listed) = listed else {
        return Err("its `phrases` is not a list".to_owned());
    };
    let mut phrases: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for (index, entry) in listed.iter().enumerate() {
        let Value::String(phrase) = entry else {
            return Err(format!("phrase {} is not a string", index + 1));
        };
        if normalize(phrase).is_empty() {
            return Err(format!("phrase {} is empty once normalized", index + 1));
        }
        let key = term_key(phrase);
        if !keys.contains(&key) {
            keys.push(key);
            phrases.push(phrase.clone());
        }
    }
    Ok(PublicPhrases { phrases })
}
