// SPDX-License-Identifier: AGPL-3.0-only
//! Metrics after they arrive: result ingest, the VictoriaMetrics client and writer, derived metrics
//! and what each metric means (ADR-012/025/105).

pub(crate) mod batch_writer;
pub(crate) mod derived;
pub(crate) mod host_collector;
pub(crate) mod interface_util;
pub(crate) mod metric_meaning;
pub(crate) mod result_ingest;
pub(crate) mod sink;
pub(crate) mod store;
