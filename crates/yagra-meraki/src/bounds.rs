// SPDX-License-Identifier: AGPL-3.0-only
//! The bounds an organization's cadences, device cap and request rate are validated against.
//!
//! They lived in core's `config.rs` until the integration became its own crate (ADR-202 Inc.6);
//! they are facts about the Meraki tables, not about the process environment.

// ── Cisco Meraki cadence bounds (own band, NOT the per-node 1h cap — slow tiers must not be
// blocked). Mirror the CHECK constraints in migration 0038 — and 0124 for the inventory floor, 0130
// for the switch ports, 0131 for the wireless tier; the API validates against these. ──
/// Availability/uplink tier cadence bounds (seconds).
pub const MERAKI_FAST_MIN_SECS: i32 = 60;
pub const MERAKI_FAST_MAX_SECS: i32 = 3600;
/// Switch-port tier cadence bounds (seconds, ADR-167 decision 11). The Dashboard has nothing finer than
/// its five-minute usage buckets, so a faster collect would read the same bucket twice; and much
/// past ten minutes a healthy port's row would cross `repo::INTERFACE_STALE_SECS` (900 s) between
/// two collects and be drawn as stale.
pub const MERAKI_SWITCH_PORTS_MIN_SECS: i32 = 300;
pub const MERAKI_SWITCH_PORTS_MAX_SECS: i32 = 600;
/// Wireless tier cadence bounds (seconds, ADR-168 decision 9): the channel utilization is a five-minute
/// bucket, and much past ten minutes a radio's row would be drawn as stale between two collects.
pub const MERAKI_WIRELESS_MIN_SECS: i32 = 300;
pub const MERAKI_WIRELESS_MAX_SECS: i32 = 600;
/// Traffic tier cadence bounds (seconds). The floor is the shortest usage window the Dashboard was
/// measured to accept, and a new organization starts on it (migration 0135, ADR-164 decision 34).
pub const MERAKI_TRAFFIC_MIN_SECS: i32 = 300;
pub const MERAKI_TRAFFIC_MAX_SECS: i32 = 86_400;
/// Inventory sync cadence bounds (seconds). The floor was 900 while the tier did nothing; since
/// ADR-164 it is how long a newly connected device waits to be noticed, and three paged GETs a
/// minute is still under 1% of the Dashboard API's per-organization limit (migration 0124).
pub const MERAKI_INVENTORY_MIN_SECS: i32 = 60;
pub const MERAKI_INVENTORY_MAX_SECS: i32 = 604_800;
/// The most nodes one organization may be allowed to hold through automatic import (ADR-164).
/// Mirrors the CHECK in migration 0125; the default a new organization starts with (10,000 since
/// migration 0134, ADR-164 decision 33) is that column's, and lives nowhere else. The cap is never silent: what it leaves out is written back
/// as `devices_over_cap` and shown on the organization's page.
pub const MERAKI_MAX_DEVICES_HARD: i32 = 50_000;
/// Hard cap on the per-org request-rate budget (requests/sec) — a safeguard so an operator can't
/// dial polling up to a level that would starve the customer's own Dashboard API usage.
pub const MERAKI_TARGET_RPS_MAX: f64 = 10.0;
