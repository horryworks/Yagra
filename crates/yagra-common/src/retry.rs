// SPDX-License-Identifier: AGPL-3.0-only
//! "Try until the other side is up", one way (ADR-184).
//!
//! Each binary starts before the things it talks to are ready, so each one retried its first
//! connection: core to PostgreSQL and to NATS, the poller to NATS, core to ClickHouse's schema.
//! Four hand-written loops, three of them identical, and one had learned something the others had
//! not — a URL that does not parse is not "not up yet", and retrying it spends a minute before
//! blaming a healthy server (measured on a GCE deployment, 2026-09-08). Here that distinction is
//! an argument, so every caller has to answer it.
//!
//! No runtime and no logging in this crate: the caller passes its own `sleep` and says what to log
//! on each retry.

use std::future::Future;
use std::time::Duration;

/// How long to keep trying: `retries` more attempts after the first, `delay` apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub retries: u32,
    pub delay: Duration,
}

/// Why [`until_ready`] stopped without an answer.
#[derive(Debug)]
pub enum GaveUp<E> {
    /// The operation failed in a way waiting cannot repair; nothing was retried after it.
    Fatal(E),
    /// Every attempt in the budget failed; the error is the last one.
    Exhausted(E),
}

impl<E> GaveUp<E> {
    /// The error, whichever way it ended.
    pub fn into_inner(self) -> E {
        match self {
            Self::Fatal(e) | Self::Exhausted(e) => e,
        }
    }
}

/// Run `op` until it succeeds, it fails fatally, or the budget is spent. `on_retry` is told each
/// error that will be retried and the attempt number it failed on (1-based).
///
/// # Errors
/// [`GaveUp::Fatal`] at once for an error `is_fatal` accepts; [`GaveUp::Exhausted`] after
/// `budget.retries + 1` failed attempts.
pub async fn until_ready<T, E, Op, Fut, Sleep, SleepFut>(
    budget: Budget,
    is_fatal: impl Fn(&E) -> bool,
    mut on_retry: impl FnMut(&E, u32),
    mut op: Op,
    mut sleep: Sleep,
) -> Result<T, GaveUp<E>>
where
    Op: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if is_fatal(&e) => return Err(GaveUp::Fatal(e)),
            Err(e) if attempt > budget.retries => return Err(GaveUp::Exhausted(e)),
            Err(e) => {
                on_retry(&e, attempt);
                sleep(budget.delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::future::ready;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Drive a future that never actually waits (every sleep here is `ready(())`).
    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("nothing here should wait"),
        }
    }

    const BUDGET: Budget = Budget {
        retries: 3,
        delay: Duration::from_secs(2),
    };

    #[test]
    fn an_answer_on_a_later_attempt_is_the_answer_and_each_miss_is_reported() {
        let calls = Cell::new(0);
        let mut seen = Vec::new();
        let mut slept = Duration::ZERO;
        let got = block_on(until_ready(
            BUDGET,
            |_: &&str| false,
            |e, n| seen.push((e.to_string(), n)),
            || {
                calls.set(calls.get() + 1);
                ready(if calls.get() < 3 { Err("down") } else { Ok(7) })
            },
            |d| {
                slept += d;
                ready(())
            },
        ));
        assert_eq!(got.ok(), Some(7));
        assert_eq!(seen, vec![("down".to_owned(), 1), ("down".to_owned(), 2)]);
        assert_eq!(slept, Duration::from_secs(4));
    }

    #[test]
    fn a_fatal_error_is_not_retried() {
        let calls = Cell::new(0);
        let got: Result<(), _> = block_on(until_ready(
            BUDGET,
            |e: &&str| *e == "bad url",
            |_, _| panic!("a fatal error must not be retried"),
            || {
                calls.set(calls.get() + 1);
                ready(Err("bad url"))
            },
            |_| ready(()),
        ));
        assert!(matches!(got, Err(GaveUp::Fatal("bad url"))));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn the_budget_is_one_attempt_plus_the_retries() {
        let calls = Cell::new(0);
        let got: Result<(), _> = block_on(until_ready(
            BUDGET,
            |_: &&str| false,
            |_, _| {},
            || {
                calls.set(calls.get() + 1);
                ready(Err("down"))
            },
            |_| ready(()),
        ));
        assert!(matches!(got, Err(GaveUp::Exhausted("down"))));
        assert_eq!(calls.get(), 4);
    }
}
