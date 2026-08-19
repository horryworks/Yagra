// SPDX-License-Identifier: AGPL-3.0-only
//! Rendering an operator's notification template (ADR-039).
//!
//! A channel may override the subject and/or the body that Yagra sends. The override is a
//! minijinja template evaluated against [`AlertFacts`] — the flat, declared variable set in
//! `yagra-common` — and nothing else.
//!
//! **The one rule that outranks the feature: a broken template must never eat a notification.**
//! Alerting is the last line, and a typo in a template silently swallowing an outage page is worse
//! than having no template feature at all. So [`render_with_fallback`] never fails: whatever goes
//! wrong, the caller gets the built-in text it passed in and a [`TemplateFailure`] to count. That
//! is also why the built-in form is a plain `format!` in `alerts.rs` and *not* itself a template —
//! a fallback that runs through the same engine is not a fallback.
//!
//! **Two guards, catching different things.** [`validate`] rejects a template that does not compile,
//! at save time, with a typed 400 — the operator is standing right there and can fix it. The
//! runtime fallback catches what save time cannot: a template stored by an older core, a filter
//! that errors only on certain data, an output that only gets too large for some alerts, and a JSON
//! body that only becomes invalid when a node name contains a quote.
//!
//! **The engine is fenced by its feature list, not by trust.** No loader is registered and
//! `loader`/`multi_template` are compiled out, so `{% include %}` does not exist; `fuel` bounds
//! execution because the render is a synchronous call on the notify worker where a wall-clock
//! timeout is not expressible. See the `minijinja` entry in the workspace `Cargo.toml`.

use minijinja::{Environment, UndefinedBehavior};
use yagra_common::AlertFacts;

use crate::notifications::ChannelKind;

/// Longest rendered subject accepted, in characters. A subject is a headline; past this it is not
/// one, and every downstream channel truncates it anyway.
pub(crate) const MAX_SUBJECT_CHARS: usize = 512;
/// Longest rendered body accepted, in characters. Comfortably above any hand-written notification
/// and far below anything that would strain a vendor endpoint.
pub(crate) const MAX_BODY_CHARS: usize = 64_000;
/// Execution budget for one render. Legitimate subject/body templates use a few hundred steps; a
/// runaway loop exhausts this in milliseconds and falls back.
const RENDER_FUEL: u64 = 50_000;

/// A channel's optional overrides. `None` on a field means "use the built-in form for it".
///
/// The two are independent on purpose — overriding only the subject is a common and reasonable
/// thing to want, and it should not require restating the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelTemplate {
    /// Template for the notification subject / summary line.
    pub subject: Option<String>,
    /// Template for the notification body / payload.
    pub body: Option<String>,
}

impl ChannelTemplate {
    /// Whether this channel overrides nothing, and so needs no rendering at all.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        self.subject.is_none() && self.body.is_none()
    }
}

/// Which field a failure happened on. Both are reported, because falling back is per-field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateField {
    /// The subject / summary line.
    Subject,
    /// The body / payload.
    Body,
}

impl TemplateField {
    /// Stable token, used in the API error and as a metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TemplateField::Subject => "subject",
            TemplateField::Body => "body",
        }
    }
}

/// Why a template could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateFailure {
    /// Which of the two fields failed.
    pub field: TemplateField,
    /// What went wrong.
    pub kind: FailureKind,
    /// Operator-facing detail. This is the operator's own template, so the engine's message
    /// (including the offending line) is exactly what they need and reveals nothing else.
    pub message: String,
}

/// The class of failure — also the `reason` label on the error counter, so the four are worth
/// keeping distinct: they point at four different mistakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The template does not parse.
    Compile,
    /// It parsed but evaluation failed (bad filter, attribute on undefined, fuel exhausted).
    Render,
    /// It rendered, but past the size cap.
    TooLarge,
    /// It rendered, but the channel needs JSON and the result is not JSON.
    NotJson,
}

impl FailureKind {
    /// Stable token for the metric label and the API response.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            FailureKind::Compile => "compile",
            FailureKind::Render => "render",
            FailureKind::TooLarge => "too_large",
            FailureKind::NotJson => "not_json",
        }
    }
}

/// Whether this channel's body has to be valid JSON.
///
/// Not cosmetic: a webhook body is POSTed verbatim under `content-type: application/json`, and a
/// PagerDuty body is parsed into `payload.custom_details` — where invalid JSON becomes `null`
/// **silently**, which is how an operator ends up with a page carrying no detail and no error.
/// JSM takes the body as a plain `description` string and email as plain text, so neither cares.
#[must_use]
pub fn body_must_be_json(kind: ChannelKind) -> bool {
    match kind {
        ChannelKind::Webhook | ChannelKind::PagerDuty => true,
        ChannelKind::Jsm | ChannelKind::Email => false,
    }
}

/// A sandboxed environment.
///
/// Built per call rather than shared: an `Environment` is cheap, templates here are a few hundred
/// bytes, and a notification is a dedup'd, retried, network-bound event — so compilation is not on
/// any hot path, and a shared environment would need invalidating whenever a template is edited.
fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    // Lenient: an undefined name renders as empty text, which is what makes an absent fact a
    // usable `{{ value }}` rather than an error. Attribute access on undefined still errors, so a
    // typo like `{{ node.name }}` is caught rather than silently blank.
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    env.set_fuel(Some(RENDER_FUEL));
    env
}

/// Turn a minijinja error into operator-facing text. The alternate form carries the template line
/// and the offending expression, which is the whole value of the preview endpoint.
fn detail(err: &minijinja::Error) -> String {
    format!("{err:#}")
}

/// Compile-check an operator's template pair without rendering it. Used at save time.
///
/// # Errors
/// Returns the first field that does not parse.
pub fn validate(template: &ChannelTemplate) -> Result<(), TemplateFailure> {
    let env = environment();
    for (field, source) in [
        (TemplateField::Subject, template.subject.as_deref()),
        (TemplateField::Body, template.body.as_deref()),
    ] {
        let Some(source) = source else { continue };
        env.template_from_str(source).map_err(|e| TemplateFailure {
            field,
            kind: FailureKind::Compile,
            message: detail(&e),
        })?;
    }
    Ok(())
}

/// Render one field, applying the size cap and the JSON rule.
fn render_field(
    env: &Environment<'static>,
    field: TemplateField,
    source: &str,
    facts: &AlertFacts,
    needs_json: bool,
) -> Result<String, TemplateFailure> {
    let fail = |kind: FailureKind, message: String| TemplateFailure {
        field,
        kind,
        message,
    };
    let out = env.render_str(source, facts).map_err(|e| {
        // A compile error surfaces here too when the template was stored by an older core, or
        // predates a validation rule. Distinguishing the two is worth the branch: "it does not
        // parse" and "it failed on this alert" send an operator to different places.
        let kind = if e.kind() == minijinja::ErrorKind::SyntaxError {
            FailureKind::Compile
        } else {
            FailureKind::Render
        };
        fail(kind, detail(&e))
    })?;

    let cap = match field {
        TemplateField::Subject => MAX_SUBJECT_CHARS,
        TemplateField::Body => MAX_BODY_CHARS,
    };
    let len = out.chars().count();
    if len > cap {
        // Deliberately not truncated. A clipped JSON body is invalid JSON, and a clipped subject
        // is a worse headline than the built-in one — so the built-in form wins outright.
        return Err(fail(
            FailureKind::TooLarge,
            format!("rendered {len} characters; the limit is {cap}"),
        ));
    }

    if needs_json && field == TemplateField::Body {
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&out) {
            return Err(fail(
                FailureKind::NotJson,
                format!(
                    "this channel sends the body as JSON, and the rendered text is not valid \
                     JSON: {e}. Values interpolated into JSON need the `tojson` filter, e.g. \
                     {{{{ node_name | tojson }}}}"
                ),
            ));
        }
    }
    Ok(out)
}

/// What a channel will actually send, and what (if anything) went wrong getting there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// Subject to send.
    pub subject: String,
    /// Body to send.
    pub body: String,
    /// Failures that forced a field back to its built-in form. Empty on the happy path, and also
    /// empty when the channel simply has no override.
    pub failures: Vec<TemplateFailure>,
}

/// Render a channel's overrides, falling back **per field** to the built-in text.
///
/// Per-field rather than all-or-nothing: the two templates are independent, so a typo in the body
/// should not also discard a subject the operator got right. The notification goes out either way —
/// this function has no failure mode that reaches the caller.
#[must_use]
pub fn render_with_fallback(
    template: Option<&ChannelTemplate>,
    facts: &AlertFacts,
    needs_json: bool,
    builtin_subject: &str,
    builtin_body: &str,
) -> Rendered {
    let mut out = Rendered {
        subject: builtin_subject.to_owned(),
        body: builtin_body.to_owned(),
        failures: Vec::new(),
    };
    let Some(template) = template.filter(|t| !t.is_builtin()) else {
        return out;
    };
    let env = environment();
    if let Some(source) = template.subject.as_deref() {
        match render_field(&env, TemplateField::Subject, source, facts, false) {
            Ok(s) => out.subject = s,
            Err(e) => out.failures.push(e),
        }
    }
    if let Some(source) = template.body.as_deref() {
        match render_field(&env, TemplateField::Body, source, facts, needs_json) {
            Ok(s) => out.body = s,
            Err(e) => out.failures.push(e),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{minimal_facts, sample_facts, NotifyEvent, TEMPLATE_VARIABLES};

    const BUILTIN_SUBJECT: &str = "node 6f1c9d2a is critical";
    const BUILTIN_BODY: &str = r#"{"builtin":true}"#;

    fn tpl(subject: Option<&str>, body: Option<&str>) -> ChannelTemplate {
        ChannelTemplate {
            subject: subject.map(str::to_owned),
            body: body.map(str::to_owned),
        }
    }

    fn render(t: &ChannelTemplate, needs_json: bool) -> Rendered {
        render_with_fallback(
            Some(t),
            &sample_facts(NotifyEvent::Fire),
            needs_json,
            BUILTIN_SUBJECT,
            BUILTIN_BODY,
        )
    }

    #[test]
    fn no_template_means_the_built_in_text_verbatim() {
        for t in [None, Some(&ChannelTemplate::default())] {
            let r = render_with_fallback(
                t,
                &sample_facts(NotifyEvent::Fire),
                true,
                BUILTIN_SUBJECT,
                BUILTIN_BODY,
            );
            assert_eq!(r.subject, BUILTIN_SUBJECT);
            assert_eq!(r.body, BUILTIN_BODY);
            assert!(r.failures.is_empty());
        }
    }

    #[test]
    fn a_template_interpolates_the_declared_variables() {
        let r = render(
            &tpl(
                Some("{{ severity | upper }}: {{ node_name }} ({{ metric }})"),
                Some("{{ node_name }} at {{ node_address }} — {{ value }} > {{ threshold }}"),
            ),
            false,
        );
        assert_eq!(r.subject, "CRITICAL: core-sw-01 (if_in_util_pct)");
        assert_eq!(
            r.body, "core-sw-01 at 192.0.2.11 — 94.2 > 90.0",
            "numbers must render as numbers"
        );
        assert!(r.failures.is_empty());
    }

    /// The reason minijinja was chosen over a `{{field}}` substituter: operators want wording that
    /// depends on the alert. If this stops working the dependency is not earning its place.
    #[test]
    fn a_template_can_branch_on_the_event_and_the_severity() {
        let source = "{% if event == 'resolve' %}RECOVERED{% elif severity == 'critical' %}\
                      PAGE{% else %}notice{% endif %} {{ node_name }}";
        for (event, want) in [
            (NotifyEvent::Fire, "PAGE core-sw-01"),
            (NotifyEvent::Resolve, "RECOVERED core-sw-01"),
        ] {
            let r = render_with_fallback(
                Some(&tpl(Some(source), None)),
                &sample_facts(event),
                false,
                BUILTIN_SUBJECT,
                BUILTIN_BODY,
            );
            assert_eq!(r.subject, want);
        }
    }

    /// An absent fact has to be *undefined*, not the text `none`. This is the property the
    /// `skip_serializing_if` in `yagra-common` exists for, checked from the engine's side.
    #[test]
    fn an_absent_fact_renders_empty_and_takes_a_default() {
        let r = render_with_fallback(
            Some(&tpl(
                Some("[{{ metric }}] [{{ value | default('n/a') }}] [{% if group %}g{% endif %}]"),
                None,
            )),
            &minimal_facts(NotifyEvent::Fire),
            false,
            BUILTIN_SUBJECT,
            BUILTIN_BODY,
        );
        assert_eq!(r.subject, "[] [n/a] []");
        assert!(r.failures.is_empty());
    }

    #[test]
    fn a_template_that_does_not_parse_is_rejected_at_save_time() {
        let err = validate(&tpl(None, Some("{% if severity %}unclosed"))).unwrap_err();
        assert_eq!(err.field, TemplateField::Body);
        assert_eq!(err.kind, FailureKind::Compile);
        assert!(!err.message.is_empty());
        validate(&tpl(Some("{{ node_name }}"), Some("{{ severity }}"))).unwrap();
        // An empty pair is valid — it means "use the built-in form".
        validate(&ChannelTemplate::default()).unwrap();
    }

    /// The load-bearing property of this whole module: whatever the template does, the caller
    /// still has something to send.
    #[test]
    fn a_render_failure_falls_back_to_the_built_in_text() {
        // Parses, then fails at evaluation: attribute access on an undefined name.
        let r = render(&tpl(Some("{{ nope.attr }}"), None), false);
        assert_eq!(r.subject, BUILTIN_SUBJECT, "the notification must still go");
        assert_eq!(r.failures.len(), 1);
        assert_eq!(r.failures[0].kind, FailureKind::Render);
        assert_eq!(r.failures[0].field, TemplateField::Subject);
    }

    /// Falling back is per field, so one typo does not discard the field that was written
    /// correctly.
    #[test]
    fn only_the_field_that_failed_falls_back() {
        let r = render(
            &tpl(Some("ok {{ node_name }}"), Some("{{ nope.attr }}")),
            false,
        );
        assert_eq!(r.subject, "ok core-sw-01");
        assert_eq!(r.body, BUILTIN_BODY);
        assert_eq!(r.failures.len(), 1);
        assert_eq!(r.failures[0].field, TemplateField::Body);
    }

    /// A runaway loop must not hang the notify worker. Bounded by fuel, not by hope.
    #[test]
    fn a_runaway_loop_exhausts_its_fuel_and_falls_back() {
        let r = render(
            &tpl(None, Some("{% for i in range(10000000) %}x{% endfor %}")),
            false,
        );
        assert_eq!(r.body, BUILTIN_BODY);
        assert_eq!(r.failures.len(), 1);
        // Fuel exhaustion is an evaluation failure, not a syntax one.
        assert_eq!(r.failures[0].kind, FailureKind::Render);
    }

    #[test]
    fn an_oversized_render_falls_back_rather_than_truncating() {
        // Few iterations, lots of output per iteration — so this trips the size cap rather than
        // the fuel budget. (The two limits guard different things; see the fuel test.)
        let long = "x".repeat(70);
        let body = format!("{{% for i in range(1000) %}}{long}{{% endfor %}}");
        let r = render(&tpl(None, Some(&body)), false);
        assert_eq!(r.body, BUILTIN_BODY, "a clipped body is worse than none");
        assert_eq!(r.failures[0].kind, FailureKind::TooLarge);

        let r = render(
            &tpl(
                Some("{% for i in range(200) %}0123456789{% endfor %}"),
                None,
            ),
            false,
        );
        assert_eq!(r.subject, BUILTIN_SUBJECT);
        assert_eq!(r.failures[0].kind, FailureKind::TooLarge);
    }

    /// The failure this catches is silent in production today: PagerDuty parses the body into
    /// `custom_details` with `unwrap_or(Null)`, so a body that stops being JSON becomes a page
    /// with no detail and no error anywhere.
    #[test]
    fn a_body_that_is_not_json_falls_back_on_a_json_channel() {
        let r = render(&tpl(None, Some("plain text, not json")), true);
        assert_eq!(r.body, BUILTIN_BODY);
        assert_eq!(r.failures[0].kind, FailureKind::NotJson);
        // …and the same body is fine on a channel that takes text.
        let r = render(&tpl(None, Some("plain text, not json")), false);
        assert_eq!(r.body, "plain text, not json");
        assert!(r.failures.is_empty());
    }

    /// The trap the `tojson` hint in the error message exists for: a device- or operator-supplied
    /// name containing a quote breaks a hand-built JSON body, and the escape is a filter away.
    #[test]
    fn tojson_is_what_makes_a_hand_built_json_body_safe() {
        let mut facts = sample_facts(NotifyEvent::Fire);
        facts.node_name = r#"sw"01"#.to_owned();
        let naive = render_with_fallback(
            Some(&tpl(None, Some(r#"{"n":"{{ node_name }}"}"#))),
            &facts,
            true,
            BUILTIN_SUBJECT,
            BUILTIN_BODY,
        );
        assert_eq!(naive.failures[0].kind, FailureKind::NotJson);
        assert!(naive.failures[0].message.contains("tojson"));

        let escaped = render_with_fallback(
            Some(&tpl(None, Some(r#"{"n":{{ node_name | tojson }}}"#))),
            &facts,
            true,
            BUILTIN_SUBJECT,
            BUILTIN_BODY,
        );
        assert!(escaped.failures.is_empty());
        assert_eq!(escaped.body, r#"{"n":"sw\"01"}"#);
    }

    /// Nothing outside the declared set is reachable, and reaching for it is not an error either —
    /// it renders empty, so a stale template keeps working with a blanked field rather than
    /// falling back wholesale.
    #[test]
    fn an_undeclared_name_renders_empty_rather_than_failing() {
        let r = render(&tpl(Some("[{{ snmp_community }}]"), None), false);
        assert_eq!(r.subject, "[]");
        assert!(r.failures.is_empty());
    }

    /// Every name the catalogue advertises has to actually resolve; a palette entry that renders
    /// empty would look like a Yagra bug to the operator who clicked it.
    #[test]
    fn every_advertised_variable_resolves_against_the_sample() {
        let facts = sample_facts(NotifyEvent::Fire);
        for v in TEMPLATE_VARIABLES {
            let source = format!("{{{{ {} }}}}", v.name);
            let r = render_with_fallback(
                Some(&tpl(Some(&source), None)),
                &facts,
                false,
                BUILTIN_SUBJECT,
                BUILTIN_BODY,
            );
            assert!(
                r.failures.is_empty(),
                "`{}` failed to render: {:?}",
                v.name,
                r.failures
            );
            assert!(
                !r.subject.is_empty(),
                "`{}` is advertised but renders empty on the preview sample",
                v.name
            );
        }
    }

    /// Only the two channel kinds that put the body into JSON demand it. Exhaustive so a new
    /// channel kind has to answer the question.
    #[test]
    fn the_json_requirement_follows_how_the_channel_carries_the_body() {
        assert!(body_must_be_json(ChannelKind::Webhook));
        assert!(body_must_be_json(ChannelKind::PagerDuty));
        assert!(!body_must_be_json(ChannelKind::Jsm));
        assert!(!body_must_be_json(ChannelKind::Email));
    }

    #[test]
    fn failure_tokens_are_stable_and_distinct() {
        let tokens: Vec<&str> = [
            FailureKind::Compile,
            FailureKind::Render,
            FailureKind::TooLarge,
            FailureKind::NotJson,
        ]
        .iter()
        .map(|k| k.as_str())
        .collect();
        assert_eq!(tokens, ["compile", "render", "too_large", "not_json"]);
        assert_eq!(TemplateField::Subject.as_str(), "subject");
        assert_eq!(TemplateField::Body.as_str(), "body");
    }
}
