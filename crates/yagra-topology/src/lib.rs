// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-topology — the connectivity graph and how an outage's alerts are grouped.
//!
//! [`derive`] turns what the fleet observed (CDP/LLDP neighbours, interface addresses, routing
//! adjacencies) into undirected links between nodes (ADR-043). [`grouping`] decides, when nodes go
//! down together, whose notification is rolled up into whose (ADR-236): nothing names a parent in
//! advance, and a down node inside a region of down nodes is rolled up into the region's border.

pub mod derive;
pub mod grouping;
