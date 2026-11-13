use crate::any::AnyConnection;
use crate::connection::{ConnectOptions, Connection, LogSettings};
use crate::database::Database;
use crate::error::Error;
use log::LevelFilter;
use std::any::Any;
use std::fmt;
use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Opaque options for connecting to a database. These may only be constructed by parsing from
/// a connection url.
///
/// ```text
/// postgres://postgres:password@localhost/database
/// mysql://root:password@localhost/database
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AnyConnectOptions {
    pub database_url: Url,
    pub log_settings: LogSettings,
    driver_options: Option<DriverOptions>,
}

#[derive(Clone)]
struct DriverOptions {
    routing_url: Url,
    options: Arc<dyn AnyDriverOptions>,
}

trait AnyDriverOptions: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn to_url_lossy(&self) -> Url;
}

impl<O: ConnectOptions> AnyDriverOptions for O {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_url_lossy(&self) -> Url {
        ConnectOptions::to_url_lossy(self)
    }
}
impl fmt::Debug for DriverOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Driver options can contain passwords, certificates and private keys.
        f.debug_struct("DriverOptions").finish_non_exhaustive()
    }
}
impl FromStr for AnyConnectOptions {
    type Err = Error;

    fn from_str(url: &str) -> Result<Self, Self::Err> {
        Ok(AnyConnectOptions {
            database_url: url
                .parse::<Url>()
                .map_err(|e| Error::Configuration(e.into()))?,
            log_settings: LogSettings::default(),
            driver_options: None,
        })
    }
}

impl ConnectOptions for AnyConnectOptions {
    type Connection = AnyConnection;

    fn from_url(url: &Url) -> Result<Self, Error> {
        Ok(AnyConnectOptions {
            database_url: url.clone(),
            log_settings: LogSettings::default(),
            driver_options: None,
        })
    }

    fn to_url_lossy(&self) -> Url {
        match &self.driver_options {
            Some(prepared) => prepared.options.to_url_lossy(),
            None => self.database_url.clone(),
        }
    }

    #[inline]
    fn connect(&self) -> impl Future<Output = Result<AnyConnection, Error>> + Send + '_ {
        AnyConnection::connect(self)
    }

    fn log_statements(mut self, level: LevelFilter) -> Self {
        self.log_settings.statements_level = level;
        self
    }

    fn log_slow_statements(mut self, level: LevelFilter, duration: Duration) -> Self {
        self.log_settings.slow_statements_level = level;
        self.log_settings.slow_statements_duration = duration;
        self
    }
}

impl AnyConnectOptions {
    /// Use already-resolved driver options for every new connection.
    ///
    /// This avoids parsing the URL again when the pool expands or replaces connections.
    /// For example, PostgreSQL's environment and passfile defaults are resolved when the
    /// supplied options are constructed, rather than on each connection attempt.
    /// External resources referenced by those options retain the driver's usual semantics.
    ///
    /// The URL is retained for selecting the registered driver; connection parameters come
    /// from `options`. Its scheme must belong to that driver's database. Mutating
    /// [`Self::database_url`] afterwards causes new connection attempts to fail. The selected
    /// driver must also use the supplied options type; a mismatch never falls back to parsing.
    /// Statement logging is controlled by [`Self::log_settings`], overriding the supplied
    /// options' statement log settings.
    /// [`ConnectOptions::to_url_lossy`] delegates to the supplied options. A resulting URL
    /// does not retain this snapshot or settings that cannot be represented in a URL.
    ///
    /// Clones share the supplied options, and each connection receives a driver-defined
    /// clone. No process-global options registry or environment mutation is used.
    pub fn with_driver_options<O: ConnectOptions>(mut self, options: O) -> Result<Self, Error>
    where
        O::Connection: Sized,
    {
        if !<O::Connection as Connection>::Database::URL_SCHEMES
            .contains(&self.database_url.scheme())
        {
            return Err(Error::Configuration(
                "supplied driver options do not match the URL scheme".into(),
            ));
        }

        self.driver_options = Some(DriverOptions {
            routing_url: self.database_url.clone(),
            options: Arc::new(options),
        });
        Ok(self)
    }

    pub(crate) fn driver_options<O: ConnectOptions>(&self) -> Result<Option<O>, Error> {
        let Some(prepared) = &self.driver_options else {
            return Ok(None);
        };
        if prepared.routing_url != self.database_url {
            return Err(Error::Configuration(
                "the routing URL changed after driver options were supplied".into(),
            ));
        }
        let options = prepared
            .options
            .as_ref()
            .as_any()
            .downcast_ref::<O>()
            .ok_or_else(|| {
                Error::Configuration(
                    "supplied driver options do not match the registered driver".into(),
                )
            })?;

        Ok(Some(
            options
                .clone()
                .log_statements(self.log_settings.statements_level)
                .log_slow_statements(
                    self.log_settings.slow_statements_level,
                    self.log_settings.slow_statements_duration,
                ),
        ))
    }
}
