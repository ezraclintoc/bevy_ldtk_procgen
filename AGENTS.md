# AGENTS.md

Guidance for AI coding agents working in this repository.

## Read `DESIGN.md` and `ARCHITECTURE.md` first

`DESIGN.md` is the contract with a consumer of the generator: inputs, outputs,
guarantees. `ARCHITECTURE.md` is the implementation map: module layout, concrete
types, the placement algorithm. Neither describes existing code —
**nothing in either is implemented yet**. Where this file disagrees with either
of them about intended behaviour, they win and this file is the thing to fix.

## What this is

`bevy_ldtk_procgen` is a **library crate** (Bevy 0.18 / `bevy_ecs_ldtk` 0.14) that
procedurally connects LDtk levels into a dungeon at runtime. There are no `[[bin]]`
targets.

**Status: pre-alpha, mid-rewrite.** The previous implementation has been deleted. The
crate currently compiles, has zero tests, and does nothing.

## Current state of the tree

Do not trust older commits, docs, or your own memory about what exists. As of this
branch (`rewrite/generator`):

| Thing | State |
|---|---|
| `src/lib.rs` | **Empty file.** The entire public API is unwritten. |
| `src/world/` | **Deleted** — `mod.rs`, `pipeline.rs`, `types.rs`, `spatial_hash.rs`, `debug.rs`, `tests.rs` |
| `examples/` | **Deleted** — `dungeon.rs`, `pan_camera.rs`, `player_collision.rs`, `common/mod.rs` |
| Tests | None. `cargo test` passes vacuously. |
| Clippy | Clean, except `missing_docs` on the empty crate root. |
| `nix build` | **Broken.** `cargoBuildFlags` still says `--example dungeon`, which no longer exists. Fix the flake when an example returns. |

Do not recreate the `src/world/` module tree by reflex. The old layout was built around
`WorldState` and a `RoomType` enum, both of which `DESIGN.md` removes. The rewrite's
module layout **is** decided — `src/generator/` (pure) and `src/plugin/` (Bevy) — see
`ARCHITECTURE.md` §1. Neither directory exists yet; create them as the code lands rather
than restoring `src/world/`.

Names that no longer mean anything: `WorldState`, `RoomType`, `WorldPlugin::debug`,
`find_bridging_room`, `poll_task`, `regenerate_on_key`.

## Vocabulary

From `DESIGN.md` §1. These five terms are load-bearing; do not invent synonyms.

| Term | Meaning |
|---|---|
| **Catalog** | Every room definition parsed from the `.ldtk`, plus derived indices. Immutable after load. |
| **RoomDef** | One authored room: size, doors, tags, weight. Lives in the Catalog. |
| **Layout** | The record of which rooms were placed where. Append-only. The generator's output. |
| **Placement** | One entry in the Layout: a `RoomId` plus an origin. `Copy`. |
| **Anchor** | Entities carrying `GenerationAnchor`. Generation follows them. |

## Invariants

These are the mistakes that are expensive to discover late. Each one is specified in
`DESIGN.md`; they are restated here because they are easy to violate while writing
otherwise-reasonable code.

- **The Layout is truth; spawned entities are a cache over it.** Culling despawns
  entities and must never mutate the Layout. If it does, doors reopen, revisited areas
  regenerate differently, and the seed stops reproducing.
- **Never iterate a `HashMap` during placement.** Iteration order varies run to run and
  silently destroys determinism *even with a fixed seed*. Direction-keyed indices are a
  4-element array indexed by discriminant.
- **Never hardcode a tile size.** Read `gridSize` from the LDtk project. The old
  implementation inferred door direction with `if entity.width != 16`, which is the
  direct reason a 512px-grid project could not load. The repo now ships an 18px tileset
  specifically so that assumption cannot hide.
- **`RoomId` is a newtype over `u32`, constructible only by the Catalog.** Not a bare
  `usize` — it must not be confusable with a door index or a placement index. Because
  IDs cannot be forged, catalog lookup is provably in-bounds, which is what removes the
  indexing panic *by construction* rather than by scattering `.get()` calls.
- **Placements hold a `RoomId`, never a cloned `RoomDef`.** The definition lives exactly
  once, in the Catalog.
- **There are no reserved tags, of either kind.** Bool tags come from one project-wide
  `LocalEnum.RoomTags` (checkboxes, absent = `false`); float tags are named `Float`
  fields (`danger`, `loot`, ...; absent = `0.0`). Both are interned to a small integer
  (`TagId` / `FloatTagId`) at catalog build — never `String`-keyed at runtime, and never
  looked up by name inside the placement loop. Do not add engine meaning to any tag
  name, including `boss`, `shop`, `corridor` — see `DESIGN.md` §2.
- **There is no separate `weight` field.** A room's base selection weight is its
  `weight` float tag (absent → `1.0`, the one deliberate exception to float tags'
  usual `0.0` default).
- **`PlacementCounters::tag_counts` is a `Vec`, not a `HashMap`.** This is the entire
  reason tags are interned (previous two bullets) — a `TagId` is an array index.
- **Every error variant names the offending level**, and `CatalogError` is
  `#[non_exhaustive]`. "Invalid room" without a name is not actionable.
- **No implicit camera fallback for the anchor**, and **no runtime keybindings** in the
  library. The old code registered an unconditional `R`-to-regenerate handler, stealing
  that key from every consumer.

### Generation is synchronous

**This reverses the previous architecture.** Placement runs on the main thread inside a
per-frame time budget (`DESIGN.md` §5). It is *not* on `AsyncComputeTaskPool`.

If you find guidance anywhere — an old commit message, a stale comment, an earlier
version of this file — saying generation must stay off the main thread, that guidance is
obsolete. Measured cost was 39–48 µs per room, roughly flat to 1000 rooms; the frame-time
problem was `bevy_ecs_ldtk` spawning tilemap entities, which is throttled separately.

Going synchronous is what lets meta-generation read live world state with no snapshotting
and no `Send + Sync` bounds, and makes determinism trivial instead of something to defend.

## Environment

NixOS host. The flake provides everything; do not expect system-wide Rust to match.

```bash
nix develop          # cargo, rustc, rust-analyzer, clippy, rustfmt, prek, typos
```

Bevy `dlopen`s its graphics, input, and audio backends, so the devShell exports
`LD_LIBRARY_PATH` covering `vulkan-loader`, `libGL`, `wayland`, `libxkbcommon`, X11, and
`alsa-lib`. **Running a windowed example outside `nix develop` fails at window creation,
not at compile time.** If a graphics or audio backend fails to load, check that you are
in the devShell before touching any code.

`src` in the flake filters out `target`, `result`, and `.direnv` so the build does not
copy a multi-gigabyte target directory into the Nix store.

## Commands

```bash
cargo test                                    # headless, no GPU needed
cargo clippy --all-targets --all-features
cargo fmt --all
prek run --all-files                          # typos + fmt + clippy
prek install                                  # one-time git hook install
```

## Lint policy

This is the part most likely to surprise you.

Lint levels live in `Cargo.toml` under `[lints.rust]` / `[lints.clippy]`, and apply to
every target in the package — lib, examples, and tests alike. The clippy pre-commit hook
deliberately does **not** pass `-D warnings`; the deny/warn split in `Cargo.toml` is the
policy, so `deny` blocks a commit and `warn` does not.

**`deny` means: this crate must not panic.** A library has no business aborting its
host's process. Denied: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
`unreachable`, `exit`, `indexing_slicing`, `panic_in_result_fn`, `unwrap_in_result`.

Rules for agents:

- **Never add an `.unwrap()`, `.expect()`, `panic!`, or bare `a[i]` index to non-test
  code.** Use `let ... else`, `?`, `.get()`, `.first()`, or return an `Option`/`Result`.
- **Never silence a deny lint with `#[allow]` to make a build pass.** If a panic is
  genuinely unavoidable, say so and ask; do not decide unilaterally.
- `clippy.toml` already relaxes the panic lints inside `#[cfg(test)]` and `#[test]`.
  Tests may unwrap freely — do not add `#[allow]` attributes there.

`warn`-level lints cover cast lossiness (`cast_possible_truncation`,
`cast_precision_loss`, `cast_sign_loss`, `cast_lossless`), `float_cmp`, and public-API
docs (`missing_docs`, `missing_panics_doc`, `doc_markdown`, `unreachable_pub`).

The lint config is a design constraint, not decoration: the error taxonomy in
`DESIGN.md` §6 exists because every panic the old implementation had was a missing error
variant. `.expect("No spawn room found.")` became `NoStartRoomMatched`.

Clippy is currently clean. A red run means you broke it.

## Assets

| File | Grid | Committed? |
|---|---|---|
| `assets/kenney_tiny-dungeon_16px.png` | 16px | Yes — CC0 |
| `assets/kenney_pixel-platformer_18px.png` | 18px | Yes — CC0 |
| `assets/maaot_mossy-cavern_512px.png` | 512px | **No** — license forbids redistribution |
| `assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk` | 16px | Yes |
| `assets/ezraclintoc_maaot-mossy-cavern_512px_variabledoors.ldtk` | 512px | **No** — references the ungitted PNG above; kept as a local fixture only |

Rules:

- **Check the licence before committing any art.** CC0 only. Several popular itch.io
  packs (Maaot, Elthen) permit commercial use but forbid redistribution, which a public
  git repo does by definition. If an asset is not CC0, add it to `.gitignore` and
  document a download step in the README instead.
- The three grid sizes are deliberate test coverage. 16 and 18 are both committed
  because 18 is not a power of two and shares no factor structure with 16, so it flushes
  out hardcoded `16`s and `>> 4`s that an 8px or 32px tileset would pass straight
  through. 512px is the extreme case, available locally only.
- Naming convention: `<author>_<pack>_<gridsize>px.png` for tilesets,
  `<author>_<pack>_<gridsize>px_<variant>.ldtk` for projects.

## Conventions

- Commit messages: Conventional Commits, `type(scope): Description`. Types in use:
  `feat`, `fix`, `chore`. (Older commits are inconsistent about the colon; new ones
  should include it.)
- `typos` runs over all text files and the repo is currently clean; keep it that way.
- **Adding a room is an LDtk-editor task, not a code task.** Never add hardcoded room
  definitions to Rust. This is the project's central promise — see `DESIGN.md` §10,
  "Not a level editor."
- `DESIGN.md` has a **Deferred** section (bounded mode, global invariants, door order,
  spatial bounds, hot-reload) and a **Resolved during design** section (the reasoning
  behind decisions already made, so it isn't re-derived mid-implementation). If your
  work depends on something in Deferred, raise it — it is deferred, not decided.
