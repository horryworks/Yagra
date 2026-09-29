// SPDX-License-Identifier: AGPL-3.0-only
//! `subnet_overlap_rules` / `subnet_overlap_acks` — what an operator has said about ranges two
//! sites both use (ADR-187), and the one inventory read the comparison needs.
//!
//! The overlaps themselves are never stored: `crate::subnet_overlaps` recomputes them from
//! `node_l3` on every read. Only the person's decisions live here.

use super::*;

/// One stored exclusion rule.
#[derive(Debug, Clone)]
pub struct StoredOverlapRule {
    pub id: Uuid,
    /// `network/length`, as PostgreSQL renders a `cidr`.
    pub range: Option<String>,
    pub port_text: Option<String>,
    pub reason: String,
    pub note: String,
    pub enabled: bool,
    pub builtin: bool,
}

/// What an exclusion rule says, as written by an operator.
#[derive(Debug, Clone)]
pub struct OverlapRuleInput {
    /// Already validated as a network by the caller.
    pub range: Option<String>,
    pub port_text: Option<String>,
    pub reason: &'static str,
    pub note: String,
    pub enabled: bool,
}

/// Why a rule write did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum OverlapRuleRefusal {
    NotFound,
    /// A built-in rule may only be switched on or off.
    Builtin,
}

/// One stored acknowledgement: the key, the sites (the nil uuid for the root) and the note.
pub type StoredOverlapAck = (String, Vec<Uuid>, String);

impl NodeRepo {
    /// Every exclusion rule, built-in first, then oldest first.
    pub async fn subnet_overlap_rules(&self) -> anyhow::Result<Vec<StoredOverlapRule>> {
        let rows = sqlx::query(
            "SELECT id, range_cidr::TEXT AS range_cidr, port_text, reason, note, enabled, builtin \
             FROM subnet_overlap_rules ORDER BY builtin DESC, created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(StoredOverlapRule {
                    id: r.try_get("id")?,
                    range: r.try_get("range_cidr")?,
                    port_text: r.try_get("port_text")?,
                    reason: r.try_get("reason")?,
                    note: r.try_get("note")?,
                    enabled: r.try_get("enabled")?,
                    builtin: r.try_get("builtin")?,
                })
            })
            .collect()
    }

    /// Add a rule and return its id.
    pub async fn insert_subnet_overlap_rule(
        &self,
        rule: &OverlapRuleInput,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO subnet_overlap_rules (id, range_cidr, port_text, reason, note, enabled) \
             VALUES ($1, $2::CIDR, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(&rule.range)
        .bind(&rule.port_text)
        .bind(rule.reason)
        .bind(&rule.note)
        .bind(rule.enabled)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Replace a rule. A built-in rule accepts only a change of `enabled`; anything else it is sent
    /// is refused rather than half-applied.
    pub async fn update_subnet_overlap_rule(
        &self,
        id: Uuid,
        rule: &OverlapRuleInput,
    ) -> anyhow::Result<Result<(), OverlapRuleRefusal>> {
        let existing = self
            .subnet_overlap_rules()
            .await?
            .into_iter()
            .find(|r| r.id == id);
        let Some(existing) = existing else {
            return Ok(Err(OverlapRuleRefusal::NotFound));
        };
        if existing.builtin {
            let same = existing.range == rule.range
                && existing.port_text == rule.port_text
                && existing.reason == rule.reason
                && existing.note == rule.note;
            if !same {
                return Ok(Err(OverlapRuleRefusal::Builtin));
            }
        }
        let done = sqlx::query(
            "UPDATE subnet_overlap_rules SET range_cidr = $2::CIDR, port_text = $3, reason = $4, \
                    note = $5, enabled = $6 WHERE id = $1",
        )
        .bind(id)
        .bind(&rule.range)
        .bind(&rule.port_text)
        .bind(rule.reason)
        .bind(&rule.note)
        .bind(rule.enabled)
        .execute(&self.pool)
        .await?;
        // Deleted between the read above and this write: nothing was saved, so say so.
        if done.rows_affected() == 0 {
            return Ok(Err(OverlapRuleRefusal::NotFound));
        }
        Ok(Ok(()))
    }

    /// Delete an operator's rule. A built-in one is refused.
    pub async fn delete_subnet_overlap_rule(
        &self,
        id: Uuid,
    ) -> anyhow::Result<Result<(), OverlapRuleRefusal>> {
        let builtin: Option<bool> =
            sqlx::query_scalar("SELECT builtin FROM subnet_overlap_rules WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        match builtin {
            None => Ok(Err(OverlapRuleRefusal::NotFound)),
            Some(true) => Ok(Err(OverlapRuleRefusal::Builtin)),
            Some(false) => {
                let done =
                    sqlx::query("DELETE FROM subnet_overlap_rules WHERE id = $1 AND NOT builtin")
                        .bind(id)
                        .execute(&self.pool)
                        .await?;
                // Deleted by someone else in between: this call removed nothing.
                if done.rows_affected() == 0 {
                    return Ok(Err(OverlapRuleRefusal::NotFound));
                }
                Ok(Ok(()))
            }
        }
    }

    /// Every acknowledgement.
    pub async fn subnet_overlap_acks(&self) -> anyhow::Result<Vec<StoredOverlapAck>> {
        let rows = sqlx::query("SELECT overlap_key, site_ids, note FROM subnet_overlap_acks")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("overlap_key")?,
                    r.try_get("site_ids")?,
                    r.try_get("note")?,
                ))
            })
            .collect()
    }

    /// Record (or replace) an acknowledgement.
    pub async fn set_subnet_overlap_ack(
        &self,
        key: &str,
        sites: &[Uuid],
        note: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO subnet_overlap_acks (overlap_key, site_ids, note) VALUES ($1, $2, $3) \
             ON CONFLICT (overlap_key) DO UPDATE \
             SET site_ids = EXCLUDED.site_ids, note = EXCLUDED.note, acked_at = now()",
        )
        .bind(key)
        .bind(sites)
        .bind(note)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove an acknowledgement. `false` when there was none.
    pub async fn delete_subnet_overlap_ack(&self, key: &str) -> anyhow::Result<bool> {
        let done = sqlx::query("DELETE FROM subnet_overlap_acks WHERE overlap_key = $1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Every node's id, name and folder — what the comparison files addresses under sites by.
    pub async fn node_folders(&self) -> anyhow::Result<Vec<(Uuid, String, Option<Uuid>)>> {
        let rows = sqlx::query("SELECT id, name, group_id FROM nodes")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("name")?, r.try_get("group_id")?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{OverlapRuleInput, OverlapRuleRefusal};
    use crate::pgtest;
    use sqlx::PgPool;

    fn wan(text: &str) -> OverlapRuleInput {
        OverlapRuleInput {
            range: None,
            port_text: Some(text.to_owned()),
            reason: "wan",
            note: String::new(),
            enabled: true,
        }
    }

    /// Rules and acknowledgements round-trip, the seeded CGNAT row reads back first and refuses
    /// everything but a switch, and a node's folder is what the comparison is told.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn rules_and_acks_round_trip_and_the_builtin_only_switches(pool: PgPool) {
        let repo = pgtest::repo(pool.clone());
        let rules = repo.subnet_overlap_rules().await.expect("rules");
        assert_eq!(rules.len(), 1, "the migration seeds CGNAT");
        let cgnat = &rules[0];
        assert!(cgnat.builtin);
        assert_eq!(cgnat.range.as_deref(), Some("100.64.0.0/10"));

        let id = repo
            .insert_subnet_overlap_rule(&wan("onu"))
            .await
            .expect("insert");
        let rules = repo.subnet_overlap_rules().await.expect("rules");
        assert_eq!(rules[1].id, id, "built-in first, then oldest");
        assert_eq!(rules[1].port_text.as_deref(), Some("onu"));

        let mut off = wan("onu");
        off.enabled = false;
        assert_eq!(
            repo.update_subnet_overlap_rule(id, &off)
                .await
                .expect("update"),
            Ok(())
        );
        assert!(!repo.subnet_overlap_rules().await.expect("rules")[1].enabled);

        let builtin_edit = wan("anything");
        assert_eq!(
            repo.update_subnet_overlap_rule(cgnat.id, &builtin_edit)
                .await
                .expect("update"),
            Err(OverlapRuleRefusal::Builtin)
        );
        let switch_off = OverlapRuleInput {
            range: cgnat.range.clone(),
            port_text: None,
            reason: "wan",
            note: cgnat.note.clone(),
            enabled: false,
        };
        assert_eq!(
            repo.update_subnet_overlap_rule(cgnat.id, &switch_off)
                .await
                .expect("update"),
            Ok(())
        );
        assert_eq!(
            repo.delete_subnet_overlap_rule(cgnat.id)
                .await
                .expect("delete"),
            Err(OverlapRuleRefusal::Builtin)
        );
        assert_eq!(
            repo.delete_subnet_overlap_rule(id).await.expect("delete"),
            Ok(())
        );
        assert_eq!(
            repo.delete_subnet_overlap_rule(id).await.expect("delete"),
            Err(OverlapRuleRefusal::NotFound)
        );

        let site = uuid::Uuid::new_v4();
        repo.set_subnet_overlap_ack("same:192.0.2.0/24", &[site, uuid::Uuid::nil()], "nat")
            .await
            .expect("ack");
        repo.set_subnet_overlap_ack("same:192.0.2.0/24", &[site], "nat inside")
            .await
            .expect("re-ack replaces");
        let acks = repo.subnet_overlap_acks().await.expect("acks");
        assert_eq!(
            acks,
            vec![(
                "same:192.0.2.0/24".to_owned(),
                vec![site],
                "nat inside".to_owned()
            )]
        );
        assert!(repo
            .delete_subnet_overlap_ack("same:192.0.2.0/24")
            .await
            .expect("del"));
        assert!(!repo
            .delete_subnet_overlap_ack("same:192.0.2.0/24")
            .await
            .expect("del"));

        let folder = pgtest::group(&pool, "site-a").await;
        let node = pgtest::node(&pool, "rt-01", 1, Some(folder)).await;
        let folders = repo.node_folders().await.expect("folders");
        assert!(folders.contains(&(node, "rt-01".to_owned(), Some(folder))));
    }
}
