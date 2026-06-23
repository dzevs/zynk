//! zynk fork: sqlite-vec loading + vec0 table management (ADR 0006).
//
// Static load: register `sqlite3_vec_init` ONCE process-global via
// `sqlite3_auto_extension` (against the single bundled libsqlite3-sys sqlx links),
// so every subsequently-opened sqlx SQLite connection sees `vec0` with no per-conn
// boilerplate. Idempotent + thread-safe via `Once`.
//
// B4a lands this FFI foundation ahead of its caller: the embedding worker (B4b)
// calls `register_sqlite_vec()` at startup and `ensure_vec0_table` once the active
// model's dim is known. Nothing in the bin build calls these yet, so the
// module-level `dead_code` allow keeps the strict `-D warnings` gate green until
// B4b lands (mirrors `embed::mod`'s seam-ahead-of-callers pattern).

#![allow(dead_code)]

use std::sync::Once;

use sqlx::Executor;

use crate::zynk::db::DbError;

static REGISTER: Once = Once::new();

/// Register sqlite-vec's `vec0` module process-globally via `sqlite3_auto_extension`.
///
/// Idempotent (`Once`-guarded) and safe to call from any thread. Must be invoked
/// BEFORE opening the sqlx connection(s) that will use `vec0` — once registered,
/// EVERY subsequently-opened sqlx SQLite connection sees `vec0` with zero per-conn
/// setup (the static-auto-extension property ADR 0006 D1/D2 selected). The
/// registration is process-sticky and cannot be undone in-process.
///
/// Load-bearing invariant (guarded by `cargo_lock_pins_single_libsqlite3` below):
/// this works ONLY while sqlx-sqlite and our direct `libsqlite3-sys` feature-unify
/// onto EXACTLY ONE `libsqlite3-sys` node. A second version would register vec0 on
/// a different SQLite than sqlx links, silently yielding `no such module: vec0`.
pub fn register_sqlite_vec() {
    REGISTER.call_once(|| unsafe {
        // Cast the zero-arg `sqlite_vec::sqlite3_vec_init` fn item to a thin
        // `*const ()`, then transmute it to the entry-point fn-pointer type
        // `sqlite3_auto_extension` expects (the exact signature the bundled
        // libsqlite3-sys bindgen emits). This is the B0-proven, sqlite-vec-upstream
        // pattern, retargeted from rusqlite's ffi to the libsqlite3-sys sqlx links.
        // Annotations are explicit per `clippy::missing_transmute_annotations`.
        type AutoExtFn = unsafe extern "C" fn(
            *mut libsqlite3_sys::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const libsqlite3_sys::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), AutoExtFn>(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Reject anything that is not a safe SQL identifier (ASCII alnum + `_`, non-empty).
///
/// `vec_table` is internal/config-derived (never user input), but vec0 syntax needs
/// the table name as a SQL literal (not a bind param), so we validate defensively
/// before interpolating — a malformed/empty name returns a `DbError` rather than
/// emitting unsafe SQL.
fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Lazily create the `vec0` virtual table for `dim`-length cosine-distance vectors.
///
/// Runs `CREATE VIRTUAL TABLE IF NOT EXISTS {vec_table} USING vec0(message_rowid
/// INTEGER PRIMARY KEY, embedding float[{dim}] distance_metric=cosine)` on `conn`.
/// `conn` MUST be a connection opened AFTER [`register_sqlite_vec`] (so `vec0` is
/// visible). Idempotent via `IF NOT EXISTS`; per ADR 0006 D3 this is the runtime,
/// out-of-migrations creation path (a migration must not depend on extension-load
/// ordering).
///
/// `vec_table` and `dim` are formatted into the SQL because vec0 syntax requires
/// literals, not bind params. `vec_table` is validated as a safe identifier first.
pub async fn ensure_vec0_table(
    conn: &mut sqlx::SqliteConnection,
    vec_table: &str,
    dim: usize,
) -> Result<(), DbError> {
    if !is_safe_identifier(vec_table) {
        return Err(DbError::new(
            "vec_table_invalid",
            format!("unsafe vec0 table identifier: {vec_table:?}"),
        ));
    }
    let sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {vec_table} \
         USING vec0(message_rowid INTEGER PRIMARY KEY, \
         embedding float[{dim}] distance_metric=cosine)"
    );
    conn.execute(sql.as_str()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{Connection, Row, SqliteConnection};

    // ADR 0006 D1: the static `sqlite3_auto_extension` registration is correct ONLY
    // while EXACTLY ONE `libsqlite3-sys` node exists in the tree (sqlx-sqlite + our
    // direct dep feature-unify onto it). A second version would register vec0 on a
    // different SQLite than sqlx links → silent `no such module: vec0`. This is the
    // CI guard the ADR requires: read the committed lockfile and assert the count.
    #[test]
    fn cargo_lock_pins_single_libsqlite3() {
        let lock = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock"));
        let count = lock
            .lines()
            .filter(|line| line.trim() == r#"name = "libsqlite3-sys""#)
            .count();
        assert_eq!(
            count, 1,
            "expected exactly ONE libsqlite3-sys node in Cargo.lock (got {count}); a second \
             version silently breaks the static sqlite-vec auto-extension (ADR 0006 D1)"
        );
    }

    // POSITIVE proof that the static auto-extension makes vec0 visible on a FRESH
    // sqlx connection opened AFTER registration: load → CREATE vec0 → INSERT → KNN.
    //
    // NO in-process negative control: `sqlite3_auto_extension` is process-global +
    // sticky (cannot be unregistered in-process), and nextest may have run other
    // tests in this process that already registered it — so a "without registration
    // → no vec0" assertion is unreliable here. The B0 spike proved that negative
    // control in isolated processes (ADR 0006: registration removed ⇒ `no such
    // module: vec0`); we do not and cannot reproduce it in-process.
    #[test]
    fn static_auto_extension_exposes_vec0_on_fresh_conn() {
        register_sqlite_vec();

        let tmp = std::env::temp_dir().join(format!(
            "zynk-vec0-reg-test-{}-{}.db",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("test")
        ));

        let result = crate::zynk::db::block_on(async {
            // Mirror the db.rs test pattern: a fresh file-backed sqlx connection,
            // opened AFTER registration, with NO `.extension()` call.
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(&tmp)
                    .create_if_missing(true),
            )
            .await?;

            ensure_vec0_table(&mut conn, "test_vec_reg", 4).await?;

            // vec0 accepts the embedding as a JSON-array string literal.
            conn.execute(
                "INSERT INTO test_vec_reg(message_rowid, embedding) \
                 VALUES (1, '[0.1,0.2,0.3,0.4]')",
            )
            .await?;

            // KNN: nearest neighbour to the same vector must be rowid 1.
            let row = sqlx::query(
                "SELECT message_rowid, distance FROM test_vec_reg \
                 WHERE embedding MATCH '[0.1,0.2,0.3,0.4]' AND k = 1",
            )
            .fetch_one(&mut conn)
            .await?;
            let rowid: i64 = row.try_get("message_rowid")?;
            Ok::<i64, DbError>(rowid)
        });

        let _ = std::fs::remove_file(&tmp);

        let rowid = result.expect("load → create vec0 → insert → KNN must succeed");
        assert_eq!(
            rowid, 1,
            "KNN must return the single inserted vector's rowid"
        );
    }

    #[test]
    fn ensure_vec0_table_rejects_unsafe_identifier() {
        // Defensive: vec_table is interpolated (vec0 needs a literal), so a name
        // with non-identifier chars must be rejected as a DbError, not emitted.
        register_sqlite_vec();
        let result = crate::zynk::db::block_on(async {
            let mut conn = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await?;
            ensure_vec0_table(&mut conn, "bad name; DROP TABLE x", 4).await
        });
        match result {
            Err(e) => assert_eq!(e.code, "vec_table_invalid"),
            Ok(()) => panic!("unsafe identifier must be rejected"),
        }
    }
}
