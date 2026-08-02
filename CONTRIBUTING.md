<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Contributing

## Pull requests are not being accepted right now

**Please do not open a pull request.** Yagra is developed by a single maintainer and is not set up
to review or merge outside contributions at the moment. A PR opened today will be closed without
review — not because the work is unwelcome, but because there is no process behind it yet.

This is a current state, not a permanent policy. When it changes, this file changes with it.

You are of course free to fork and modify Yagra under the terms of the AGPL — see
[LICENSE](LICENSE) and the plain-language summary in the [README](README.md#license). Nothing here
restricts what the licence grants you; it only says what this repository will merge.

## What is welcome

- **Bug reports.** Open an issue. Include the Yagra version (or image tag), what you ran, what you
  expected, and what happened. Logs from `yagra-core` / `yagra-poller` help enormously.
- **Feature requests and design discussion.** Open an issue. Being told which parts of an NMS are
  actually painful in the field is more valuable than code.
- **Security reports.** Do **not** use the issue tracker — see [SECURITY.md](SECURITY.md) for the
  private reporting channel.

## Licensing of contributions

Yagra is licensed under **AGPL-3.0-only**. A separate commercial licence may be offered to
operators who cannot accept the AGPL's terms (see the README's License section), which means the
maintainer must hold the rights necessary to relicense the whole work.

Practically: if PRs are opened in future, accepting one will require a contributor licence
agreement or an equivalent grant. This is stated up front so nobody writes code under a
misunderstanding about where it can end up.

## If you are working in a fork

The repository conventions, should they be useful to you:

- `cargo fmt --all --check`, then `cargo clippy --workspace --all-targets -- -D warnings`, then
  `cargo test --workspace`. CI gates on all three; the formatting check failing fails everything.
- In `web/`: `npm run test`, `npm run lint`, `npm run i18n:check`, `npm run build`. Every
  user-facing string exists in **both** English and Japanese, and the parity check enforces it.
- API shapes are **generated, not hand-written**. Changing a handler under
  `crates/yagra-core/src/api/` means regenerating the OpenAPI document and the TypeScript types;
  CI fails on any diff. See the commands at the top of `crates/yagra-core/src/api/openapi.rs`.
- Behaviour changes an operator or API client could notice get a bullet under `## Unreleased` in
  `RELEASE_NOTES.md` and `RELEASE_NOTES.ja.md`, in the same commit that ships them.
- Backend components talk to each other **only over the bus** (`yagra-bus`), and device I/O goes
  **only through** the `Transport` trait (`yagra-transport`). Those two boundaries are what make
  pollers stateless and remotely deployable.

## Running it

`README.md` has the quickstart; `DEPLOYMENT.md` covers a real deployment. `docker compose up
--build` from a clean clone needs no configuration.
