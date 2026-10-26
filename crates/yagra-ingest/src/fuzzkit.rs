// SPDX-License-Identifier: AGPL-3.0-only
//! Test-only: hammer a parser with broken copies of valid datagrams (ADR-215 decision 3).
//!
//! Every parser in this crate reads bytes anyone on the LAN can send to a poller — no
//! authentication, a spoofable source — and the trap path runs through the vendored `snmp2`, whose
//! upstream advisories may never reach us. This is the regression net for that: a fixed set of
//! mutations, from a fixed seed, run inside the ordinary `cargo test` so every `/flashdeploy` runs it.
//!
//! ⚠️ It is not coverage-guided fuzzing and does not claim to be. It finds the classic shapes —
//! a length field that lies, a truncation at every depth, a count that overflows a slice — and it
//! finds them the same way every run, so a failure is reproducible from the hex it prints.
//!
//! Two properties are checked per input: the parser returns (a panic fails the test, naming the
//! input), and it returns quickly — [`SLOW`] is far above any honest parse, so crossing it means a
//! loop or an allocation that grows with a number the sender chose.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

/// One parse taking this long is a defect, not a slow machine: every honest parse here is
/// microseconds, and a datagram is at most 64 KiB.
const SLOW: Duration = Duration::from_millis(250);

/// A small deterministic generator (xorshift64*). Not for anything but picking mutations.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// One broken copy of `seed`: between one and four mutations stacked on top of each other.
pub(crate) fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..=rng.below(4) {
        match rng.below(8) {
            // Flip one bit.
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            // Replace one byte with an extreme or a random value.
            1 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = [0x00, 0x7f, 0x80, 0xff, rng.next() as u8][rng.below(5)];
            }
            // Cut it short — every depth of every structure ends somewhere.
            2 => v.truncate(rng.below(v.len() + 1)),
            // A two-byte field that lies: zero, all ones, or just past the end.
            3 if v.len() >= 2 => {
                let i = rng.below(v.len() - 1);
                let lie: u16 =
                    [0, 0xffff, v.len() as u16, (v.len() as u16).wrapping_add(1)][rng.below(4)];
                v[i..i + 2].copy_from_slice(&lie.to_be_bytes());
            }
            // A BER length that lies: the long form claiming a huge length.
            4 if !v.is_empty() => {
                let i = rng.below(v.len());
                v.splice(i..i, [0x84, 0x7f, 0xff, 0xff, 0xff]);
            }
            // Repeat a chunk, as a record or a varbind repeated.
            5 if !v.is_empty() => {
                let start = rng.below(v.len());
                let len = 1 + rng.below((v.len() - start).min(64));
                let chunk = v[start..start + len].to_vec();
                let at = rng.below(v.len() + 1);
                v.splice(at..at, chunk);
            }
            // Junk on the end.
            6 => {
                let extra = rng.below(64);
                v.extend((0..extra).map(|_| rng.next() as u8));
            }
            // Delete a chunk from the middle.
            _ if v.len() > 1 => {
                let start = rng.below(v.len());
                let len = 1 + rng.below(v.len() - start);
                v.drain(start..start + len);
            }
            _ => {}
        }
    }
    v
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Run `parse` over every seed as it is, then `rounds` mutations of each. Panics — naming the
/// input — if one parse panics or takes longer than [`SLOW`]. Returns how many inputs were run, so
/// a caller can assert the harness was not handed nothing.
pub(crate) fn hammer(
    name: &str,
    seeds: &[Vec<u8>],
    rounds: usize,
    seed: u64,
    mut parse: impl FnMut(&[u8]),
) -> usize {
    assert!(!seeds.is_empty(), "{name}: no seeds");
    // `YAGRA_FUZZ_SCALE=N` runs N times the rounds from a different seed: the deeper, one-off
    // search. Unset, every run is the same fixed net.
    let scale: usize = std::env::var("YAGRA_FUZZ_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1);
    let rounds = rounds * scale;
    let mut rng = Rng::new(seed ^ (scale as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut run = 0usize;
    let one = |input: &[u8], parse: &mut dyn FnMut(&[u8])| {
        let started = Instant::now();
        if catch_unwind(AssertUnwindSafe(|| parse(input))).is_err() {
            panic!("{name}: the parser panicked on {}", hex(input));
        }
        let took = started.elapsed();
        assert!(
            took < SLOW,
            "{name}: one parse took {took:?} on {} ({} bytes)",
            hex(&input[..input.len().min(256)]),
            input.len()
        );
    };
    for s in seeds {
        one(s, &mut parse);
        run += 1;
        for _ in 0..rounds {
            let input = mutate(s, &mut rng);
            one(&input, &mut parse);
            run += 1;
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_are_reproducible_and_actually_mutate() {
        let seed = b"0123456789abcdef".to_vec();
        let a: Vec<Vec<u8>> = {
            let mut r = Rng::new(7);
            (0..50).map(|_| mutate(&seed, &mut r)).collect()
        };
        let b: Vec<Vec<u8>> = {
            let mut r = Rng::new(7);
            (0..50).map(|_| mutate(&seed, &mut r)).collect()
        };
        assert_eq!(a, b, "the same seed gives the same inputs");
        assert!(
            a.iter().filter(|m| **m != seed).count() > 40,
            "most copies differ"
        );
    }

    /// The harness itself is the thing that has to be believed, so it is shown to fail: a parser
    /// that panics on one byte value is caught and the input is named.
    #[test]
    fn a_panicking_parser_is_caught() {
        let result = catch_unwind(|| {
            hammer("probe", &[vec![1, 2, 3, 4]], 500, 1, |b| {
                assert!(!b.contains(&0xff), "boom");
            })
        });
        let err = result.expect_err("a 0xff appears within 500 mutations");
        let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("probe: the parser panicked on"), "{msg}");
    }
}
