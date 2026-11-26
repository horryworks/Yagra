// SPDX-License-Identifier: AGPL-3.0-only
//! The loop that ends a held notification's wait (ADR-236 Inc.2 decision 11).
//!
//! A border whose neighbour is failing without having committed is held rather than paged. When
//! the neighbour commits, the poll path settles it at once; when the neighbour recovers or never
//! commits, no poll result of the held node says so, so this asks the engine every
//! [`TICK`]. The judgement is [`AlertManager::release_held`]; this file is the tick and the
//! delivery, in the shape `deleted` uses.
//!
//! The same tick sends the update an outage owes once more nodes have been grouped into it and its
//! minute of gathering is over (ADR-270 decision 3): no poll result says that minute has passed
//! either, and `release_held` hands both out.

use std::sync::Arc;
use std::time::Duration;

use super::AlertManager;
use yagra_alert::action::AlertSink;

/// How often a held notification and a pending update are looked at. It adds at most this much to
/// a wait that is a minute or more long, and with nothing held or pending a tick takes a few locks
/// and returns.
const TICK: Duration = Duration::from_secs(15);

/// Leader-only: page every held notification whose wait is over, and send every update that is
/// due.
///
/// Leader-only for the reason every alert loop is: poll-result ingest runs on the leader, so only
/// its engine holds anything, and two instances delivering the same release would page twice.
pub(crate) async fn run_held_release_watch(alerts: Arc<AlertManager>, sink: Arc<dyn AlertSink>) {
    loop {
        tokio::time::sleep(TICK).await;
        for action in alerts.release_held(yagra_common::clock::now_unix_ms()) {
            sink.dispatch(action).await;
        }
    }
}
