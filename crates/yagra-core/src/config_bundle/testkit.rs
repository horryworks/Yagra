// SPDX-License-Identifier: AGPL-3.0-only
//! The empty bundle every test that needs one starts from.
//!
//! Here rather than in one of the test modules because two of them need it (`super::types` and
//! `super` itself), and a fixture copied into both is the shape `extensibility.md` §3 is about.
//! Declared `#[cfg(test)] mod testkit;` in [`super`], which is also how `srcread` derives that this
//! file is excluded from the text the guards read.

use chrono::Utc;

use super::*;

pub(super) fn empty_bundle() -> ConfigBundle {
    ConfigBundle {
        format: BUNDLE_FORMAT.to_owned(),
        version: BUNDLE_VERSION,
        exported_at: Utc::now(),
        yagra_version: "test".to_owned(),
        secrets: SecretsMode::References,
        notes: Vec::new(),
        app_settings: None,
        profiles: Vec::new(),
        collection_templates: Vec::new(),
        collection_template_items: Vec::new(),
        profile_collection_templates: Vec::new(),
        classification_rules: Vec::new(),
        node_groups: Vec::new(),
        nodes: Vec::new(),
        thresholds: Vec::new(),
        url_checks: Vec::new(),
        dns_checks: Vec::new(),
        forward_destinations: Vec::new(),
        event_sources: Vec::new(),
        event_rules: Vec::new(),
        report_definitions: Vec::new(),
        report_schedules: Vec::new(),
        analysis_schedules: Vec::new(),
    }
}
