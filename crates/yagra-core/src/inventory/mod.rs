// SPDX-License-Identifier: AGPL-3.0-only
//! The inventory tree read as sites and ranges: subnet overlaps, missing prefixes, sites, effective
//! labels and pins (ADR-135/146/170/187).

/// Per-account pins on the inventory tree (ADR-146).
pub(crate) mod pins;
// Which subnets a folder's devices carry that its IP ranges do not cover (ADR-170).
pub(crate) mod prefix_gaps;
/// Which site a node belongs to — one rule for Subnet overlaps and Missing IP prefixes.
pub(crate) mod sites;
/// Which address ranges two sites both use (ADR-187). Pure; `api::subnet_overlaps` reads the stores.
pub(crate) mod subnet_overlaps;
/// Effective label resolution (own + every ancestor folder's − excluded). The accumulating
/// counterpart to `poolres`, which resolves one value from the nearest folder that carries it.
pub(crate) mod tagres;
