//! How this crate reads and writes YAML: through serde-saphyr, every host file under
//! the one set of reader options [`options`] states.

use serde::de::DeserializeOwned;
use serde::Serialize;

mod options;

use options::options;

/// One YAML document read as `T`.
pub(crate) fn from_str<T: DeserializeOwned>(raw: &str) -> Result<T, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(raw, options())
}

/// `value` written as one YAML document, in serde-saphyr's own default style.
pub(crate) fn to_string<T: Serialize>(value: &T) -> Result<String, serde_saphyr::SerializeError> {
    serde_saphyr::to_string(value)
}

/// A value a walk found in a document, read as `T` the way the typed read of that
/// document takes it: written back as YAML and read through the same reader, so a quoted
/// `"true"` or `"3600"` converts here exactly where it converts there.
pub(crate) fn convert<T: DeserializeOwned>(value: &serde_json::Value) -> Option<T> {
    from_str(&to_string(value).ok()?).ok()
}

/// The `version` a document declares, read the way its typed read reads it.
pub(crate) fn declared_version(document: &serde_json::Value) -> Option<u64> {
    convert(document.get("version")?)
}

#[cfg(test)]
mod tests;
