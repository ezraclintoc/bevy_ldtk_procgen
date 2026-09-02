# AGENTS.md

Guidance for AI coding agents working in this repository.

## What this is

`bevy_ldtk_procgen` is a **library crate** (Bevy 0.18 / `bevy_ecs_ldtk` 0.14) that
procedurally connects LDtk levels into a dungeon at runtime. There are no `[[bin]]`
targets. The runnable demo is `examples/dungeon.rs`.

Public API surface is small and deliberate — `WorldPlugin` plus the types re-exported
from `src/lib.rs`'s `prelude`. Everything else in `src/world/` is internal even where
it is marked `pub`, because `mod world;` is private.

## Layout

| Path | Role |
|---|---|
| `src/lib.rs` | Public surface: `WorldPlugin` + `prelude` |
| `src/world/mod.rs` | Plugin definition, system registration, re-exports |
| `src/world/pipeline.rs` | Generation algorithm: room indexing, batch placement, door matching |
| `src/world/types.rs` | `Room`, `RoomDef`, `Door`, `Dir`, `WorldState`, rect collision helpers |
| `src/world/spatial_hash.rs` | Grid-backed broad-phase for room/door collision queries |
| `src/world/debug.rs` | Gizmo overlays, gated on `WorldPlugin::debug` |
| `src/world/tests.rs` | Headless tests: parse `assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk` via `serde_json`, run generation with no Bevy `App` |
| `assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk` | Room catalog consumed by the demo and the tests |

## Environment

NixOS host. The flake provides everything; do not expect system-wide Rust to match.

```bash
nix develop          # cargo, rustc, rust-analyzer, clippy, rustfmt, prek, typos
nix build            # builds the dungeon example, wrapped with assets + library path
nix run              # runs it
```

Bevy `dlopen`s its graphics, input, and audio backends, so the devShell exports
`LD_LIBRARY_PATH` covering `vulkan-loader`, `libGL`, `wayland`, `libxkbcommon`, X11, and
`alsa-lib`. **Running `cargo run` outside `nix develop` will fail at window creation, not
at compile time.** If a graphics or audio backend fails to load, check that you are in
the devShell before touching any code.

The flake's `packages.default` needs a `postInstall` because `buildRustPackage`'s install
hook finds no binaries in a library crate; it installs the example, copies `assets/`, and
wraps with `BEVY_ASSET_ROOT`. `src` filters out `target`, `result`, and `.direnv` so the
build does not copy a multi-gigabyte target directory into the Nix store.

## Commands

```bash
cargo test                                    # headless, no GPU needed
cargo run --example dungeon --release
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

### Known pre-existing violations

`cargo clippy` currently reports **7 deny-level errors, all in
`src/world/pipeline.rs`** (lines ~202, ~216, ~244, ~258, ~365, ~366, ~394): two
`unwrap`/`expect` calls and five panicking index expressions. These predate the lint
config and are **not yet fixed**.

Consequence: the clippy hook fails on a clean checkout. Do not "fix" this by weakening
`Cargo.toml`. Either fix the call sites properly or leave them alone — but never treat a
red clippy run as evidence that your own change broke something without checking whether
the failure is one of these seven.

There are also ~155 warn-level findings, dominated by `missing_docs` (~44) and
`unreachable_pub` (~24). The `unreachable_pub` cluster is one structural fact, not 24
mistakes: items are `pub` inside the private `world` module without being re-exported.

## Conventions

- Commit messages: `type(scope) Description` — see `git log`. Types in use: `feat`,
  `fix`, `chore`.
- `typos` runs over all text files and the repo is currently clean; keep it that way.
- Generation must stay off the main thread. Placement runs in batches on
  `AsyncComputeTaskPool` and rooms are drained into the world at a capped rate per frame.
  Do not move placement work into a synchronous system.
- Adding a room is an LDtk-editor task, not a code task. Do not add hardcoded room
  definitions to Rust.
