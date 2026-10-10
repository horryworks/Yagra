// SPDX-License-Identifier: AGPL-3.0-only
//! The five sealed columns, read and bound in one place (ADR-184).
//!
//! Nine tables store an envelope-sealed secret ([`crate::secrets::SEALED_TABLES`]), each in the
//! same five columns: `key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce`. Eleven readers and
//! eleven writers used to spell them out by hand, and the hand-spelled copies are where the one
//! real failure came from: `key_id` is `INTEGER` in eight tables and `BIGINT` in `credentials`,
//! sqlx checks the width **at run time**, and three readers that asked for the wrong one failed
//! every row and degraded to an empty read — measured on a lab core 2026-08-30, when a deployment
//! with one notification channel delivered nothing at all.
//!
//! So the width is decided here, once, by asking for the wide type and falling back to the narrow
//! one, and every caller binds the wide type (PostgreSQL assigns a `BIGINT` parameter to an
//! `INTEGER` column; a `key_id` is a KEK generation and never approaches either limit).
//!
//! `yagra-secrets` stays free of sqlx: it knows how to seal, not where the result is stored.

use sqlx::postgres::{PgArguments, PgRow};
use sqlx::query::Query;
use sqlx::{Postgres, Row};
use yagra_secrets::SealedSecret;

/// `key_id` at whichever width its table declares.
fn key_id(row: &PgRow) -> Result<u32, sqlx::Error> {
    let wide = match row.try_get::<i64, _>("key_id") {
        Ok(v) => v,
        Err(_) => i64::from(row.try_get::<i32, _>("key_id")?),
    };
    // A generation outside `u32` is not one this build ever wrote; 0 names no KEK, so opening it
    // fails as a wrong key would rather than as a panic.
    Ok(u32::try_from(wide).unwrap_or(0))
}

/// The sealed secret a row holds. For a table whose five columns are `NOT NULL`.
///
/// # Errors
/// A column missing from the `SELECT`, or a NULL in one of them.
pub fn sealed_from_row(row: &PgRow) -> Result<SealedSecret, sqlx::Error> {
    Ok(SealedSecret {
        key_id: key_id(row)?,
        wrapped_dek: row.try_get("wrapped_dek")?,
        dek_nonce: row.try_get("dek_nonce")?,
        ciphertext: row.try_get("ciphertext")?,
        ct_nonce: row.try_get("ct_nonce")?,
    })
}

/// The sealed secret a row holds, or `None` when it holds none. For the two tables whose five
/// columns are all-or-none (`forward_destinations`, `llm_config`, each enforced by a CHECK): a NULL
/// `ciphertext` means "no secret", which is a valid configuration rather than a damaged row.
///
/// # Errors
/// As [`sealed_from_row`], for a row that has a ciphertext.
pub fn sealed_from_row_opt(row: &PgRow) -> Result<Option<SealedSecret>, sqlx::Error> {
    match row.try_get::<Option<Vec<u8>>, _>("ciphertext")? {
        None => Ok(None),
        Some(_) => sealed_from_row(row).map(Some),
    }
}

/// Binding the five columns, in the order they are declared:
/// `key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce`. The statement has to name them
/// consecutively, which every statement that stores one already does.
pub trait BindSealed<'q> {
    /// Bind a secret.
    fn bind_sealed(self, sealed: &'q SealedSecret) -> Self;
    /// Bind a secret, or five NULLs — "no secret" for an all-or-none table, or "keep what is
    /// stored" for a statement that coalesces them back.
    fn bind_sealed_opt(self, sealed: Option<&'q SealedSecret>) -> Self;
}

impl<'q> BindSealed<'q> for Query<'q, Postgres, PgArguments> {
    fn bind_sealed(self, sealed: &'q SealedSecret) -> Self {
        self.bind(i64::from(sealed.key_id))
            .bind(&sealed.wrapped_dek)
            .bind(&sealed.dek_nonce)
            .bind(&sealed.ciphertext)
            .bind(&sealed.ct_nonce)
    }

    fn bind_sealed_opt(self, sealed: Option<&'q SealedSecret>) -> Self {
        self.bind(sealed.map(|s| i64::from(s.key_id)))
            .bind(sealed.map(|s| &s.wrapped_dek))
            .bind(sealed.map(|s| &s.dek_nonce))
            .bind(sealed.map(|s| &s.ciphertext))
            .bind(sealed.map(|s| &s.ct_nonce))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The width rule is **derived, not written down**: `migrations/` says which width each
    /// `key_id` column has. This pins the premise [`key_id`]'s fallback rests on — exactly one
    /// wide column and the rest narrow — so a third width would fail here, not at run time.
    #[test]
    fn every_sealed_key_id_column_is_one_of_the_two_widths_read_here() {
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let (mut wide_cols, mut narrow_cols, mut other) = (0usize, 0usize, Vec::new());
        for entry in std::fs::read_dir(&migrations).expect("migrations/ is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().is_none_or(|x| x != "sql") {
                continue;
            }
            let sql = std::fs::read_to_string(&path)
                .expect("migration is readable")
                .to_lowercase();
            // A declaration, not a reference: the type word follows the column name.
            for tail in sql.split("key_id").skip(1) {
                let word: String = tail
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect();
                match word.as_str() {
                    "bigint" => wide_cols += 1,
                    "integer" => narrow_cols += 1,
                    "smallint" | "numeric" | "int" => other.push(path.display().to_string()),
                    _ => {}
                }
            }
        }
        assert!(
            other.is_empty(),
            "a key_id column of a third width in {other:?}; `key_id` reads only BIGINT and INTEGER"
        );
        assert_eq!(
            wide_cols, 1,
            "exactly one key_id column is BIGINT (credentials, 0002)"
        );
        assert!(
            narrow_cols >= 7,
            "only {narrow_cols} INTEGER key_id columns found — the migration scan stopped matching"
        );
    }

    /// Both widths, against a real database: an `INTEGER` table (`web_tls_config`) and the
    /// `BIGINT` one (`credentials`) each take a wide bind and give back the secret that went in.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sealed_secret_survives_both_key_id_widths(pool: sqlx::PgPool) {
        let sealed = SealedSecret {
            key_id: 7,
            wrapped_dek: vec![1, 2, 3],
            dek_nonce: vec![4, 5],
            ciphertext: vec![6, 7, 8, 9],
            ct_nonce: vec![10],
        };
        let same = |got: &SealedSecret| {
            got.key_id == sealed.key_id
                && got.wrapped_dek == sealed.wrapped_dek
                && got.dek_nonce == sealed.dek_nonce
                && got.ciphertext == sealed.ciphertext
                && got.ct_nonce == sealed.ct_nonce
        };

        sqlx::query(
            "INSERT INTO credentials (id, name, kind, key_id, wrapped_dek, dek_nonce, ciphertext, \
             ct_nonce) VALUES ($1, 'c', 'snmp_v2c', $2, $3, $4, $5, $6)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind_sealed(&sealed)
        .execute(&pool)
        .await
        .expect("a wide bind into the BIGINT table");
        let row = sqlx::query(
            "SELECT key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce FROM credentials",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(same(
            &sealed_from_row(&row).expect("the BIGINT column reads")
        ));

        sqlx::query(
            "INSERT INTO web_tls_config (id, source, certificate, key_id, wrapped_dek, dek_nonce, \
             ciphertext, ct_nonce, subject, issuer, sans, not_before, not_after, \
             fingerprint_sha256, key_algorithm) \
             VALUES (1, 'self_signed', 'pem', $1, $2, $3, $4, $5, 's', 'i', '[]'::jsonb, now(), \
             now(), 'f', 'ecdsa')",
        )
        .bind_sealed(&sealed)
        .execute(&pool)
        .await
        .expect("a wide bind into an INTEGER table");
        let row = sqlx::query(
            "SELECT key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce FROM web_tls_config",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(same(
            &sealed_from_row(&row).expect("the INTEGER column reads")
        ));
        assert!(same(
            &sealed_from_row_opt(&row)
                .unwrap()
                .expect("a row with a ciphertext holds a secret")
        ));
    }
}
