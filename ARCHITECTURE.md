# bevy_ldtk_procgen — Architecture

**Status: pre-implementation**, same as `DESIGN.md`. This is the map for whoever
writes the code: module layout, concrete types, and the placement algorithm.
`DESIGN.md` is the contract with a consumer — what goes in, what comes out, what
is guaranteed. This document is how that contract gets built. Where the two
disagree, `DESIGN.md` wins and this file is the thing to fix.

---

## 1. Module layout

One crate (`DESIGN.md`, "Resolved during design"). The core/Bevy boundary is a
convention enforced by review, not the compiler:

```
src/
  lib.rs              public API surface, re-exports, crate docs
  generator/          pure — imports no bevy, no bevy_ecs_ldtk
    error.rs          CatalogError, CatalogErrors
    geom.rs           TilePos, TileRect, Dir
    catalog.rs        Catalog, RoomDef, RoomId, TagId, FloatTagId, parse + validate
    layout.rs         Layout, Placement, PlacementId, OpenDoor
    spatial.rs        SpatialHash
    weight.rs         WeightConfig, PlacementCounters, sampling
    place.rs          the placement loop (§4)
  plugin/             bevy layer
    mod.rs            WorldPlugin, GenerationSet, resource/event registration
    load.rs           LdtkProject -> generator's neutral input type, GenerationState
    systems.rs        generate, sync_entities
    events.rs         RoomPlaced, RoomSpawned, RoomDespawned, DoorAbandoned, GenerationFailed
```

Two naming collisions to avoid on purpose: `gen` is a reserved keyword as of
edition 2024 (this crate's edition), so the pure module is `generator`, not
`gen`. `mod bevy` would shadow the crate name `bevy`, so the plugin module is
`plugin`, not `bevy`.

`generator/` parses its own minimal LDtk shape (levels, layers, entity and field
instances) rather than depending on `bevy_ecs_ldtk::ldtk::LdtkJson`, which pulls
in Bevy. `plugin/load.rs` is the only place that touches `LdtkProject`; it
converts into the generator's neutral type before handing off. This is what
keeps `cargo test -p generator-only-tests` (or equivalent `#[cfg(test)]`
modules under `generator/`) compiling without Bevy at all, which is what made
the old `tests.rs` pattern — parse the `.ldtk`, run generation, no `App`, no
GPU — fast, and is worth preserving deliberately rather than by accident.

If headless compile time or reuse outside Bevy ever becomes a real problem,
`generator/` is already shaped to lift into its own crate with no internal
changes — only `Cargo.toml` and re-exports move.

---

## 2. Units and coordinate conventions

Both mistakes the old code made, stated as rules so they can't creep back in.

**Tiles everywhere in `generator/`; pixels only at the plugin boundary.**
`grid_size: u32` (px per tile) is read once from the LDtk project and appears in
exactly two places in the whole codebase: the parser (px → tiles, in
`plugin/load.rs`) and the spawn system (tiles → px, in `plugin/systems.rs`). Any
other appearance of a tile-size constant, hardcoded or otherwise, is the bug the
old `if entity.width != 16` was.

```rust
pub struct RoomDef {
    pub size: UVec2,              // TILES
    pub doors: Vec<DoorDef>,      // door.local_pos in TILES
    ...
    pub world_offset_px: Vec2,    // LDtk world position — integration only,
}                                  // never read by generator/place.rs
```

`world_offset_px` exists only because `bevy_ecs_ldtk`'s
`LevelSpawnBehavior::UseWorldTranslation` positions levels by it. It is plumbed
straight from parse to spawn and must never enter placement math.

**y-up in `generator/`; the flip happens once, at parse.** LDtk is y-down, Bevy
is y-up. The old code mixed conventions — `world_pos` was treated as y-up while
`rects_collide_tl` computed `bottom = top - height` (y-down) — which is why it
had three different collision helpers with three different sign conventions.
`plugin/load.rs` flips every level and door position on the way in; nothing
under `generator/` ever reasons in y-down terms.

---

## 3. Core types

```rust
pub struct RoomId(u32);          // index into Catalog::rooms, mint-only-by-Catalog
pub struct PlacementId(u32);     // index into Layout::placements

pub struct TilePos { pub x: i32, pub y: i32 }
pub struct TileRect { pub min: TilePos, pub size: UVec2 }

#[repr(u8)]
pub enum Dir { N = 0, E = 1, S = 2, W = 3 }
```

**`Dir` ordering is clockwise, not compass-alphabetical**, specifically so
`opposite()` is `(self as u8 + 2) & 3` and left/right turns are `+1`/`-1` mod 4,
and so `[Vec<RoomId>; 4]` indexed by `Dir as usize` replaces the old
`HashMap<Dir, Vec<usize>>` — iterating that map was a live reproducibility
hazard (`DESIGN.md` §7), not just a style complaint.

**Collision is `TileRect`, one type, integer tiles, min-corner + size:**

```rust
pub fn overlaps(a: TileRect, b: TileRect) -> bool {
    a.min.x < b.max().x && b.min.x < a.max().x &&
    a.min.y < b.max().y && b.min.y < a.max().y
}
```

This replaces `rects_collide` / `rects_collide_tl` / `rects_collide_center` —
three conventions (center-based, top-left y-down, center-based-again) for one
concept. `Placement.origin` already *is* the min corner, so no conversion is
needed at any call site. Integer math end to end means no `float_cmp` exposure
and no epsilon debates. Strict `<` means touching edges — two rooms sharing a
wall — do not count as overlapping.

**`RoomId` and `PlacementId` are constructible only by their owning
collection.** `Catalog::get(RoomId) -> Option<&RoomDef>` is the only way to
dereference one; because the ID can't be forged, the lookup is provably
in-bounds. `.get()` still returns `Option`, not `&RoomDef` directly, because
`indexing_slicing` is denied (`AGENTS.md`) — but every call site holding a
`RoomId` obtained one honestly, from the Catalog, so the `None` branch there is
genuinely unreachable rather than a checked-just-in-case path. That's the
"panic disappears by construction" claim in `DESIGN.md` §1 made concrete.

**`Placement` and `OpenDoor` — index, don't duplicate:**

```rust
pub struct Placement { pub room: RoomId, pub origin: TilePos }        // Copy

pub struct OpenDoor { pub placement: PlacementId, pub door: u8 }       // Copy
```

`OpenDoor` stores no position. It is derived on demand:

```rust
fn open_door_pos(layout: &Layout, catalog: &Catalog, d: OpenDoor) -> Option<TilePos> {
    let p = layout.get(d.placement)?;
    let def = catalog.get(p.room)?;
    let door = def.doors.get(d.door as usize)?;
    Some(p.origin + door.local_pos)
}
```

Two integer adds. This is what removes the old float-position door-equality
comparison (`d.world_pos == door.world_pos`, flagged by `float_cmp`) entirely —
there is no float to compare, doors are identified by `(PlacementId, u8)`.

One trap already hit once: the old `open_doors: Vec<Door>` used `swap_remove` on
close, which invalidates indices into it — the reason the old spatial hash
covered only `rooms`, never doors, and door lookup stayed an O(n) linear scan.
If doors are to be spatially indexed (§5 says yes), `OpenDoor` needs either a
stable slot map (freed slots tombstoned, not swap-removed) or the spatial index
needs to be rebuilt for doors each time the open set changes shape. Slot map is
cheaper; take that unless a reason not to turns up during implementation.

---

## 4. The placement loop

Resolves `DESIGN.md`'s "Resolved during design" entry on the bridging fallback.
The old bug: `find_bridging_room` tried exactly one candidate and gave up,
which stalled generation after a handful of rooms and forced the test suite's
`MIN_EXPECTED_ROOMS` down to 3.

Per door, within the current frame's budget (`DESIGN.md` §5):

```
1. pick the next open door (nearest-to-anchor order, per DESIGN.md's
   locality invariant — every door within generation_radius before any
   door outside it)
2. candidates = catalog rooms with a door facing opposite(dir), width match
   -> if candidates is empty: this door cannot ever be filled by this
      catalog; count as one exhausted attempt immediately, go to 5
3. weights = weight(catalog, ctx, c) for c in candidates   (weight before
   try_place: weighting is a handful of array reads and multiplies,
   try_place is a spatial query — cheaper check first)
4. loop:
     pick <- sample(candidates, weights)          # weighted random draw
     if try_place(pick, door):                     # spatial overlap check
         commit: add to Layout, close `door`, open pick's other doors
         break
     else:
         remove pick from candidates (and its weight)
         if candidates empty: break                # pool exhausted
5. if the door was not filled this pass:
     door.failed_attempts += 1
     if door.failed_attempts >= DEAD_DOOR_THRESHOLD:
         remove from open set, emit DoorAbandoned
     # not retried again until nearby geometry changes (DESIGN.md, "Dead doors")
```

Step 4's "remove and resample" is the entire fix. The old code's single-try
version is step 4 with the `if candidates empty: break` branch taken
immediately after one failure instead of after the pool is actually exhausted.

**Multi-door candidates** (`DESIGN.md` §9, "multi-door preference") are folded
into step 3's weight, not handled as a separate branch: computing
`closes_doors` for a candidate is a point-query per door on that candidate
against the current open-door set (§5 — this is exactly why doors are
spatially indexed, not just rooms). On commit, *every* now-satisfied open door
the placed room closes is removed, not only the one step 1 was filling.

**Step ordering (weight before try_place) matters for cost, not correctness.**
Weighting a candidate is array reads and float multiplies against
`PlacementCounters`; `try_place` is a spatial-hash query plus rectangle overlap
tests against everything nearby. If profiling ever shows the ratio reversed for
a particular catalog shape (e.g. extremely dense placement where most
candidates fail geometrically), swapping the order is a `place.rs`-local
change, not a design change — `DESIGN.md` does not commit to which check runs
first, only that both happen.

---

## 5. Spatial index

Uniform grid, keyed by cell coordinate, storing both rooms and doors — the old
implementation indexed only rooms (because `open_doors` used `swap_remove` and
so couldn't safely hold stable indices; see §3). Indexing doors is what makes
`closes_doors` (§4) a spatial query instead of a scan over every open door in
the level.

**Cell size:** default is the largest room dimension present in the catalog,
computed once at catalog build, not a constant. This bounds any room's rect to
at most four cells regardless of grid size, and is why the old
`DEFAULT_CELL_SIZE = 128.0` — a magic number that happened to be 8 tiles at
16px and a nonsensical 0.25 tiles at 512px — could not have survived contact
with the 512px catalog. Overridable, since a catalog with one huge outlier room
and many small ones may want a smaller cell size despite that outlier.

`insert`/`query` floor-divide a rect's bounds by cell size and enumerate every
cell the rect touches (multi-cell for anything larger than one cell); `query`
dedupes results. This part of the old design (`spatial_hash.rs`) was sound and
carries forward unchanged in shape.

---

## 6. Door clearance from the catalog, not constants

The old `Door::get_bounding_box` hardcoded per-direction clearance boxes
(`Dir::N => Vec2::new(64.0, 48.0)`, `Dir::S => Vec2::new(64.0, 32.0)`, etc.),
with a comment admitting N and S differ for tilemap-specific reasons nobody
re-derived for a different tileset.

Replacement: compute, once at catalog build, the smallest known room clearance
for each `(Dir, width)` pair actually present in the catalog — the minimum
depth any room in the catalog needs behind a door of that direction and width.
Store as a lookup:

```rust
min_clearance: HashMap<(Dir, u8), UVec2>   // built once, read-only after —
                                            // not a placement-time HashMap,
                                            // so §7's hazard does not apply
```

(Read-only after construction and never iterated during placement — only
looked up by exact key — so this does not reintroduce the iteration-order
hazard `DESIGN.md` §7 warns about; it is functionally a fixed lookup table that
happens to be sparse.)

A door's bounding box becomes a lookup instead of a hardcoded case split, and
adapts automatically to any grid size or catalog shape, including the 512px
project this design is explicitly validated against.

---

## 7. Systems and ordering

Only one `SystemSet` needs to be public — the seam a consumer's weight system
hooks into (`DESIGN.md` §9):

```rust
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub enum GenerationSet { Weights }
```

Internally:

```
GenerationSet::Weights   (consumer's system(s), if any, run here)
        |
        v
generate                  reads RoomWeights + anchors, runs the §4 loop,
                           mutates Layout. No Commands, no entity queries —
                           this is the thin ECS wrapper directly over
                           generator::place, kept side-effect-free on purpose
                           so it stays trivially testable.
        |
        v
sync_entities              drains the spawn queue and the cull queue against
                           the (now-updated) Layout. All Commands and entity
                           access live here. Spawning and culling are one
                           system, not two: both are throttled queue drains
                           over an unchanged Layout, and Bevy would serialize
                           them against each other on Layout access regardless
                           of whether they're split, so splitting buys no
                           parallelism — only two systems to keep in sync.
```

`generate` and `sync_entities` are chained privately; nothing about their
names or count is public API, so either can be split or merged again later
without breaking a consumer. Only `GenerationSet::Weights` is a promise.

Without this set, a consumer's weight-writing system and `generate` would have
an undeclared conflicting access to `RoomWeights` (`ResMut` vs `Res`), which
Bevy resolves by running them in an unspecified, possibly frame-to-frame
inconsistent order — silently reintroducing exactly the kind of untracked
nondeterminism `DESIGN.md` §7 is about, just at the scheduling layer instead of
in a `HashMap`.

---

## 8. Testing strategy

Carried forward from the old `tests.rs` deliberately, not by default: parse a
real `.ldtk` file via a plain `serde_json`-backed reader (§1 — this is exactly
why `generator/` doesn't depend on `bevy_ecs_ldtk`'s types), run the placement
loop with no Bevy `App`, no `AssetServer`, no GPU. `cargo test` on `generator/`
should need nothing beyond the two bundled 16px/18px catalogs and should run in
well under a second.

The 18px Kenney tileset (`AGENTS.md`, Assets) exists specifically as a second
grid size with no factor relationship to 16 — it flushes out a hardcoded `16`
or a `>> 4` that a 32px tileset (a clean multiple) would pass straight through
undetected. Any new geometry code lands a test against both catalogs before it
lands against neither.

**Performance baseline**, carried forward from the pre-rewrite benchmark log
(`git show origin/feature/room-culling:spec.md`) as the regression floor for
the new implementation, not a target — the new type layout (§3) should beat
these, not merely match them:

| Rooms | Prior implementation, µs/room |
|---|---|
| 100 | 39.22 |
| 250 | 41.25 |
| 500 | 44.47 |
| 1000 | 48.50 |

Worth re-establishing this table for the rewrite once `place.rs` exists, using
the same "average over seeds that actually reach the target, exclude stalls"
methodology — a stall is a correctness bug, and averaging it in would make a
correctness regression look like a speed regression.

---

## 9. Known traps, not to repeat

Each of these was a real bug in the deleted implementation. Listed together
because they're easy to reintroduce independently while implementing pieces
that individually look correct.

- `MAX_ROOMS_PER_FRAME = 1000` sized a *batch cap* so large (~48 ms of work) that
  it looked like it needed a task pool to avoid hitching. It didn't — see
  `DESIGN.md` §5. A budget in time, not room count, cannot repeat this mistake
  by construction.
- Door layers matched by `layer.identifier != "Entities"` — the layer *name*,
  not its type. A project with a door layer called anything else silently
  parsed zero doors. Match on layer type.
- `regenerate_on_key` registered an unconditional `R` handler in the plugin
  itself, stealing that key from every consumer. Regeneration is an event; a
  keybinding is an example's problem, never the library's.
- Direction inferred from `entity.width != 16` — see §2. The 512px catalog
  exists specifically so this can't silently pass.
- `open_doors` used `swap_remove`, which is why doors were never in the spatial
  index (§3, §5). Use a slot map or equivalent stable-index structure for
  anything the spatial index needs to reference after the fact.
