//! Fallback [`Value`] sources.
//!
//! # Motivation and use cases
//!
//! Some configuration params may be sourced from places that do not fit well into the hierarchical config schema.
//! For example, a config param with logging directives may want to read from a `RUST_LOG` env var, regardless of where
//! the param is placed in the hierarchy. It is possible to manually move raw config values around, it may get unmaintainable
//! for large configs.
//!
//! *Fallbacks* provide a more sound approach: declare the fallback config sources as a part of the [`DescribeConfig`](macro@crate::DescribeConfig)
//! derive macro. In this way, fallbacks are documented (being a part of the config metadata)
//! and do not require splitting logic between config declaration and preparing config sources.
//!
//! Fallbacks should be used sparingly, since they make it more difficult to reason about configs due to their non-local nature.
//!
//! # Features and limitations
//!
//! - By design, fallbacks are location-independent. E.g., an [`Env`] fallback will always read from the same env var,
//!   regardless of where the param containing it is placed (including the case when it has multiple copies!).
//! - Fallbacks always have lower priority than all other config sources.

use std::{collections::HashMap, env, fmt, sync::Arc};

use crate::{
    source::Hierarchical,
    testing::MOCK_ENV_VARS,
    value::{Map, Pointer, Value, ValueOrigin, WithOrigin},
    ConfigSchema, ConfigSource,
};

/// Fallback source of a configuration param.
pub trait FallbackSource: 'static + Send + Sync + fmt::Debug + fmt::Display {
    /// Potentially provides a value for the param.
    ///
    /// Implementations should return `None` (vs `Some(Value::Null)` etc.) if the source doesn't have a value.
    fn provide_value(&self) -> Option<WithOrigin>;
}

/// Gets a string value from the specified env variable.
///
/// # Examples
///
/// ```
/// use smart_config::{fallback, testing, DescribeConfig, DeserializeConfig};
///
/// #[derive(DescribeConfig, DeserializeConfig)]
/// struct TestConfig {
///     /// Log directives. Always read from `RUST_LOG` env var in addition to
///     /// the conventional sources.
///     #[config(default_t = "info".into(), fallback = &fallback::Env("RUST_LOG"))]
///     log_directives: String,
/// }
///
/// let mut tester = testing::Tester::default();
/// let config: TestConfig = tester.test(smart_config::config!())?;
/// // Without env var set or other sources, the param will assume the default value.
/// assert_eq!(config.log_directives, "info");
///
/// tester.set_env("RUST_LOG", "warn");
/// let config: TestConfig = tester.test(smart_config::config!())?;
/// assert_eq!(config.log_directives, "warn");
///
/// // Mock env vars are still set here, but fallbacks have lower priority
/// // than other sources.
/// let input = smart_config::config!("log_directives": "info,my_crate=debug");
/// let config = tester.test(input)?;
/// assert_eq!(config.log_directives, "info,my_crate=debug");
/// # anyhow::Ok(())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Env(pub &'static str);

impl fmt::Display for Env {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "env var {:?}", self.0)
    }
}

impl Env {
    /// Gets the raw string value of the env var, taking [mock vars] into account.
    ///
    /// [mock vars]: crate::testing::Tester::set_env()
    pub fn get_raw(&self) -> Option<String> {
        MOCK_ENV_VARS
            .with(|cell| cell.borrow().get(self.0).cloned())
            .or_else(|| env::var(self.0).ok())
    }
}

impl FallbackSource for Env {
    fn provide_value(&self) -> Option<WithOrigin> {
        if let Some(value) = self.get_raw() {
            let origin = ValueOrigin::Path {
                source: Arc::new(ValueOrigin::EnvVars),
                path: self.0.into(),
            };
            Some(WithOrigin::new(value.into(), Arc::new(origin)))
        } else {
            None
        }
    }
}

/// Custom [fallback value provider](FallbackSource).
///
/// # Use cases
///
/// This provider is useful when configuration parameter deserialization logic is hard to express
/// using conventional methods, such as:
///
/// - Composite configuration parameters that rely on several environment variables.
///   In this case, you can use the getter closure to access variables via [`Env`] and combine
///   them into a single value.
/// - Configuration values that need additional validation to make the configuration object
///   correct by construction. For example, if your config has an optional field, which should
///   be `None` _either_ if it's absent or set to the `"unset"` value, you can first get it via `Env`,
///   and then only provide value if it's not equal to `"unset"`. You can think of it as a `filter` or
///   `map` function in this case.
///
/// # Examples
///
/// ```
/// # use std::sync::Arc;
/// use smart_config::{
///     fallback, testing, value::{ValueOrigin, WithOrigin},
///     DescribeConfig, DeserializeConfig,
/// };
///
/// // Value source combining two env variables. It usually makes sense to split off
/// // the definition like this so that it's more readable.
/// const COMBINED_VARS: &'static dyn fallback::FallbackSource =
///     &fallback::Manual::new("$TEST_ENV - $TEST_NETWORK", || {
///         let env = fallback::Env("TEST_ENV").get_raw()?;
///         let network = fallback::Env("TEST_NETWORK").get_raw()?;
///         let origin = Arc::new(ValueOrigin::EnvVars);
///         Some(WithOrigin::new(format!("{env} - {network}").into(), origin))
///     });
///
/// #[derive(DescribeConfig, DeserializeConfig)]
/// struct TestConfig {
///     #[config(default_t = "app".into(), fallback = COMBINED_VARS)]
///     app: String,
/// }
///
/// let config: TestConfig = testing::Tester::default()
///     .set_env("TEST_ENV", "stage")
///     .set_env("TEST_NETWORK", "goerli")
///     .test(smart_config::config!())?;
/// assert_eq!(config.app, "stage - goerli");
/// # anyhow::Ok(())
/// ```
#[derive(Debug)]
pub struct Manual {
    description: &'static str,
    getter: fn() -> Option<WithOrigin>,
}

impl Manual {
    /// Creates a provider with the specified human-readable description and a getter function.
    pub const fn new(description: &'static str, getter: fn() -> Option<WithOrigin>) -> Self {
        Self {
            description,
            getter,
        }
    }
}

impl fmt::Display for Manual {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.description)
    }
}

impl FallbackSource for Manual {
    fn provide_value(&self) -> Option<WithOrigin> {
        (self.getter)()
    }
}

#[derive(Debug)]
pub(crate) struct Fallbacks {
    inner: HashMap<(String, &'static str), WithOrigin>,
    origin: Arc<ValueOrigin>,
}

impl Fallbacks {
    #[tracing::instrument(level = "debug", name = "Fallbacks::new", skip_all)]
    pub(crate) fn new(schema: &ConfigSchema) -> Option<Self> {
        let mut inner = HashMap::new();
        for (prefix, config) in schema.iter_ll() {
            for param in config.metadata.params {
                let Some(fallback) = param.fallback else {
                    continue;
                };
                if let Some(mut val) = fallback.provide_value() {
                    tracing::trace!(
                        prefix = prefix.0,
                        config = ?config.metadata.ty,
                        param = param.rust_field_name,
                        provider = ?fallback,
                        "got fallback for param"
                    );

                    let origin = ValueOrigin::Synthetic {
                        source: val.origin.clone(),
                        transform: format!(
                            "fallback for `{}.{}`",
                            config.metadata.ty.name_in_code(),
                            param.rust_field_name,
                        ),
                    };
                    val.origin = Arc::new(origin);
                    inner.insert((prefix.0.to_owned(), param.name), val);
                }
            }
        }

        if inner.is_empty() {
            None
        } else {
            tracing::debug!(count = inner.len(), "got fallbacks for config params");
            Some(Self {
                inner,
                origin: Arc::new(ValueOrigin::Fallbacks),
            })
        }
    }
}

impl ConfigSource for Fallbacks {
    type Kind = Hierarchical;

    fn into_contents(self) -> WithOrigin<Map> {
        let origin = self.origin;
        let mut map = WithOrigin::new(Value::Object(Map::new()), origin.clone());
        for ((prefix, name), value) in self.inner {
            map.ensure_object(Pointer(&prefix), |_| origin.clone())
                .insert(name.to_owned(), value);
        }

        map.map(|value| match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        })
    }
}
