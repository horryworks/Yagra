// SPDX-License-Identifier: AGPL-3.0-only
//! Persistence for AI-assisted RCA (ADR-029): the active provider's configuration, and the reports
//! generated from it. Migration `0053_rca.sql` carries the reasoning for the schema.
//!
//! **The secret never leaves this module in a readable shape.** It is sealed with the same envelope
//! cipher as monitoring credentials and the OIDC client secret (ADR-018) and is decrypted only into
//! the short-lived [`ActiveConfig`] the orchestrator hands to a provider constructor.
//! [`LlmConfigView`] — the only thing the API returns — carries `has_api_key: bool` and nothing more.
//!
//! **Absent config is the off state.** [`RcaRepo::active`] returns `None` when there is no row or
//! the row is disabled, and the orchestrator turns that into a 503. There is no environment flag to
//! forget to set and no code path that quietly picks a default vendor.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_secrets::{EnvelopeCipher, SealedSecret, StaticKeyProvider};

use super::answer::RcaAnswer;
use super::{ProviderConfig, ProviderKind, DEFAULT_MAX_OUTPUT_TOKENS};
use crate::secrets::load_key_provider;

/// Bounds on `max_output_tokens`, matching the CHECK in migration 0053. Below the floor a model
/// cannot finish a sentence; the ceiling is far above what an RCA needs and exists to stop a typo
/// from authorizing an expensive call.
const MIN_OUTPUT_TOKENS: u32 = 256;
const MAX_OUTPUT_TOKENS: u32 = 65_536;
/// Longest accepted model id / project / location. Generous; they are identifiers, not prose.
const MAX_IDENT_CHARS: usize = 200;

/// The active provider, credential decrypted. Lives in memory for the duration of one request and
/// is never serialized — deriving `Serialize` here is exactly the mistake this type exists to make
/// hard, so it deliberately does not.
pub struct ActiveConfig {
    pub kind: ProviderKind,
    pub provider: ProviderConfig,
    pub max_output_tokens: u32,
}

/// What `GET /api/v1/llm/config` returns. No secret, by construction.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LlmConfigView {
    pub provider: String,
    pub model: String,
    pub project: String,
    pub location: String,
    pub enabled: bool,
    pub max_output_tokens: u32,
    /// True once a credential has been stored, so the form can show "set" without revealing it.
    pub has_api_key: bool,
    /// Whether this provider sends incident context outside the operator's own cloud. Computed
    /// server-side so the warning the UI shows cannot drift from what the backend actually does.
    pub leaves_operator_boundary: bool,
    pub updated_at: String,
}

/// The `PUT /api/v1/llm/config` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct LlmConfigInput {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Write-only credential. Three-valued on purpose:
    /// * `None` — keep whatever is stored (so editing the model does not require re-entering a key);
    /// * `Some("")` — clear it, which is how an operator moves Vertex onto Workload Identity;
    /// * `Some(key)` — replace it.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// A stored report. `body` carries the answer and its evidence; `summary` is lifted out of it so a
/// list view does not have to fetch the whole thing.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RcaReport {
    pub id: Uuid,
    pub node_id: Uuid,
    pub check_id: Uuid,
    pub provider: String,
    pub model: String,
    pub summary: String,
    /// The answer and the evidence it was grounded in.
    //  Held as `Value` and *declared* as `ReportBody` — the deliberate split. The column stays
    //  loose so a row written by an older build still reads back (typing it would turn a missing
    //  field into an unreadable report, which is data loss by the upgrade policy), while the
    //  published contract describes the shape the writer always produces. Everything that writes
    //  this column serializes a real `ReportBody`, so the declaration is true by construction.
    //  Kept as a plain comment, not a doc comment: doc text on a documented field is published
    //  verbatim to API clients, and none of this is their business.
    #[schema(value_type = ReportBody)]
    pub body: serde_json::Value,
    pub generated_at: String,
    pub created_by: String,
    /// Whether this response came from the cache rather than a fresh call. Not a column — it
    /// describes *this* delivery of the report, not the report.
    #[serde(default)]
    pub cached: bool,
}

/// A report's contents: the answer, the evidence it was grounded in, and the language it was
/// requested in.
//  Storing both is the point. An explanation without its evidence is an assertion, and ADR-029's
//  display rule — never show a claim the reader cannot check — needs the timeline to still be there
//  when the modal is reopened tomorrow. Below the doc line: published to API clients.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReportBody {
    pub answer: RcaAnswer,
    /// Everything the model was told, exactly as it saw it.
    //  Same loose-storage / declared-shape split as `RcaReport::body` above.
    #[schema(value_type = super::context::IncidentContext)]
    pub evidence: serde_json::Value,
    /// Which language the answer was requested in.
    pub language: crate::rca::prompt::Language,
}

/// A row to insert.
pub struct NewReport<'a> {
    pub node_id: Uuid,
    pub check_id: Uuid,
    pub context_digest: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub summary: &'a str,
    pub body: serde_json::Value,
    pub created_by: &'a str,
}

/// PostgreSQL-backed store for the provider config and the generated reports.
pub struct RcaRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<StaticKeyProvider>,
}

impl RcaRepo {
    #[must_use]
    pub fn from_env(pool: PgPool) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(load_key_provider()),
        }
    }

    // ── Provider configuration ──────────────────────────────────────────────────────────────

    /// The config as the Settings page shows it. `None` when nothing has been configured — which is
    /// the default state of every installation.
    pub async fn view(&self) -> anyhow::Result<Option<LlmConfigView>> {
        let row = sqlx::query(
            "SELECT provider, model, project, location, enabled, max_output_tokens, \
             ciphertext IS NOT NULL AS has_key, updated_at FROM llm_config WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let provider: String = row.try_get("provider")?;
        let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;
        let tokens: i32 = row.try_get("max_output_tokens")?;
        Ok(Some(LlmConfigView {
            leaves_operator_boundary: ProviderKind::parse(&provider)
                // An unrecognised stored value is treated as leaving the boundary: the honest
                // default for "we are not sure where this goes" is to warn.
                .is_none_or(ProviderKind::leaves_operator_boundary),
            provider,
            model: row.try_get("model")?,
            project: row.try_get("project")?,
            location: row.try_get("location")?,
            enabled: row.try_get("enabled")?,
            max_output_tokens: u32::try_from(tokens).unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
            has_api_key: row.try_get("has_key")?,
            updated_at: updated_at.to_rfc3339(),
        }))
    }

    /// The active provider with its credential decrypted, or `None` when RCA is off.
    ///
    /// `enabled = false` is treated exactly like no row: an operator who unticks the box has turned
    /// the feature off, and a stored credential must not keep it half-working.
    pub async fn active(&self) -> anyhow::Result<Option<ActiveConfig>> {
        self.load(true).await
    }

    /// The configured provider **ignoring `enabled`** — for the Test button, whose whole purpose is
    /// validating a provider before turning it on (the same reasoning as
    /// `ForwardStore::get_open`).
    pub async fn configured(&self) -> anyhow::Result<Option<ActiveConfig>> {
        self.load(false).await
    }

    async fn load(&self, require_enabled: bool) -> anyhow::Result<Option<ActiveConfig>> {
        let row = sqlx::query(
            "SELECT provider, model, project, location, enabled, max_output_tokens, \
             key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce FROM llm_config WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        if require_enabled && !row.try_get::<bool, _>("enabled")? {
            return Ok(None);
        }
        let provider: String = row.try_get("provider")?;
        let kind = ProviderKind::parse(&provider)
            .ok_or_else(|| anyhow::anyhow!("stored provider '{provider}' is not recognised"))?;

        // The five sealed columns move together (enforced by a CHECK); a NULL ciphertext means
        // Vertex + Workload Identity, which is a valid configuration with no secret at all.
        let secret = match row.try_get::<Option<Vec<u8>>, _>("ciphertext")? {
            None => None,
            Some(ciphertext) => {
                let key_id: i32 = row.try_get("key_id")?;
                let sealed = SealedSecret {
                    key_id: u32::try_from(key_id).unwrap_or(0),
                    wrapped_dek: row.try_get("wrapped_dek")?,
                    dek_nonce: row.try_get("dek_nonce")?,
                    ciphertext,
                    ct_nonce: row.try_get("ct_nonce")?,
                };
                let bytes = self
                    .cipher
                    .open(&sealed)
                    .map_err(|e| anyhow::anyhow!("open the LLM credential: {e}"))?;
                Some(
                    String::from_utf8(bytes)
                        .map_err(|_| anyhow::anyhow!("the LLM credential is not valid UTF-8"))?,
                )
            }
        };
        let tokens: i32 = row.try_get("max_output_tokens")?;
        Ok(Some(ActiveConfig {
            kind,
            provider: ProviderConfig {
                model: row.try_get("model")?,
                project: row.try_get("project")?,
                location: row.try_get("location")?,
                secret,
            },
            max_output_tokens: u32::try_from(tokens).unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
        }))
    }

    /// Create or replace the single config row.
    ///
    /// # Errors
    /// Returns operator-facing text for a malformed input (surfaced as 400), or the database error.
    pub async fn save(&self, input: &LlmConfigInput) -> anyhow::Result<()> {
        let kind = validate(input).map_err(|e| anyhow::anyhow!(e))?;
        let tokens = i32::try_from(
            input
                .max_output_tokens
                .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
                .clamp(MIN_OUTPUT_TOKENS, MAX_OUTPUT_TOKENS),
        )
        .unwrap_or(8192);

        // Three-valued `api_key` (see [`LlmConfigInput`]): replace, clear, or leave alone. "Leave
        // alone" is expressed with COALESCE on the excluded row so the statement stays a single
        // upsert — a read-then-write would race two admins editing the same page.
        let sealed = match input.api_key.as_deref() {
            None => Keep::AsIs,
            Some(k) if k.trim().is_empty() => Keep::Clear,
            Some(k) => Keep::Replace(
                self.cipher
                    .seal(k.as_bytes())
                    .map_err(|e| anyhow::anyhow!("seal the LLM credential: {e}"))?,
            ),
        };

        let (key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce) = match &sealed {
            Keep::Replace(s) => (
                Some(i32::try_from(s.key_id).unwrap_or(0)),
                Some(s.wrapped_dek.clone()),
                Some(s.dek_nonce.clone()),
                Some(s.ciphertext.clone()),
                Some(s.ct_nonce.clone()),
            ),
            Keep::AsIs | Keep::Clear => (None, None, None, None, None),
        };
        // On conflict: `AsIs` keeps the stored columns, the other two write what we bound (a fresh
        // seal, or NULLs to clear).
        //
        // **`AsIs` is scoped to the same vendor** (`AND llm_config.provider = EXCLUDED.provider`).
        // "Keep the stored credential" exists so that editing the model does not mean re-typing a
        // key; carrying a Gemini key over to Claude is a different thing entirely, and it would
        // fail as an authentication error at the next incident rather than at save time. Switching
        // vendor without supplying a credential therefore clears it, and the form asks for one.
        let keep = matches!(sealed, Keep::AsIs);

        sqlx::query(SAVE_SQL)
            .bind(kind.as_str())
            .bind(input.model.trim())
            .bind(input.project.trim())
            .bind(input.location.trim())
            .bind(input.enabled)
            .bind(tokens)
            .bind(key_id)
            .bind(wrapped_dek)
            .bind(dek_nonce)
            .bind(ciphertext)
            .bind(ct_nonce)
            .bind(keep)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── Reports ─────────────────────────────────────────────────────────────────────────────

    /// The newest report for a context digest, if one exists. The TTL comparison is the caller's:
    /// the store answers "has this exact evidence been explained", not "recently enough".
    pub async fn latest_for_digest(&self, digest: &str) -> anyhow::Result<Option<RcaReport>> {
        let row = sqlx::query(
            "SELECT id, node_id, check_id, provider, model, summary, body, generated_at, created_by \
             FROM rca_reports WHERE context_digest = $1 ORDER BY generated_at DESC LIMIT 1",
        )
        .bind(digest)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_report).transpose()
    }

    /// Insert a generated report and return it as stored.
    pub async fn insert(&self, new: &NewReport<'_>) -> anyhow::Result<RcaReport> {
        let id = Uuid::new_v4();
        let row = sqlx::query(
            "INSERT INTO rca_reports \
             (id, node_id, check_id, context_digest, provider, model, summary, body, created_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             RETURNING id, node_id, check_id, provider, model, summary, body, generated_at, created_by",
        )
        .bind(id)
        .bind(new.node_id)
        .bind(new.check_id)
        .bind(new.context_digest)
        .bind(new.provider)
        .bind(new.model)
        .bind(new.summary)
        .bind(&new.body)
        .bind(new.created_by)
        .fetch_one(&self.pool)
        .await?;
        row_to_report(&row)
    }
}

/// The single-statement upsert behind [`RcaRepo::save`].
///
/// A named constant so the vendor-scoping in the `CASE` arms is assertable without a database —
/// an in-memory fake cannot catch a mistake in SQL semantics, and this particular clause is what
/// stops one vendor's credential from surviving a switch to another.
const SAVE_SQL: &str = "INSERT INTO llm_config \
     (id, provider, model, project, location, enabled, max_output_tokens, \
      key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, updated_at) \
     VALUES (1,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, now()) \
     ON CONFLICT (id) DO UPDATE SET \
       provider = EXCLUDED.provider, model = EXCLUDED.model, \
       project = EXCLUDED.project, location = EXCLUDED.location, \
       enabled = EXCLUDED.enabled, max_output_tokens = EXCLUDED.max_output_tokens, \
       key_id      = CASE WHEN $12 AND llm_config.provider = EXCLUDED.provider \
                          THEN llm_config.key_id      ELSE EXCLUDED.key_id      END, \
       wrapped_dek = CASE WHEN $12 AND llm_config.provider = EXCLUDED.provider \
                          THEN llm_config.wrapped_dek ELSE EXCLUDED.wrapped_dek END, \
       dek_nonce   = CASE WHEN $12 AND llm_config.provider = EXCLUDED.provider \
                          THEN llm_config.dek_nonce   ELSE EXCLUDED.dek_nonce   END, \
       ciphertext  = CASE WHEN $12 AND llm_config.provider = EXCLUDED.provider \
                          THEN llm_config.ciphertext  ELSE EXCLUDED.ciphertext  END, \
       ct_nonce    = CASE WHEN $12 AND llm_config.provider = EXCLUDED.provider \
                          THEN llm_config.ct_nonce    ELSE EXCLUDED.ct_nonce    END, \
       updated_at  = now()";

/// Which of the three `api_key` intents a save carries.
enum Keep {
    /// No `api_key` field: leave the stored columns untouched.
    AsIs,
    /// An empty `api_key`: NULL the columns (Vertex moving to Workload Identity).
    Clear,
    /// A fresh secret, already sealed.
    Replace(yagra_secrets::SealedSecret),
}

fn row_to_report(row: &sqlx::postgres::PgRow) -> anyhow::Result<RcaReport> {
    let generated_at: chrono::DateTime<chrono::Utc> = row.try_get("generated_at")?;
    Ok(RcaReport {
        id: row.try_get("id")?,
        node_id: row.try_get("node_id")?,
        check_id: row.try_get("check_id")?,
        provider: row.try_get("provider")?,
        model: row.try_get("model")?,
        summary: row.try_get("summary")?,
        body: row.try_get("body")?,
        generated_at: generated_at.to_rfc3339(),
        created_by: row.try_get("created_by")?,
        cached: false,
    })
}

/// Validate a config input at the API edge, returning the parsed provider.
///
/// Fails at save time rather than on the next incident: a mistyped project id should be a red box
/// on the settings form, not a 502 at 3am. Pure, so every rule below is a unit test.
///
/// # Errors
/// Operator-facing text describing the first problem found.
pub fn validate(input: &LlmConfigInput) -> Result<ProviderKind, String> {
    let kind = ProviderKind::parse(input.provider.trim())
        .ok_or_else(|| format!("unknown provider '{}'", input.provider.trim()))?;
    let model = input.model.trim();
    if model.is_empty() {
        return Err("a model id is required".to_owned());
    }
    if model.chars().count() > MAX_IDENT_CHARS {
        return Err("that model id is implausibly long".to_owned());
    }
    if let Some(t) = input.max_output_tokens {
        if !(MIN_OUTPUT_TOKENS..=MAX_OUTPUT_TOKENS).contains(&t) {
            return Err(format!(
                "max_output_tokens must be between {MIN_OUTPUT_TOKENS} and {MAX_OUTPUT_TOKENS}"
            ));
        }
    }
    match kind {
        ProviderKind::Vertex => {
            // Project and location are interpolated into the endpoint path, so they go through the
            // same strict segment check the adapter applies — rejected here, where the operator can
            // see why, rather than at call time.
            let project = input.project.trim();
            let location = input.location.trim();
            if project.is_empty() {
                return Err("Vertex AI needs the GCP project id".to_owned());
            }
            if location.is_empty() {
                return Err("Vertex AI needs a region (or 'global')".to_owned());
            }
            super::provider::validate_path_segment("project", project)?;
            super::provider::validate_path_segment("location", location)?;
            // A service-account JSON is optional (Workload Identity); a malformed one is not.
            if let Some(key) = input.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
                crate::gcp::validate_service_account(key)?;
            }
        }
        ProviderKind::Gemini | ProviderKind::Claude => {
            if input.project.trim().len() > MAX_IDENT_CHARS
                || input.location.trim().len() > MAX_IDENT_CHARS
            {
                return Err("project/location are implausibly long".to_owned());
            }
        }
    }
    Ok(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(provider: &str) -> LlmConfigInput {
        LlmConfigInput {
            provider: provider.to_owned(),
            model: "a-model".to_owned(),
            project: String::new(),
            location: String::new(),
            enabled: true,
            max_output_tokens: None,
            api_key: Some("k".to_owned()),
        }
    }

    #[test]
    fn an_unknown_provider_is_rejected_rather_than_defaulted() {
        // Silently falling back to another vendor would move the operator's data across a boundary
        // they did not choose — the one thing ADR-029 will not do.
        let err = validate(&input("openai")).unwrap_err();
        assert!(err.contains("openai"), "the message names the bad value");
    }

    #[test]
    fn the_key_providers_need_only_a_model() {
        assert_eq!(validate(&input("gemini")).unwrap(), ProviderKind::Gemini);
        assert_eq!(validate(&input("claude")).unwrap(), ProviderKind::Claude);
    }

    #[test]
    fn a_blank_model_is_rejected() {
        let mut i = input("claude");
        i.model = "   ".to_owned();
        assert!(validate(&i).is_err());
    }

    #[test]
    fn vertex_requires_its_identifiers() {
        let mut i = input("vertex");
        i.api_key = None;
        assert!(validate(&i).unwrap_err().contains("project"));
        i.project = "my-project".to_owned();
        assert!(validate(&i).unwrap_err().contains("region"));
        i.location = "us-central1".to_owned();
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn vertex_rejects_a_location_that_would_escape_the_url_path() {
        // The same check the adapter applies, moved to save time so the operator sees the reason.
        let mut i = input("vertex");
        i.api_key = None;
        i.project = "my-project".to_owned();
        for bad in ["../secrets", "us-central1/../x", "us central1", "a@b"] {
            i.location = bad.to_owned();
            assert!(validate(&i).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn vertex_rejects_a_malformed_service_account_before_it_is_stored() {
        let mut i = input("vertex");
        i.project = "my-project".to_owned();
        i.location = "global".to_owned();
        i.api_key = Some("{\"not\":\"a service account\"}".to_owned());
        assert!(validate(&i).is_err());
    }

    #[test]
    fn vertex_without_a_key_is_valid_because_that_selects_workload_identity() {
        let mut i = input("vertex");
        i.project = "my-project".to_owned();
        i.location = "asia-northeast1".to_owned();
        i.api_key = None;
        assert!(validate(&i).is_ok());
        i.api_key = Some(String::new());
        assert!(
            validate(&i).is_ok(),
            "an empty key clears, it does not fail"
        );
    }

    #[test]
    fn the_output_budget_is_range_checked() {
        let mut i = input("claude");
        i.max_output_tokens = Some(10);
        assert!(validate(&i).is_err());
        i.max_output_tokens = Some(1_000_000);
        assert!(validate(&i).is_err());
        i.max_output_tokens = Some(4096);
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn keeping_the_stored_credential_is_scoped_to_the_same_vendor() {
        // Switching provider with no `api_key` must not carry the old vendor's key across: it
        // cannot work there, and the operator would meet it as a 502 mid-incident instead of as an
        // empty field on the form. Asserted against the SQL text because the guard lives in the
        // statement — there is no fake that would notice it going missing.
        for col in [
            "key_id",
            "wrapped_dek",
            "dek_nonce",
            "ciphertext",
            "ct_nonce",
        ] {
            let arm = format!(
                "{col:<11} = CASE WHEN $12 AND llm_config.provider = EXCLUDED.provider \
                 THEN llm_config.{col}"
            );
            let normalized = SAVE_SQL.split_whitespace().collect::<Vec<_>>().join(" ");
            let wanted = arm.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.contains(&wanted),
                "{col} must only be kept when the provider is unchanged"
            );
        }
    }

    #[test]
    fn the_view_never_carries_a_place_to_put_the_secret() {
        // A structural assertion: serializing a view must not produce any key that could hold one.
        let v = LlmConfigView {
            provider: "claude".to_owned(),
            model: "m".to_owned(),
            project: String::new(),
            location: String::new(),
            enabled: true,
            max_output_tokens: 8192,
            has_api_key: true,
            leaves_operator_boundary: true,
            updated_at: "now".to_owned(),
        };
        let json = serde_json::to_value(&v).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("has_api_key"));
        for forbidden in ["api_key", "secret", "ciphertext", "key_id", "wrapped_dek"] {
            assert!(
                !obj.contains_key(forbidden),
                "{forbidden} must not be returned"
            );
        }
    }
}
