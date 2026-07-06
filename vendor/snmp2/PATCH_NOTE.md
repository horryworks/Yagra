# Vendored snmp2 0.5.0 (patched)

Vendored from crates.io `snmp2 = 0.5.0` (MIT OR Apache-2.0, upstream
https://github.com/roboplc/snmp2) with **two local patches**:

- `src/v3.rs`: the six `mac.update(data)` calls in `calculate_hmac` are
  disambiguated to `hmac::Mac::update(&mut mac, data)`. Upstream code is
  ambiguous (E0034) whenever another crate in the dependency graph enables the
  `hmac/reset` feature — `sqlx-postgres` does — because `Hmac<D>` then also
  satisfies digest's blanket `DynDigest` impl, which has its own `update`.
- `src/pdu.rs`: `fn build` and `struct Buf` widened from `pub(crate)` to `pub`
  so Yagra's trap-reception tests (`yagra-ingest`) can build trap/inform PDU
  byte fixtures through the supported encoder instead of hand-rolled ASN.1.

Wired in via `[patch.crates-io]` in the workspace `Cargo.toml`. Drop this
vendor copy (and the patch entry) once upstream ships a fix.

Removed from the upstream package (not needed to build): `Cargo.lock`,
`Cargo.toml.orig`, `justfile`, `rust-toolchain.toml`, sample MIB file.
