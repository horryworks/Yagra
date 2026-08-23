// SPDX-License-Identifier: AGPL-3.0-only
//! Operator decisions about links (ADR-043 決定 4, migration 0068).
//!
//! Unlike [`crate::topology_links`] — a cache that is rebuilt from observations and pruned by age —
//! nothing here is recomputable. These rows are the operator's statements about what the derivation
//! got wrong, and they must outlive any number of derivation cycles in which the link in question
//! was not observed.
//!
//! **This store does not apply anything.** It reads and writes rows; the one place a decision is
//! *applied* is `yagra_topology::derive::apply_overrides`, which is what keeps "manual always wins"
//! a property of a single function rather than a convention two subsystems each have to remember.
//!
//! The one rule with teeth is canonical ordering: a link is an unordered pair, so `(a,b)` and
//! `(b,a)` are the same decision, and the writer normalizes before insert. Skip it and the UNIQUE
//! index means nothing — the same hide stores twice and removing one appears not to work.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{LinkDirection, LinkOverride, LinkOverrideAction, NodeId};

/// One stored override, as read back for the API.
#[derive(Debug, Clone)]
pub struct StoredOverride {
    pub id: Uuid,
    /// The lower-ordered endpoint (canonical).
    pub a_node: NodeId,
    /// The higher-ordered endpoint.
    pub b_node: NodeId,
    pub action: LinkOverrideAction,
    /// Which endpoint is upstream — present only for [`LinkOverrideAction::Direction`].
    pub direction: Option<LinkDirection>,
    pub note: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// PostgreSQL-backed store of link overrides.
pub struct LinkOverrideRepo {
    pool: PgPool,
}

impl LinkOverrideRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Put the endpoints in the order the UNIQUE index assumes.
    ///
    /// Free-standing rather than inlined at each call site: every read, write and delete has to
    /// agree, and a delete that skipped it would silently fail to remove the row it was aimed at.
    #[must_use]
    pub fn canonical(a: NodeId, b: NodeId) -> (NodeId, NodeId) {
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Put the endpoints in canonical order and re-express the direction against that order.
    ///
    /// The direction is resolved to a concrete endpoint **before** the columns move. Re-expressing
    /// it afterwards would silently invert "a is upstream" for every caller who named the pair the
    /// other way round — and a wrong parent is the one mistake in this feature that suppresses a
    /// real outage, so the inversion has to be impossible rather than unlikely.
    #[must_use]
    pub fn canonical_with_direction(
        a: NodeId,
        b: NodeId,
        direction: Option<LinkDirection>,
    ) -> (NodeId, NodeId, Option<LinkDirection>) {
        let parent = direction.map(|d| match d {
            LinkDirection::AParent => a,
            LinkDirection::BParent => b,
        });
        let (lo, hi) = Self::canonical(a, b);
        let direction = parent.map(|p| {
            if p == lo {
                LinkDirection::AParent
            } else {
                LinkDirection::BParent
            }
        });
        (lo, hi, direction)
    }

    /// Every override, for the derivation. Rows whose `action` or `direction` token is unknown —
    /// written by a newer core — are **skipped**, so an older core degrades to "no override" for
    /// that pair rather than failing the whole read and losing every operator decision at once.
    pub async fn all(&self) -> anyhow::Result<Vec<LinkOverride>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .map(|o| LinkOverride {
                a_node: o.a_node,
                b_node: o.b_node,
                action: o.action,
                direction: o.direction,
            })
            .collect())
    }

    /// Every override with its metadata, ordered so the list does not shuffle between reads.
    pub async fn list(&self) -> anyhow::Result<Vec<StoredOverride>> {
        let rows = sqlx::query(
            "SELECT id, a_node, b_node, action, direction, note, created_by, created_at \
             FROM link_overrides ORDER BY created_at DESC, id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let token: String = row.try_get("action")?;
            let Some(action) = LinkOverrideAction::from_token(&token) else {
                continue;
            };
            let direction = row
                .try_get::<Option<String>, _>("direction")?
                .and_then(|d| LinkDirection::from_token(&d));
            // A direction row whose token did not parse would otherwise read as "direction, but no
            // direction", which `forced_parent()` answers with `None` — the same as no row at all,
            // so skipping it changes nothing and states the intent.
            if action == LinkOverrideAction::Direction && direction.is_none() {
                continue;
            }
            out.push(StoredOverride {
                id: row.try_get("id")?,
                a_node: NodeId(row.try_get("a_node")?),
                b_node: NodeId(row.try_get("b_node")?),
                action,
                direction,
                note: row.try_get("note")?,
                created_by: row.try_get("created_by")?,
                created_at: row.try_get("created_at")?,
            });
        }
        Ok(out)
    }

    /// Record a decision, replacing any previous one of the same kind for the same pair.
    ///
    /// Upsert rather than insert: an operator re-declaring the direction of a link means the new
    /// answer, not a constraint violation they have to resolve by deleting the old row first.
    pub async fn upsert(
        &self,
        a: NodeId,
        b: NodeId,
        action: LinkOverrideAction,
        direction: Option<LinkDirection>,
        note: Option<&str>,
        created_by: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let (a, b, direction) = Self::canonical_with_direction(a, b, direction);
        let row = sqlx::query(
            "INSERT INTO link_overrides (a_node, b_node, action, direction, note, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (a_node, b_node, action) DO UPDATE SET \
                 direction = EXCLUDED.direction, \
                 note = EXCLUDED.note, \
                 created_by = EXCLUDED.created_by, \
                 created_at = now() \
             RETURNING id",
        )
        .bind(a.as_uuid())
        .bind(b.as_uuid())
        .bind(action.as_str())
        .bind(direction.map(LinkDirection::as_str))
        .bind(note)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    /// Remove one decision by id. Returns whether a row went.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM link_overrides WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The endpoints of one override, for the scope check the API has to make before deleting it.
    pub async fn endpoints(&self, id: Uuid) -> anyhow::Result<Option<(NodeId, NodeId)>> {
        let row = sqlx::query("SELECT a_node, b_node FROM link_overrides WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            None => Ok(None),
            Some(r) => Ok(Some((
                NodeId(r.try_get("a_node")?),
                NodeId(r.try_get("b_node")?),
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "link_overrides")
    }

    #[test]
    fn canonical_ordering_is_idempotent_and_order_independent() {
        // The UNIQUE index is only a constraint if both orderings land on the same row.
        let x = NodeId::new();
        let y = NodeId::new();
        assert_eq!(
            LinkOverrideRepo::canonical(x, y),
            LinkOverrideRepo::canonical(y, x)
        );
        let once = LinkOverrideRepo::canonical(x, y);
        assert_eq!(LinkOverrideRepo::canonical(once.0, once.1), once);
        assert!(once.0 <= once.1);
    }

    #[test]
    fn a_direction_survives_canonicalization_whichever_order_it_was_submitted_in() {
        // The failure this prevents: an operator names (child, parent), the writer sorts the
        // endpoints, and the stored direction now points at the child. Nothing downstream can
        // detect that — it is a valid row asserting the opposite of what was meant, and it
        // suppresses the parent's alert when the child goes down.
        let x = NodeId::new();
        let y = NodeId::new();
        let (lo, hi) = LinkOverrideRepo::canonical(x, y);

        // "the first one I named is upstream", submitted both ways round.
        let (a1, b1, d1) =
            LinkOverrideRepo::canonical_with_direction(lo, hi, Some(LinkDirection::AParent));
        assert_eq!((a1, b1), (lo, hi));
        assert_eq!(d1, Some(LinkDirection::AParent));

        let (a2, b2, d2) =
            LinkOverrideRepo::canonical_with_direction(hi, lo, Some(LinkDirection::AParent));
        assert_eq!((a2, b2), (lo, hi));
        assert_eq!(
            d2,
            Some(LinkDirection::BParent),
            "the operator named `hi` as upstream; after sorting that is the `b` column"
        );

        // And with no direction there is nothing to move.
        assert_eq!(
            LinkOverrideRepo::canonical_with_direction(hi, lo, None),
            (lo, hi, None)
        );
    }

    #[test]
    fn a_repeat_decision_replaces_rather_than_failing() {
        // Without the upsert, re-declaring a direction is a 500 from a UNIQUE violation, and the
        // operator's only route to the new answer is deleting the old row first.
        let src = production_source();
        assert!(src.contains("ON CONFLICT (a_node, b_node, action) DO UPDATE SET"));
        assert!(src.contains("direction = EXCLUDED.direction"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder})"
            );
        }
    }

    #[test]
    fn the_list_is_ordered_so_it_does_not_shuffle_between_reads() {
        assert!(production_source().contains("ORDER BY created_at DESC, id"));
    }
}
