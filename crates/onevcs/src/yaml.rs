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

#[cfg(test)]
mod tests;
