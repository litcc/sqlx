#![cfg(any(feature = "_sqlite", feature = "postgres"))]

use sqlx::any::{AnyConnectOptions, AnyPoolOptions};
#[cfg(feature = "_sqlite")]
use sqlx::{ConnectOptions, Connection};
use std::str::FromStr;
use std::time::Duration;

#[cfg(feature = "_sqlite")]
fn sqlite_options(version: u32) -> sqlx::Result<AnyConnectOptions> {
    // This routing URL cannot be opened. Every connection must use the supplied options.
    AnyConnectOptions::from_str("sqlite:///missing-sqlx-driver-options/database.sqlite")?
        .with_driver_options(
            sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")?
                .pragma("user_version", version.to_string()),
        )
}

#[cfg(feature = "_sqlite")]
#[tokio::test]
async fn options_are_isolated_and_reused_for_expansion_and_replacement() -> anyhow::Result<()> {
    sqlx::any::install_default_drivers();
    let options = sqlite_options(41)?;
    let pool = AnyPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(options.clone())
        .await?;
    let other = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(sqlite_options(42)?)
        .await?;

    let mut first = pool.acquire().await?;
    let mut second = pool.acquire().await?;
    for connection in [&mut first, &mut second] {
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut **connection)
            .await?;
        assert_eq!(version, 41);
    }
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&other)
        .await?;
    assert_eq!(version, 42);

    sqlx::query("CREATE TEMP TABLE items (value TEXT)")
        .execute(&mut *first)
        .await?;
    let mut transaction = first.begin().await?;
    sqlx::query("INSERT INTO items VALUES (?)")
        .bind("rolled back")
        .execute(&mut *transaction)
        .await?;
    transaction.rollback().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&mut *first)
        .await?;
    assert_eq!(count, 0);

    first.close().await?;
    second.close().await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await?;
    assert_eq!(version, 41);
    pool.close().await;
    other.close().await;
    Ok(())
}

#[cfg(feature = "_sqlite")]
#[tokio::test]
async fn changing_the_routing_url_fails_without_exposing_it() -> anyhow::Result<()> {
    sqlx::any::install_default_drivers();
    let mut options = sqlite_options(41)?;
    options.database_url.set_path("private-routing-path");
    let error = options.connect().await.unwrap_err();
    assert!(matches!(error, sqlx::Error::Configuration(_)));
    assert!(!error.to_string().contains("private-routing-path"));
    Ok(())
}

#[cfg(feature = "_sqlite")]
#[test]
fn supplied_options_are_not_formatted_in_debug_output() -> anyhow::Result<()> {
    let options = AnyConnectOptions::from_str("sqlite::memory:")?.with_driver_options(
        sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")?
            .pragma("key", "private-driver-option"),
    )?;
    assert!(!format!("{options:?}").contains("private-driver-option"));
    Ok(())
}

#[cfg(all(feature = "postgres", feature = "_sqlite"))]
#[test]
fn incompatible_driver_options_are_rejected() -> anyhow::Result<()> {
    let error = AnyConnectOptions::from_str("sqlite::memory:")?
        .with_driver_options(
            sqlx::postgres::PgConnectOptions::new_without_pgpass().password("private-password"),
        )
        .unwrap_err();
    assert!(matches!(error, sqlx::Error::Configuration(_)));
    assert!(!error.to_string().contains("private-password"));
    Ok(())
}

#[cfg(feature = "postgres")]
#[test]
fn exported_url_uses_the_supplied_connection_parameters() -> anyhow::Result<()> {
    use sqlx::ConnectOptions;
    let options = AnyConnectOptions::from_str("postgres://routing-user@localhost/routing-db")?
        .with_driver_options(
            sqlx::postgres::PgConnectOptions::new_without_pgpass()
                .host("localhost")
                .username("actual-user")
                .database("actual-db"),
        )?;
    let url = options.to_url_lossy();
    assert_eq!(url.username(), "actual-user");
    assert_eq!(url.path(), "/actual-db");
    Ok(())
}

#[cfg(feature = "_sqlite")]
#[tokio::test]
async fn any_statement_logging_overrides_supplied_options() -> anyhow::Result<()> {
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = std::sync::Arc::clone(&logs);
    env_logger::Builder::new()
        .parse_filters("debug")
        .format(move |_, record| {
            captured.lock().unwrap().push(record.args().to_string());
            Ok(())
        })
        .try_init()?;
    sqlx::any::install_default_drivers();

    let options = AnyConnectOptions::from_str("sqlite::memory:")?
        .with_driver_options(
            sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")?
                .disable_statement_logging(),
        )?
        .log_statements("debug".parse()?);
    let mut connection = options.connect().await?;
    sqlx::query("SELECT 1 AS sqlx_any_logging_enabled")
        .execute(&mut connection)
        .await?;
    connection.close().await?;

    let options = AnyConnectOptions::from_str("sqlite::memory:")?
        .with_driver_options(sqlx::sqlite::SqliteConnectOptions::from_str(
            "sqlite::memory:",
        )?)?
        .disable_statement_logging();
    let mut connection = options.connect().await?;
    sqlx::query("SELECT 1 AS sqlx_any_logging_disabled")
        .execute(&mut connection)
        .await?;
    connection.close().await?;

    let logs = logs.lock().unwrap();
    assert!(logs
        .iter()
        .any(|message| message.contains("sqlx_any_logging_enabled")));
    assert!(!logs
        .iter()
        .any(|message| message.contains("sqlx_any_logging_disabled")));
    Ok(())
}

#[cfg(feature = "postgres")]
#[test]
#[ignore = "Requires a PostgreSQL server and DATABASE_URL containing its password; runs in an isolated child with a private PGPASSFILE"]
fn postgres_connections_reuse_the_resolved_pgpass_password() -> anyhow::Result<()> {
    const CHILD: &str = "SQLX_TEST_DRIVER_OPTIONS_CHILD";
    if let Ok(path) = std::env::var(CHILD) {
        let url = std::env::var("DATABASE_URL")?;
        let driver_options = sqlx::postgres::PgConnectOptions::from_str(&url)?;
        let options = AnyConnectOptions::from_str(&url)?.with_driver_options(driver_options)?;
        // Re-parsing this URL would now resolve the wrong password, including on the first connect.
        std::fs::write(path, "*:*:*:*:invalid-replacement-password\n")?;
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                // A fresh parse must fail; otherwise trust authentication or an explicit
                // password query parameter could make this regression test pass vacuously.
                let reparsed = sqlx::postgres::PgConnectOptions::from_str(&url)?;
                let error = sqlx::ConnectOptions::connect(&reparsed).await.unwrap_err();
                assert_eq!(
                    error
                        .as_database_error()
                        .and_then(|error| error.code())
                        .as_deref(),
                    Some("28P01")
                );
                sqlx::any::install_default_drivers();
                let pool = AnyPoolOptions::new()
                    .max_connections(2)
                    .acquire_timeout(Duration::from_secs(3))
                    .connect_with(options)
                    .await?;
                let mut first = pool.acquire().await?;
                let mut second = pool.acquire().await?;
                let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&mut *first)
                    .await?;
                let other_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&mut *second)
                    .await?;
                assert_ne!(pid, other_pid);
                let _ = sqlx::query("SELECT pg_terminate_backend(pg_backend_pid())")
                    .execute(&mut *first)
                    .await;
                drop(first.detach());
                second.close().await?;
                let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&pool)
                    .await?;
                assert_ne!(replacement_pid, pid);
                pool.close().await;
                Ok(())
            });
    }

    let mut url = sqlx_core::Url::parse(&std::env::var("DATABASE_URL")?)?;
    let password = sqlx_core::percent_encoding::percent_decode_str(
        url.password()
            .ok_or_else(|| anyhow::anyhow!("DATABASE_URL must contain a password"))?,
    )
    .decode_utf8()?
    .replace('\\', "\\\\")
    .replace(':', "\\:");
    url.set_password(None)
        .map_err(|_| anyhow::anyhow!("invalid PostgreSQL URL"))?;
    let mut passfile = tempfile::NamedTempFile::new()?;
    use std::io::Write;
    writeln!(passfile, "*:*:*:*:{password}")?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "postgres_connections_reuse_the_resolved_pgpass_password",
            "--ignored",
        ])
        .env(CHILD, passfile.path())
        .env("PGPASSFILE", passfile.path())
        .env_remove("PGPASSWORD")
        .env("DATABASE_URL", url.as_str())
        .status()?;
    assert!(
        status.success(),
        "isolated PostgreSQL regression test failed"
    );
    Ok(())
}
