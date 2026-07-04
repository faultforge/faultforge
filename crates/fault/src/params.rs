//! The typed, closed parameter schema and its pure validator.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A v1 parameter type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    /// A JSON string.
    String,
    /// A JSON integer (a number with no fractional part).
    Int,
    /// A JSON boolean.
    Bool,
}

impl ParamType {
    /// Returns `true` if `value` is of this type.
    ///
    /// `Int` accepts only integer JSON numbers; a float such as `3.5` (or `3.0`,
    /// which JSON parses as a float) is rejected.
    #[must_use]
    fn matches(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Int => value.is_i64() || value.is_u64(),
            Self::Bool => value.is_boolean(),
        }
    }
}

/// A plugin's parameter schema: a closed map of parameter name to type. Every
/// declared parameter is required; no defaults.
pub type ParamsSchema = BTreeMap<String, ParamType>;

/// Why supplied parameters failed validation against a [`ParamsSchema`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParamsError {
    /// A parameter declared in the schema was not supplied.
    #[error("missing required parameter: {0}")]
    Missing(String),
    /// A supplied parameter is not declared in the schema.
    #[error("undeclared parameter: {0}")]
    Undeclared(String),
    /// A supplied parameter has the wrong type.
    #[error("parameter '{name}' has the wrong type, expected {expected:?}")]
    TypeMismatch {
        /// The offending parameter name.
        name: String,
        /// The type the schema declared.
        expected: ParamType,
    },
}

/// Validate supplied `params` against `schema`.
///
/// This is the one pure function both the master (before dispatch) and the agent
/// (before execution) use, so validation cannot drift between the two ends.
///
/// # Errors
///
/// Returns the first of: [`ParamsError::Undeclared`] for a key not in `schema`,
/// [`ParamsError::Missing`] for a declared key with no value, or
/// [`ParamsError::TypeMismatch`] for a value of the wrong type.
pub fn validate_params(
    schema: &ParamsSchema,
    params: &Map<String, Value>,
) -> Result<(), ParamsError> {
    for key in params.keys() {
        if !schema.contains_key(key) {
            return Err(ParamsError::Undeclared(key.clone()));
        }
    }
    for (name, expected) in schema {
        match params.get(name) {
            None => return Err(ParamsError::Missing(name.clone())),
            Some(value) if !expected.matches(value) => {
                return Err(ParamsError::TypeMismatch {
                    name: name.clone(),
                    expected: *expected,
                });
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> ParamsSchema {
        BTreeMap::from([
            ("marker_path".to_string(), ParamType::String),
            ("count".to_string(), ParamType::Int),
            ("force".to_string(), ParamType::Bool),
        ])
    }

    fn params(value: &Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn all_present_and_well_typed_is_ok() {
        let p = params(&json!({"marker_path": "/tmp/m", "count": 3, "force": true}));
        assert_eq!(validate_params(&schema(), &p), Ok(()));
    }

    #[test]
    fn missing_parameter_is_named() {
        let p = params(&json!({"count": 3, "force": true}));
        assert_eq!(
            validate_params(&schema(), &p),
            Err(ParamsError::Missing("marker_path".to_string()))
        );
    }

    #[test]
    fn undeclared_parameter_is_named() {
        let p = params(&json!({"marker_path": "/tmp/m", "count": 3, "force": true, "extra": 1}));
        assert_eq!(
            validate_params(&schema(), &p),
            Err(ParamsError::Undeclared("extra".to_string()))
        );
    }

    #[test]
    fn string_declared_but_number_supplied_is_type_mismatch() {
        let p = params(&json!({"marker_path": 7, "count": 3, "force": true}));
        assert_eq!(
            validate_params(&schema(), &p),
            Err(ParamsError::TypeMismatch {
                name: "marker_path".to_string(),
                expected: ParamType::String,
            })
        );
    }

    #[test]
    fn int_rejects_a_float() {
        let p = params(&json!({"marker_path": "/tmp/m", "count": 3.5, "force": true}));
        assert_eq!(
            validate_params(&schema(), &p),
            Err(ParamsError::TypeMismatch {
                name: "count".to_string(),
                expected: ParamType::Int,
            })
        );
    }

    #[test]
    fn param_type_serde_is_lowercase() {
        assert_eq!(serde_json::to_string(&ParamType::Int).unwrap(), "\"int\"");
        let t: ParamType = serde_json::from_str("\"bool\"").unwrap();
        assert_eq!(t, ParamType::Bool);
    }
}
