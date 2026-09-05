# bevy_ldtk_procgen

[![Nightly Build](https://github.com/ezraclintoc/bevy_ldtk_procgen/actions/workflows/nightly.yaml/badge.svg)](https://github.com/ezraclintoc/bevy_ldtk_procgen/actions/workflows/nightly.yaml)
[![Status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-red)](https://github.com/ezraclintoc/bevy_ldtk_procgen)

A procedural, room-by-room level generator for Bevy and LDtk (`bevy_ecs_ldtk`). Author a
catalog of rooms as LDtk levels, mark their doors, and the plugin connects them into a
non-linear layout at runtime — no hand-authored dungeon required.

> [!WARNING]
> **Pre-alpha.** Public API changes without notice.

## Compatibility

| `bevy_ldtk_procgen` | `bevy` | `bevy_ecs_ldtk` |
| ------------------- | ------ | --------------- |
| unreleased          | `0.18` | `0.14`          |

## Features

### Implemented

_Nothing yet — the generator is being rewritten from scratch on this branch._

### Planned

- Procedural Room Generation
- Asynchronous Generation
- Spatial Hashing
- Support for variable door widths
- Culling
- Saving and loading
- Deterministic Seeding

## Installation

Not published to crates.io yet, so add it as a git dependency:

```toml
[dependencies]
bevy_ldtk_procgen = { git = "https://github.com/ezraclintoc/bevy_ldtk_procgen" }
```

Pin a commit while the crate is pre-alpha, since `main` moves without regard for
breaking changes:

```toml
bevy_ldtk_procgen = { git = "https://github.com/ezraclintoc/bevy_ldtk_procgen", rev = "<commit sha>" }
```

Bevy needs system graphics, input, and audio libraries present at runtime. On NixOS the
flake in this repo provides them:

```bash
nix develop
```

### Bundled assets

`assets/kenney_tiny-dungeon_16px.png` is CC0 and ships with the repo. The Mossy Cavern
tileset is not redistributable, so it is **not** in this repository — the matching
`.ldtk` map is, but it will fail to load until you supply the image yourself:

1. Download [Mossy Cavern](https://maaot.itch.io/mossy-cavern) by Maaot (name your own
   price, free is fine).
2. Place the 512px tileset at `assets/maaot_mossy-cavern_512px.png`.

The path matters — `assets/ezraclintoc_maaot-mossy-cavern_512px_variabledoors.ldtk`
refers to it by that exact filename.

## Contributing

Issues and pull requests are welcome. The repo is set up to be built from the flake:

```bash
nix develop          # cargo, rustc, rust-analyzer, clippy, rustfmt, prek, typos
prek install         # install the pre-commit hooks
```

Before opening a pull request:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test
```

Notes for contributors:

- The lint policy in `Cargo.toml` denies `unwrap`, `expect`, `panic`, `todo`,
  `unimplemented`, and direct indexing. Return errors instead.
- Commit messages follow Conventional Commits (`feat(maps): ...`, `chore(tooling): ...`).
- `AGENTS.md` documents the crate layout and the LDtk authoring conventions.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option — the standard convention for Rust crates.

Licenses apply to the source code only. Bundled art assets carry their own terms, listed below.

## Asset Credits

- Tilesets:
  - [Tiny Dungeon](https://kenney.nl/assets/tiny-dungeon) by [Kenney](https://kenney.nl) — `assets/kenney_tiny-dungeon_16px.png`, licensed [CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/).
  - [Pixel Platformer](https://kenney.nl/assets/pixel-platformer) by [Kenney](https://kenney.nl) — `assets/kenney_pixel-platformer_18px.png`, licensed [CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/). Used as the 18px test catalog, to keep tile-size assumptions honest.
  - [Mossy Cavern](https://maaot.itch.io/mossy-cavern) by [Maaot](https://maaot.itch.io/) — **not bundled**, download it yourself (see [Bundled assets](#bundled-assets)). Free and commercial use permitted, modification permitted, credit appreciated but not required; redistribution, repackaging, and resale are not permitted, modified or otherwise — which is why the image is not committed here.
