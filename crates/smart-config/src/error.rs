//! Config deserialization errors.

use std::{fmt, sync::Arc};

use serde::{de, de::Error};

use crate::{
    metadata::{ConfigMetadata, ParamMetadata},
    value::{ValueOrigin, WithOrigin},
};

/// Marker error for [`DeserializeConfig`](crate::DeserializeConfig) operations. The error info os stored
/// in [`DeserializeContext`](crate::de::DeserializeContext) as [`ParseErrors`].
#[derive(Debug)]
pub struct DeserializeConfigError(());

impl DeserializeConfigError {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum LocationInConfig {
    Param(usize),
}

#[doc(hidden)] // variants not stabilized yet
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ParseErrorCategory {
    /// Generic error.
    Generic,
    /// Missing field (parameter / config) error.
    MissingField,
}

/// Low-level deserialization error.
#[derive(Debug)]
#[non_exhaustive]
pub enum LowLevelError {
    /// Error coming from JSON deserialization logic.
    Json {
        err: serde_json::Error,
        category: ParseErrorCategory,
    },
    #[doc(hidden)] // implementation detail
    InvalidArray,
    #[doc(hidden)] // implementation detail
    InvalidObject,
    #[doc(hidden)] // implementation detail
    Validation,
}

impl From<serde_json::Error> for LowLevelError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json {
            err,
            category: ParseErrorCategory::Generic,
        }
    }
}

impl fmt::Display for LowLevelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json { err, .. } => fmt::Display::fmt(err, formatter),
            Self::InvalidArray => formatter.write_str("error(s) deserializing array items"),
            Self::InvalidObject => formatter.write_str("error(s) deserializing object entries"),
            Self::Validation => formatter.write_str("validation failed"),
        }
    }
}

/// Error together with its origin.
pub type ErrorWithOrigin = WithOrigin<LowLevelError>;

impl ErrorWithOrigin {
    pub(crate) fn json(err: serde_json::Error, origin: Arc<ValueOrigin>) -> Self {
        Self::new(err.into(), origin)
    }

    /// Creates a custom error.
    pub fn custom(message: impl fmt::Display) -> Self {
        Self::json(de::Error::custom(message), Arc::default())
    }
}

impl de::Error for ErrorWithOrigin {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::json(de::Error::custom(msg), Arc::default())
    }

    fn missing_field(field: &'static str) -> Self {
        let err = LowLevelError::Json {
            err: de::Error::missing_field(field),
            category: ParseErrorCategory::MissingField,
        };
        Self::new(err, Arc::default())
    }
}

impl fmt::Display for ErrorWithOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}]: {}", self.origin, self.inner)
    }
}

impl std::error::Error for ErrorWithOrigin {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.inner {
            LowLevelError::Json { err, .. } => Some(err),
            LowLevelError::InvalidArray
            | LowLevelError::InvalidObject
            | LowLevelError::Validation => None,
        }
    }
}

/// Config parameter deserialization errors.
pub struct ParseError {
    pub(crate) inner: serde_json::Error,
    pub(crate) category: ParseErrorCategory,
    pub(crate) path: String,
    pub(crate) origin: Arc<ValueOrigin>,
    pub(crate) config: &'static ConfigMetadata,
    pub(crate) location_in_config: Option<LocationInConfig>,
    pub(crate) validation: Option<String>,
}

impl fmt::Debug for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParseError")
            .field("inner", &self.inner)
            .field("origin", &self.origin)
            .field("path", &self.path)
            .field("config.ty", &self.config.ty)
            .field("location_in_config", &self.location_in_config)
            .field("validation", &self.validation)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = self.location_in_config.and_then(|location| {
            Some(match location {
                LocationInConfig::Param(idx) => {
                    let param = self.config.params.get(idx)?;
                    format!("param `{}` in ", param.name)
                }
            })
        });
        let field = field.as_deref().unwrap_or("");

        let origin = if matches!(self.origin(), ValueOrigin::Unknown) {
            String::new()
        } else {
            format!(" [origin: {}]", self.origin)
        };

        let failed_action = if let Some(validation) = &self.validation {
            format!("validating '{validation}' for")
        } else {
            "parsing".to_owned()
        };

        write!(
            formatter,
            "error {failed_action} {field}`{config}` at `{path}`{origin}: {err}",
            err = self.inner,
            config = self.config.ty.name_in_code(),
            path = self.path
        )
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

impl ParseError {
    pub(crate) fn generic(path: String, config: &'static ConfigMetadata) -> Self {
        Self {
            inner: serde_json::Error::custom("unspecified error deserializing configuration"),
            category: ParseErrorCategory::Generic,
            path,
            origin: Arc::default(),
            config,
            location_in_config: None,
            validation: None,
        }
    }

    /// Returns the wrapped error.
    pub fn inner(&self) -> &serde_json::Error {
        &self.inner
    }

    #[doc(hidden)]
    pub fn category(&self) -> ParseErrorCategory {
        self.category
    }

    /// Returns an absolute path on which this error has occurred.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns an origin of the value deserialization of which failed.
    pub fn origin(&self) -> &ValueOrigin {
        &self.origin
    }

    /// Returns human-readable description of the failed validation, if the error is caused by one.
    pub fn validation(&self) -> Option<&str> {
        self.validation.as_deref()
    }

    /// Returns metadata for the failing config.
    pub fn config(&self) -> &'static ConfigMetadata {
        self.config
    }

    /// Returns metadata for the failing parameter if this error concerns a parameter. The parameter
    /// is guaranteed to be contained in [`Self::config()`].
    pub fn param(&self) -> Option<&'static ParamMetadata> {
        let LocationInConfig::Param(idx) = self.location_in_config?;
        self.config.params.get(idx)
    }
}

/// Collection of [`ParseError`]s returned from [`ConfigParser::parse()`](crate::ConfigParser::parse()).
#[derive(Debug, Default)]
pub struct ParseErrors {
    errors: Vec<ParseError>,
}

impl ParseErrors {
    pub(crate) fn push(&mut self, err: ParseError) {
        self.errors.push(err);
    }

    /// Iterates over the contained errors.
    pub fn iter(&self) -> impl Iterator<Item = &ParseError> + '_ {
        self.errors.iter()
    }

    /// Returns the number of contained errors.
    #[allow(clippy::len_without_is_empty)] // is_empty should always return false
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Returns a reference to the first error.
    #[allow(clippy::missing_panics_doc)] // false positive
    pub fn first(&self) -> &ParseError {
        self.errors.first().expect("no errors")
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.errors.truncate(len);
    }
}

impl IntoIterator for ParseErrors {
    type Item = ParseError;
    type IntoIter = std::vec::IntoIter<ParseError>;

    fn into_iter(self) -> Self::IntoIter {
        self.errors.into_iter()
    }
}

impl fmt::Display for ParseErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for err in &self.errors {
            writeln!(formatter, "{err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseErrors {}

impl FromIterator<ParseError> for Result<(), ParseErrors> {
    fn from_iter<I: IntoIterator<Item = ParseError>>(iter: I) -> Self {
        let errors: Vec<_> = iter.into_iter().collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ParseErrors { errors })
        }
    }
}
