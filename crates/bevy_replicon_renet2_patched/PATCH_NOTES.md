# Why this crate is vendored

Upstream: [UkoeHB/renet2](https://github.com/UkoeHB/renet2), subdirectory
`bevy_replicon_renet2`, version `0.18.0` (the published crates.io release,
downloaded and extracted verbatim — this is not a `git` checkout).

`bevy_replicon_renet2` 0.18.0 requires `bevy_replicon = "^0.41"` — both on
crates.io and on upstream's unreleased `main` branch as of 2026-08-12 (checked
directly). This project is pinned to `bevy_replicon 0.42`, and those two
version ranges do not overlap at all, so the published crate cannot be added
as a dependency here without Cargo failing to resolve.

Before patching, the full crate source (460 lines across `src/`) was checked
for any use of the specific APIs that actually changed between
`bevy_replicon` 0.41 and 0.42 (`RepliconTick`'s dropped `Ord`/`PartialOrd`,
`TrackAppExt::track_mutate_messages` moving to `ServerPlugin`,
`DiffIndex::is_newer_than` being renamed, `ServerMutateTicks` always being
present) — zero matches. The `^0.41` constraint upstream appears to simply be
unbumped, not a real incompatibility.

## What was actually changed

Only `Cargo.toml`, two lines:

1. `bevy_replicon = { version = "0.41", ... }` → `{ version = "0.42", ... }`.
2. `bevy_renet2 = { path = "../bevy_renet2", version = "0.16.0", ... }` →
   dropped the `path` (it pointed at a sibling directory in upstream's
   monorepo that doesn't exist here); resolves `bevy_renet2` from crates.io
   via the `version` requirement alone, same as any other dependency.

No `src/` files were touched.

## When to remove this

Check upstream periodically (`https://crates.io/crates/bevy_replicon_renet2`)
— once a release requires `bevy_replicon ^0.42` or later, switch
`chemgame`'s `Cargo.toml` back to the real crates.io dependency and delete
this directory. Worth opening a PR upstream with the same one-line bump
rather than only fixing it locally.
