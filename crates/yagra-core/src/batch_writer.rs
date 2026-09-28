// SPDX-License-Identifier: AGPL-3.0-only
//! Draining a bounded queue into batched writes, one way (ADR-184).
//!
//! The event persist writer, the event-alert action writer and the ClickHouse flow writer were the
//! same forty-line loop: wait for one item, take whatever else is already queued up to a cap, write
//! the batch, report the queue's depth — and on shutdown drain what is left and write it once more.
//! Three copies of a shutdown path is three chances for one to stop flushing, and nothing but a
//! deployment that restarts under load would notice.
//!
//! What differed is one decision, and it is the argument: [`FlushPolicy`].
//!
//! Not written through here, on purpose: `result_ingest.rs`'s two writers. They linger for a batch
//! to fill, spill to disk when the store is down and feed two channels — a different program that
//! happens to start with `recv`.

use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

/// When a drained batch is written.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FlushPolicy {
    /// After every drain, however small — the batch is whatever had queued up while the last write
    /// ran. For writes whose latency someone waits on (an event row, an alert notification).
    EveryDrain,
    /// Only when the batch is full, or when this much time has passed — for a store that is cheap
    /// per batch and expensive per round trip (ClickHouse).
    WhenFullOrEvery(Duration),
}

/// Run the writer until the channel closes or `shutdown` fires; either way, what is queued is
/// written before returning. `stream` labels the `yagra_persist_queue_depth` gauge. `flush` must
/// leave the buffer empty.
///
/// `flush` returns a boxed future rather than being an `AsyncFnMut`: the writers are spawned, and
/// an async closure's future cannot yet be required to be `Send`. One allocation per batch.
pub(crate) async fn run<T, F>(
    mut rx: Receiver<T>,
    batch_max: usize,
    policy: FlushPolicy,
    stream: &'static str,
    shutdown: CancellationToken,
    mut flush: F,
) where
    F: for<'a> FnMut(&'a mut Vec<T>) -> BoxFuture<'a, ()>,
{
    let mut buf: Vec<T> = Vec::new();
    let mut ticker = match policy {
        FlushPolicy::EveryDrain => None,
        FlushPolicy::WhenFullOrEvery(every) => {
            let mut t = tokio::time::interval(every);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            Some(t)
        }
    };
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(item) = rx.try_recv() {
                    buf.push(item);
                    if buf.len() >= batch_max {
                        flush(&mut buf).await;
                    }
                }
                flush(&mut buf).await;
                break;
            }
            () = async {
                match ticker.as_mut() {
                    Some(t) => {
                        t.tick().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {
                flush(&mut buf).await;
            }
            first = rx.recv() => {
                let Some(item) = first else {
                    flush(&mut buf).await;
                    break;
                };
                buf.push(item);
                while buf.len() < batch_max {
                    match rx.try_recv() {
                        Ok(item) => buf.push(item),
                        Err(_) => break,
                    }
                }
                let write_now = match policy {
                    FlushPolicy::EveryDrain => true,
                    FlushPolicy::WhenFullOrEvery(_) => buf.len() >= batch_max,
                };
                if write_now {
                    flush(&mut buf).await;
                }
                metrics::gauge!("yagra_persist_queue_depth", "stream" => stream).set(rx.len() as f64);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Batches = Arc<Mutex<Vec<Vec<u32>>>>;

    fn recorder() -> (
        Batches,
        impl for<'a> FnMut(&'a mut Vec<u32>) -> BoxFuture<'a, ()>,
    ) {
        let batches: Batches = Arc::new(Mutex::new(Vec::new()));
        let log = batches.clone();
        let flush = flusher(move |buf| {
            let log = log.clone();
            Box::pin(async move {
                if !buf.is_empty() {
                    log.lock().unwrap().push(std::mem::take(buf));
                }
            })
        });
        (batches, flush)
    }

    /// Where a hand-written drain is allowed, and why.
    const DRAINS_ITS_OWN: &[(&str, &str)] = &[
        ("batch_writer.rs", "the one loop"),
        (
            "result_ingest.rs",
            "lingers for a batch to fill, spills to disk and feeds two channels — a different program",
        ),
    ];

    /// ADR-184: nobody else fills a batch by `try_recv` up to a cap. The needle is the fill loop
    /// itself — `while buf.len() < MAX { match rx.try_recv() …` — built at run time.
    #[test]
    fn no_other_module_drains_a_channel_by_hand() {
        let files = crate::module_source::crate_code();
        assert!(files.len() >= 150, "only {} files were read", files.len());
        let fill = regex::Regex::new(&format!(
            r"while \w+\.len\(\) < \w+ \{{\s*match \w+\.{}\(\)",
            "try_recv"
        ))
        .unwrap();
        let mut found = Vec::new();
        for (name, code) in &files {
            if fill.is_match(code) {
                found.push(name.as_str());
            }
        }
        let offenders: Vec<&&str> = found
            .iter()
            .filter(|n| !DRAINS_ITS_OWN.iter().any(|(f, _)| f == *n))
            .collect();
        assert!(
            offenders.is_empty(),
            "{offenders:?} drain a queue into a batch by hand — use `batch_writer::run`"
        );
        assert_eq!(
            found.len(),
            DRAINS_ITS_OWN.len(),
            "the fill loop was not found where it is declared: {found:?}"
        );
    }

    /// Hands a closure to the compiler with the higher-ranked bound already stated, which is what
    /// lets it infer the future's lifetime from the argument's.
    fn flusher<F>(f: F) -> F
    where
        F: for<'a> FnMut(&'a mut Vec<u32>) -> BoxFuture<'a, ()>,
    {
        f
    }

    #[tokio::test]
    async fn every_drain_writes_what_had_queued_and_the_rest_on_close() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        for i in 0..5 {
            tx.send(i).await.unwrap();
        }
        drop(tx);
        let (batches, flush) = recorder();
        run(
            rx,
            3,
            FlushPolicy::EveryDrain,
            "test",
            CancellationToken::new(),
            flush,
        )
        .await;
        assert_eq!(*batches.lock().unwrap(), vec![vec![0, 1, 2], vec![3, 4]]);
    }

    #[tokio::test]
    async fn a_shutdown_writes_everything_still_queued() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        for i in 0..4 {
            tx.send(i).await.unwrap();
        }
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let (batches, flush) = recorder();
        run(rx, 3, FlushPolicy::EveryDrain, "test", shutdown, flush).await;
        assert_eq!(*batches.lock().unwrap(), vec![vec![0, 1, 2], vec![3]]);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn when_full_waits_for_a_full_batch_or_the_timer() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let (batches, flush) = recorder();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(run(
            rx,
            3,
            FlushPolicy::WhenFullOrEvery(Duration::from_secs(10)),
            "test",
            shutdown.clone(),
            flush,
        ));
        tx.send(1).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            batches.lock().unwrap().is_empty(),
            "one row is not a full batch"
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            *batches.lock().unwrap(),
            vec![vec![1]],
            "the timer wrote it"
        );
        for i in 2..5 {
            tx.send(i).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            batches.lock().unwrap().len(),
            2,
            "a full batch is written at once"
        );
        shutdown.cancel();
        writer.await.unwrap();
    }
}
