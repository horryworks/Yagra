# Vendored snmp2 0.5.0 (patched)

Vendored from crates.io `snmp2 = 0.5.0` (MIT OR Apache-2.0, upstream
https://github.com/roboplc/snmp2) with **four local patches**:

- `src/v3.rs`: the six `mac.update(data)` calls in `calculate_hmac` are
  disambiguated to `hmac::Mac::update(&mut mac, data)`. Upstream code is
  ambiguous (E0034) whenever another crate in the dependency graph enables the
  `hmac/reset` feature — `sqlx-postgres` does — because `Hmac<D>` then also
  satisfies digest's blanket `DynDigest` impl, which has its own `update`.
- `src/pdu.rs`: `fn build` and `struct Buf` widened from `pub(crate)` to `pub`
  so Yagra's trap-reception tests (`yagra-ingest`) can build trap/inform PDU
  byte fixtures through the supported encoder instead of hand-rolled ASN.1.
- `src/pdu.rs` + `src/v3.rs` (ADR-158): **a PDU that does not fit the fixed
  `BUFFER_SIZE` buffer is an error, not a panic.** `Buf` writes backwards from
  the end of the buffer and upstream indexed past its start once a PDU outgrew
  it. That was reachable from the network: a received inform re-encodes larger
  than it arrived (Timeticks `43 01 FF` becomes `43 05 00 FF FF FF FF`), and
  one crafted datagram panicked a trap reader for good. `Buf` now carries an
  `overflowed` mark — a push that does not fit writes nothing and sets it,
  `reset()` clears it, `Buf::overflowed()` reads it — and `pdu::build`,
  `Pdu::to_bytes` (v1 path) and `v3::build` (before encrypting and before
  signing) return `Error::BufferOverflow` when it is set. `v3::build`'s
  auth-position arithmetic saturates so a short buffer cannot underflow first.
- `src/pdu.rs` (ADR-158): `Buf::push_i64` rewritten without `unsafe`. Upstream
  copied through raw pointers and computed `available().len() - 8` before
  checking that eight bytes were free. The bytes produced are identical; the
  encodings across every sign and length boundary are pinned by
  `yagra-ingest`'s `integers_encode_minimally_across_every_sign_boundary`
  (this vendored crate is outside the workspace, so its own tests never run).

Wired in via `[patch.crates-io]` in the workspace `Cargo.toml`. Drop this
vendor copy (and the patch entry) once upstream ships a fix.

Removed from the upstream package (not needed to build): `Cargo.lock`,
`Cargo.toml.orig`, `justfile`, `rust-toolchain.toml`, sample MIB file.
