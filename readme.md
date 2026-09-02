# bevy_ldtk_procgen

[![Nightly Build](https://github.com/ezraclintoc/bevy_ldtk_procgen/actions/workflows/nightly.yaml/badge.svg)](https://github.com/ezraclintoc/bevy_ldtk_procgen/actions/workflows/nightly.yaml)

A procedural, room-by-room level generator for Bevy and LDtk (`bevy_ecs_ldtk`). Author a
catalog of rooms as LDtk levels, mark their doors, and this plugin randomly connects
them into a non-linear layout at runtime - no hand-authored dungeon required.

Not published to crates.io yet, but the crate itself is a real library - add
`bevy_ldtk_procgen::WorldPlugin` (or `use bevy_ldtk_procgen::prelude::*;`) to your own
Bevy app via a git dependency, or clone this repo and run the bundled demo. See
"Library vs. demo" below.

## Features

- Completely Procedural: generates unique, non-linear layouts dynamically at runtime from a catalog of LDtk rooms.

- Asynchronous Generation: room placement runs in serialized batches on Bevy's `AsyncComputeTaskPool`, so it never blocks the main/render thread, and chains multiple rooms deep per batch instead of one door at a time.

- Spatial Hashing: room-vs-room and room-vs-door collision checks are grid-backed, so placement cost stays close to flat as the dungeon grows instead of degrading with total room count.

- Frame-Rate-Limited Spawning: newly generated rooms are drained into the world a few at a time per frame, so a big batch of new rooms doesn't spike frame time. This can make generation *look* slower than it is - a batch is usually fully placed in the background well before every room in it has visibly appeared. Enable debug mode (see Controls) to see this directly: room-bounds gizmos read placement data straight from `WorldState`, so they appear immediately for rooms that haven't visually spawned yet.

- Easy LDtk Content Creation: designing and adding new room templates happens entirely inside the LDtk editor - no code changes needed to add a room.

## Bevy compatibility

| bevy_ldtk_procgen | Bevy | bevy_ecs_ldtk |
|-------------------|------|---------------|
| (unreleased)      | 0.18 | 0.14          |

## Getting Started

Ensure you're on the latest stable Rust toolchain, clone the repository, and run the
bundled demo:

```bash
cargo run --example dungeon --release
```

> Note: Nightly binaries are also automatically built and available for download under
> the GitHub Actions tab if you want to try an executable demo.

## Bring your own LDtk file

Point the plugin at your own project instead of the bundled example rooms:

```rust
app.add_plugins(WorldPlugin {
    ldtk_path: "my_dungeon.ldtk".into(),
});
```

Your `.ldtk` file needs to follow a few conventions for the generator to be able to
parse and connect your rooms - these aren't currently validated at load time, so a
mismatch will silently misplace or fail to connect rooms rather than error clearly:

- **One level per room.** Each LDtk level is treated as a single placeable room.
- **Door entities, named exactly `Door`,** on an `Entities` layer, placed flush against
  whichever edge(s) of the level should be connectable. Direction is inferred
  automatically from the door's shape and position: a door exactly 1 tile (16px) wide
  is treated as a side door - West if it sits flush with the level's left edge, East
  otherwise; a door wider than 1 tile is treated as a top/bottom door - North if flush
  with the top edge, South otherwise.
- **Every door must be the same width.** The current door-centering math assumes a
  uniform door size across the whole project - a mixed-width project will misalign.
- **Two level fields, on every level:** `weight` (Float) - relative probability this
  room is chosen when multiple candidates fit a given door; and `room_type` (Enum, one
  of `Spawn` / `Room` / `Hallway`). Exactly one `Spawn`-type level is required - that's
  where generation starts.
- **16px tiles, currently required.** Door and room-size math throughout the generator
  assumes a 16px LDtk grid; this isn't yet configurable (tracked in Future Improvements).

## Controls

- WASD — Move the camera around the world.

- Scroll Wheel — Zoom the camera in and out.

- R — Refresh / Regenerate a completely new map layout instantly.

Debug overlays are off by default. Enable them with:

```bash
cargo run --example dungeon --release -- --debug
```

The `--` before `--debug` (or `-d`) is required - without it, cargo tries to parse
`--debug` as its own argument instead of forwarding it to the game. With it on, you
should see: small green circles at every open door, translucent green room-bounds
rectangles, green door-clearance boxes, and a faint white grid centered on the camera
once zoomed in - if none of that appears, the flag isn't reaching the binary (check for
the missing `--` first).

## Library vs. demo

This repo builds as a library (`src/lib.rs`), not a single-executable game - the
generator itself is what's published, and the game you get with `cargo run --example
dungeon` is just the demo built on top of it. The public API is intentionally small:
`WorldPlugin` (add it to your `App`, pointed at your own `.ldtk` file) and, for reading
back what got generated, `WorldState`/`Room`/`Door`/`RoomDef`/`DoorDef`/`Dir`/
`RoomType`/`GenerationState` - available directly (`bevy_ldtk_procgen::WorldPlugin`) or
via `use bevy_ldtk_procgen::prelude::*;`. Everything else (the placement algorithm,
spatial hashing, async batching internals) is a private implementation detail, not part
of the API - you don't call it directly, you just add `WorldPlugin` and read
`WorldState` back.

Not yet published to crates.io, so today "using this" still means a git dependency or
cloning the repo rather than `cargo add bevy_ldtk_procgen` - see Future Improvements.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option - the standard convention for Rust crates.

## Asset Credits

- Tileset: [Tiny Dungeon](https://kenney.nl/assets/tiny-dungeon) by [Kenney](https://kenney.nl) (`assets/kenney_tiny-dungeon_16px.png`), licensed [CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/).

## Current Status & Known Bugs

### Status

- Just finished Spatial Hashing and multi-room-per-batch branch chaining.

### Known Bugs

- Regenerate Hitch: Pressing R to regenerate freezes for a moment, longer the more rooms are currently placed. `regenerate_on_key` recursively despawns every placed room's entity tree (tiles, colliders, LDtk sub-entities) in a single frame with no throttling - the same class of cost the spawn side already got rate-limited for, but never applied to teardown.

## Future Improvements

While the core generation layout logic is running, here are the planned roadmap items to optimize and expand the system:

- Publish to crates.io, so this can be used with `cargo add` instead of a git dependency.

- Configurable tile size: LDtk grid/tile size is currently hardcoded to 16px throughout the placement math.

- Save & Load System: Implement serialization and deserialization to allow players to save generated dungeon layouts and reload them later.

- Room Culling / Unloading: Despawn or freeze LDtk levels that are too far outside the camera's viewport to optimize GPU memory and collision performance.

- Derive Door Clearance Sizes: Door bounding boxes (the clearance a room reserves beyond each door) are currently hand-picked constants that happen to match the smallest single-door room for that direction in the LDtk project. Derive them from the room catalog instead, so they can't silently drift out of sync if a smaller or larger single-door room is ever added. And make spatial hash query offset be tied to the smallest room.

- Derive Cell size from average or mean room size.

## Tech Stack

- Engine: Bevy Engine
- Level Editor: LDtk (Level Designer Toolkit)
- Crates Used: bevy_ecs_ldtk, rand
