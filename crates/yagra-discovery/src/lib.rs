//! Yagra-discovery — device discovery and classification.
//!
//! IP-range / SNMP sweep / LLDP-CDP based discovery, classification into profiles, and
//! the built-in **Credential Finder** that probes candidate credentials to find the one a
//! device accepts. The per-device probe rate limiter ([`credential_finder`]) is in place;
//! sweep/classification logic lands as the discovery feature is built out.

pub mod credential_finder;

pub use credential_finder::{AttemptDecision, CredentialProbeLimiter, LimiterConfig};
