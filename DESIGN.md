# bevy_ldtk_procgen — Design

**Status: pre-implementation.** This document specifies intended behaviour, not
existing behaviour. The generator is being rewritten from scratch; this is the
target it is being written against. Nothing here is implemented yet.

**Scope: streaming generation only.** The dungeon has no predetermined end.
Generation follows an anchor entity, filling doors near it as the anchor moves;
rooms behind may be culled. There is no point at which a layout is "complete".

Bounded generation — a fixed-size dungeon generated once and then finished — is
deferred, not rejected. Several features depend on it and are listed under
[Deferred](#deferred) rather than specified here, because each one is unsound
without a completion point.

Every decision reached during design is recorded either inline or under
[Resolved during design](#resolved-during-design), rather than left to be
re-derived from prose. There is no separate "open questions" list at the moment
— everything raised so far has a resolution; new ones go here as they surface.

---

## 1. Vocabulary

These five terms are load-bearing. Nothing in the codebase should invent a
synonym for any of them.

| Term | Meaning | Mutability |
|---|---|---|
| **Catalog** | Every room definition parsed from the `.ldtk`, plus derived indices. | Immutable after load |
| **RoomDef** | One authored room: size, doors, bool tags, float tags. Lives in the Catalog. | Immutable |
| **Layout** | The record of which rooms were placed where. The generator's output. | Append-only |
| **Placement** | One entry in the Layout: a `RoomId` plus an origin. | `Copy` |
| **Anchor** | Entities carrying `GenerationAnchor`. Generation follows them. | Read each frame |

A `Placement` holds a `RoomId` **instead of** a `RoomDef`, not alongside one:

```rust
struct RoomId(u32);
struct Placement { room: RoomId, origin: TilePos }   // Copy, ~12 bytes
```

The definition lives exactly once, in the Catalog. Forty-four authored rooms
placed five hundred times means forty-four `RoomDef`s, not five hundred clones of
`String` and `Vec<DoorDef>`. The Layout becomes a flat `Vec<Placement>`, "same
room?" is an integer compare, and editing a definition changes every placement of
it at once.

`RoomId` is a newtype over `u32` — not a bare `usize`, so it cannot be confused
with a door index or a placement index — and is constructible only by the
Catalog. Because IDs cannot be forged, catalog lookup is provably in-bounds and
the out-of-range indexing panic disappears by construction rather than by
sprinkling `.get()` calls to satisfy a lint.

The tax is that most functions need both halves: `fn f(catalog: &Catalog, layout:
&Layout)`. That is the standard cost of a handle/arena design and is accepted
deliberately.

The critical distinction is **Layout vs. spawned entities**. The Layout is
truth: append-only, never culled, fully determined by the seed. Spawned Bevy
entities are a *cache* over the Layout — created and destroyed freely by
culling, and carrying no information the Layout doesn't already have.

Anything that breaks that rule breaks reproducibility. Culling must never
mutate the Layout.

---

## 2. Authoring contract

What a `.ldtk` project must satisfy to load. Every rule here corresponds to an
error variant in §6 — if a rule can be broken, it must be detectable at load
time and reported with the offending level named.

### Project level

- **Uniform grid size.** Every layer in the project shares one `gridSize`. The
  generator reads it from the project; it is never assumed. Mixed grid sizes are
  an error, not a warning.
- **One level per room.** Levels are the unit of placement.
- **Levels are tile-aligned.** Each level's pixel dimensions are exact multiples
  of the grid size.

### Per level

- **A door layer.** An Entities layer containing entities named `Door`, each
  placed flush against the level edge it connects through. Direction is derived
  from which edge the entity touches — computed from the level bounds and the
  project grid size, never from a hardcoded tile width. Door width comes from the
  entity's extent along that edge.
- **Tags** (**optional**, two kinds — see below).

**At least one single-door room per direction.** A dungeon closes by capping
open doors with dead ends; a direction with no cap room has doors that can never
close. Warned at load, not discovered later as a dungeon that never finishes.

**No collision layer is required.** The generator never reads one. Room-vs-room
placement is rectangle-based on level bounds, so an IntGrid wall layer is
invisible to it. Your game almost certainly wants one for physics — that is
between you and `bevy_ecs_ldtk`'s `register_ldtk_int_cell_for_layer`, and the
generator stays ignorant of it deliberately.

Door layers are matched by layer **type**, not by name. The previous
implementation tested `layer.identifier != "Entities"`, so a project whose door
layer was called `Objects` parsed zero doors and reported no error.

### Why doors are entities

Alternatives considered: an IntGrid value inside the wall layer, a separate
IntGrid door layer, or deriving doors from gaps in the perimeter wall. The last
three are all tile-aligned by construction, which is a genuine advantage.

Entities win on one point that outweighs it: **doors are the feature most likely
to grow fields.** Locked, one-way, requires-key, size class, tags for
meta-generation to weight against. An IntGrid cell holds an integer and nothing
else. Deriving from wall gaps holds nothing at all, and additionally removes the
author's ability to have an interior gap that is *not* a connection point.

The problems previously blamed on this choice were not caused by it:

1. Direction was inferred with `if entity.width != 16` — a hardcoded tile size,
   and the direct reason a 512px-grid project cannot load. Fixed by deriving from
   level bounds.
2. Nothing validated that a door sat on an edge, so a door one tile inside
   silently produced a room that never connected. Now `DoorNotOnEdge`.
3. Doors were identified by float position equality. An unfilled door is now a
   `RoomId` plus a door index into that room's definition, with world position
   derived on demand rather than stored — never compared as a float.

An explicit `direction` field on the Door entity was rejected: it is redundant
with position and so introduces a failure mode where the two disagree. Infer,
then validate.

### Tags

`RoomType` is removed. It was a closed Rust enum parsed from a level field,
which meant adding a room category required editing Rust — contradicting the
project's central promise that authoring happens entirely in LDtk.

**There are no reserved tags.** The library assigns no meaning to any tag name.
`boss`, `treasure`, `corridor` — none of them trigger generator behaviour on
their own. They are *data on rooms*; meta-generation (§9) is what reads them,
and consumers read them directly for gameplay ("is the player in a `shop`
room?").

There are two kinds, matching what LDtk can actually express on a level:

- **Bool tags** — `Array<LocalEnum.RoomTags>`, one project-wide enum, shown in
  the editor as checkboxes. A room's tags are whichever values are present in
  the array; **absent means `false`**. Multi-select is native — a room can
  carry any number of these at once.
- **Float tags** — one named `Float` field per tag (`danger: Float`, `loot:
  Float`, ...), declared once on the level definition like any other field.
  **Absent means `0.0`.**

Both are interned, not string-keyed at runtime. LDtk fixes the full set of each
kind at the project level — the `RoomTags` enum's values, and the level's `Float`
field definitions — so the catalog resolves every tag to a small integer once,
at build time:

```rust
pub struct TagId(u16);       // ordinal into the RoomTags enum
pub struct FloatTagId(u16);  // ordinal into the project's named Float fields
```

which is what lets `PlacementCounters::tag_counts` (§9) be a flat array instead
of a `HashMap`.

Name resolution and value lookup are two separate steps, on two different
types — a Catalog holds many rooms, so "the value of tag X" is only meaningful
per room; the Catalog can only resolve a *name* to an *id*:

```rust
impl Catalog {
    pub fn tag_id(&self, name: &str) -> Option<TagId>;
    pub fn float_tag_id(&self, name: &str) -> Option<FloatTagId>;
}
impl RoomDef {
    pub fn bool_tag(&self, id: TagId) -> bool;
    pub fn float_tag(&self, id: FloatTagId) -> f32;
}
```

Resolution is a **setup-time** lookup — call `Catalog::tag_id`/`float_tag_id`
once (plugin build, or the first time a system needs it) and hold the returned
id from then on. Looking up by string inside the placement loop reopens the
same cost and hazard interning exists to avoid; there is deliberately no
combined by-name-straight-to-value convenience method, since that would invite
doing exactly that.

**A generic string-keyed tag bag was considered and rejected.** It cannot be
interned — the tag set would only be known at runtime, forcing a `HashMap`
lookup back into the hot path and reopening the iteration-order hazard this
design otherwise avoids everywhere else (§7).

There is no separate `weight` field in the authoring contract. A room's base
selection weight is just its `weight` float tag (absent → 1.0 is the one
deliberate exception to the general 0.0 default, since a room with no stated
weight should be ordinary, not unselectable), read through the same mechanism
as every other float tag rather than a bespoke schema entry.

### Starting room

The generator does not require a designated spawn room. Requiring one would mean
a five-room test project fails to load until something is tagged.

```rust
pub enum StartRoom {
    Any,             // default: weighted pick from the whole catalog
    Tagged(String),
    Named(String),   // level identifier
}
```

`NoStartRoomMatched` is therefore *conditional* — an error only when the developer asked
for a constraint that nothing satisfies.

---

## 3. Inputs

Everything a developer configures.

| Input | Type | Default | Notes |
|---|---|---|---|
| `project` | `Handle<LdtkProject>` | — | A handle, not a path. Lets the consumer control loading and preloading. (Hot-reload is [deferred](#deferred); the handle-based API doesn't foreclose it.) |
| `seed` | `u64` | random | Explicit parameter. Never read from the environment. |
| `anchor` | `GenerationAnchor` component | — | Generation follows entities carrying it. See below. |
| `budget` | `GenerationBudget` | `TimeBudget(500µs)` | How much work per frame. See §5. |
| `max_rooms` | `Option<(usize, CountBy)>` | `None` | Ceiling, not a target. See §9. |
| `generation_radius` | `f32` | — | How far ahead of the anchor to fill doors. |
| `culling` | `CullingPolicy` | `Radius` | See §8. |
| `start` | `StartRoom` | `Any` | Which room generation begins from. See §2. |
| `validation` | `Strict` \| `Lenient` | `Strict` | See §6. |

The seed deserves emphasis: reproducing a bad layout is the highest-value
debugging affordance the library can offer, and it only exists if the seed is a
first-class parameter.

### Anchor

```rust
#[derive(Component)]
pub struct GenerationAnchor;
```

The plugin queries `(&GlobalTransform, With<GenerationAnchor>)` each frame. The
consumer attaches it to a player, a camera, or a scripted focus point.

There is deliberately **no implicit camera fallback** — magic that works until
someone has two cameras. The quickstart spends one line on it:
`commands.spawn((Camera2d, GenerationAnchor))`.

- **Zero anchors** — generation idles. Not an error: it is the normal state
  during a loading screen, or before the player entity exists.
- **Multiple anchors** — union. Doors near *any* anchor are eligible, which is
  the obvious semantics for split-screen. Anchors are iterated in a
  position-sorted order, never in `Entity` order, because entity IDs depend on
  spawn order and are reused (see §7).

---

## 4. Outputs

The current implementation has essentially no output API — a consumer cannot ask
what was generated. That is the largest gap being closed.

### State

`GenerationState`: `Loading → Building → Ready → Failed`.

### Resources

- `Catalog` — behind an `Arc`, immutable, cheap to share.
- `Layout` — placed rooms, their bounds and tags, and the door connectivity
  graph.

### Queries

- **Point → room.** "Which room contains this position?" Consumers need this
  constantly: which room is the player in, has it been cleared, what music
  plays. Backed by the spatial index that already exists for placement.
- **Connectivity graph.** Rooms and their door links, for minimaps, pathfinding,
  locked doors, and boss placement.

### Events

- `RoomPlaced { room: RoomId, at: TilePos }` — Layout grew.
- `RoomSpawned { room: RoomId, entity: Entity }` — entities now exist.
- `RoomDespawned { room: RoomId }` — culled; the Layout is unchanged.
- `GenerationFailed(CatalogErrors)`.
- `DoorAbandoned { at: TilePos, dir: Dir }` — a door was marked dead after
  repeated placement failure; wall it off. See §9.
- `GenerationSettled` — no reachable open doors remain near the anchor.

`RoomSpawned` is the single most important extension point in the library. It
is how a consumer attaches enemies, loot, and triggers to a generated room. The
bundled `.ldtk` already defines `Chest` and `GhostSpawn` entities that nothing
currently consumes — that gap is this event's reason for existing.

---

## 5. Generation is synchronous

Generation runs on the main thread inside a per-frame time budget. It is not
moved to a task pool.

Measured cost from the pre-rewrite benchmark log is 39–48 µs per room, roughly
flat from 100 to 1000 rooms once spatial hashing landed. A 16.67 ms frame fits
roughly 340 rooms; a budget of 500 µs places about ten. The math was never the
frame-time problem.

The problem was **entity spawning** — `bevy_ecs_ldtk` populating tilemaps — which
is throttled separately and independently of generation.

Going synchronous buys:

- **Meta-generation can read live world state** (§9) with no snapshotting and no
  `Send + Sync` bounds on the weight source.
- No deep clone of generator state per batch.
- No frame of latency between deciding and placing.
- Reproducing a bad seed becomes trivial instead of something to defend (§7).

### Budget modes

```rust
pub enum GenerationBudget {
    Immediate,               // everything within radius, this frame
    RoomsPerFrame(usize),
    TimeBudget(Duration),    // default: 500 µs
}
```

All three are legitimate: `Immediate` for loading screens, small dungeons, and
tests; `RoomsPerFrame` for predictability; `TimeBudget` for streaming, because a
room-count cap guesses at hardware while a time budget states the intent.

**Guarantee: the budget affects timing, never the result.** Door-fill order is
determined by the algorithm and the seed, not by how many rooms are placed per
frame, so all three modes converge on the same layout. This makes `Immediate`
usable in tests to assert what the streaming configuration will eventually
produce.

---

## 6. Errors

Catalog construction parses and validates in one pass:

```rust
pub fn build_catalog(project: &LdtkJson) -> Result<Catalog, CatalogErrors>
```

Two properties matter:

**It returns the Catalog, not `()`.** If it validated, the work is done; a
`Result<(), _>` throws that away and forces a second pass.

**It collects every error, not the first.** A designer fixing twelve malformed
rooms one rebuild at a time is a bad experience. `CatalogErrors` wraps a `Vec`.

```rust
#[non_exhaustive]
pub enum CatalogError {
    EmptyCatalog,
    NoStartRoomMatched { constraint: String },
    MixedGridSize     { level: String, expected: i32, found: i32 },
    LevelNotTileAligned { level: String },
    DoorLayerMissing  { level: String },
    DoorNotOnEdge     { level: String, at: IVec2 },
    DuplicateLevelIid { iid: String },
    NoCapRoomForDirection { dir: Dir },   // warning under Lenient
}
```

`#[non_exhaustive]` is required — without it, adding a variant is a breaking
change.

Every variant names the offending level. "Invalid room" without a name is not
actionable.

`Strict` fails the load on any error. `Lenient` drops the offending rooms and
proceeds, still reporting them. Strict is the default: a silent partial load is
how you end up asking why the dungeon only uses three of forty rooms.

Most panics in the old implementation were a missing error variant.
`NoStartRoomMatched` replaces an `.expect("No spawn room found.")` directly —
though under `StartRoom::Any` that case no longer arises at all.

---

## 7. Reproducibility

No determinism guarantee is made. Meta-generation (§9) is explicitly designed to
read live external state — player level, quest flags — so the same seed can
produce different layouts depending on when that state changed. Documenting a
guarantee that meta-generation immediately breaks would be dishonest.

What is still worth having, as an engineering practice rather than a promise: the
placement algorithm itself introduces no *extra* randomness beyond the RNG draws
and whatever the weight table says. Concretely, placement code never iterates a
`HashMap` — any map iterated mid-placement makes output vary run to run even with
external state held fixed, which is a bug (an untracked source of randomness),
not a feature. Direction-keyed indices use a 4-element array indexed by
discriminant instead.

Practical upshot: with meta-generation disabled and a fixed seed, a run should
reproduce. That is what makes a bad seed reportable and debuggable. It is not
extended into a guarantee once external state is in play.

---

## 8. Culling

Culling is in the library, because the part that is easy to get wrong is the
part that must not be got wrong: despawning a room must not touch the Layout. If
it does, doors reopen, revisited areas regenerate differently, and the seed stops
reproducing.

The *policy*, however, is game-specific:

```rust
pub enum CullingPolicy {
    Disabled,      // consumer drives spawn/despawn via events
    Radius(f32),   // default, with hysteresis to avoid boundary flicker
}
```

Default on, escape hatch available.

---

## 9. Meta-generation

Room selection weights vary with context instead of being fixed per room. The
factors split cleanly by how often they change, and that split decides the API.

| Factor | Changes | Owner |
|---|---|---|
| Player level, difficulty, quest state, biome | per frame | consumer |
| `rooms_placed`, `depth`, previous room, tag counts | per placement | engine |

Final weight for a candidate:

```
final = room_weights[room] * builtin_adjust(ctx, room)
```

### Consumer side — one system, once per frame

External factors change at most once per frame, so they are computed once per
frame by an ordinary Bevy system that returns a full weight table, piped into an
engine system that stores it:

```rust
fn my_weights(catalog: Res<Catalog>, player: Res<PlayerLevel>) -> Vec<f32> {
    catalog.iter()
        .map(|def| def.float_tag(WEIGHT) * if def.bool_tag(BOSS) {
            player.level as f32
        } else {
            1.0
        })
        .collect()
}

app.add_systems(
    Update,
    my_weights.pipe(bevy_ldtk_procgen::apply_weights).in_set(GenerationSet::Weights),
);
```

`f32`, not `f64` — every other value in the pipeline already is one, and mixing
widths would force a cast at each boundary for no precision that matters here.

Output index `i` corresponds to `RoomId(i)`; `apply_weights` stores it in a
`RoomWeights` resource for `generate` to read. No trait, no `fn` pointer, no
boxing, no generic plugin parameter — full ECS access on the consumer's side,
and expensive work is affordable at once per frame.

Returning a **complete** table rather than mutating one in place is deliberate:
it means there is nothing to reset between frames. A consumer who does not care
about a room simply passes its base weight (`def.float_tag(WEIGHT)`) straight
through; there is no way to leave a stale override in place by only touching
some entries, because every entry is written every time.

A length mismatch against `catalog.len()` cannot be a panic (`indexing_slicing`
is denied, §6's lint policy) — `apply_weights` treats a short table as base
weight for the missing tail and logs a warning; it never indexes past the
shorter of the two.

### Engine side — per placement

Per-placement factors cannot live in a per-frame table. A frame places on the
order of ten rooms, and a table has no memory between them: a quota of three
shops would be exceeded within a single frame, because every placement would read
the same pre-quota weight.

These are therefore engine-applied, driven by data:

```rust
#[derive(Resource)]
pub struct WeightConfig {
    pub ceiling:            Option<(usize, CountBy)>,   // enables closure bias
    pub tag_max:            Vec<(String, usize)>,       // ("shop", 3)
    pub tag_min_depth:      Vec<(String, u32)>,         // ("boss", 20)
    pub tag_transition:     Option<TransitionTable>,
    pub prefer_multi_door:  bool,                       // default true, see below
}

pub enum CountBy {
    All,            // every placement, hallways included
    Tag(String),    // only rooms carrying this tag
}
```

Tag names in `WeightConfig` are resolved to `TagId`/`FloatTagId` once, against
the loaded Catalog, when the config takes effect — not looked up by string
inside the placement loop.

`CountBy` matters more than it looks. A hallway is a placement, so counting
everything conflates "50 rooms" with "50 things". The bundled catalog is 35
hallway templates out of 44, so a target of 50 under `CountBy::All` yields
roughly ten actual rooms and closes the dungeon far too early. Closure bias must
measure progress against the thing the developer meant.

### Placement counters

Maintained incrementally by the engine, readable by every built-in:

```rust
pub struct PlacementCounters {
    pub placed_total:        usize,
    pub tag_counts:          Vec<usize>,          // one slot per TagId, not a HashMap
    pub rooms_since_tag:     Vec<usize>,          // same indexing; "rest room every 10"
    pub template_uses:       Vec<u16>,             // per-template quota, indexed by RoomId
    pub recent_templates:    [Option<RoomId>; 4],  // anti-repeat window
    pub distance_from_start: f32,                  // shops near, boss far
}
```

`tag_counts` and `rooms_since_tag` are sized to the catalog's `TagId` space at
construction and indexed directly by ordinal — the whole reason tags are
interned (§2) rather than `String`-keyed is so this stays a flat `Vec`.

`tag_counts` is load-bearing for `tag_max` and `CountBy::Tag`; the rest exist so
per-tag cooldowns, per-template quotas, anti-repetition, and distance-based
weighting are expressible without a consumer-side hook.

- **closure bias** — weights by door count against remaining room budget.
  Prefers high-door-count rooms while far from the `ceiling`, single-door caps
  on approach. Primary mechanism for landing near a room count.
- **`tag_max`** — hard exclusion past a quota. Exact, because it is re-evaluated
  every placement.
- **`tag_min_depth`** — hard gate below a depth.
- **`tag_transition`** — per-tag matrix over the previously placed room, making
  `corridor -> room` likely and `room -> room` rare. Cheap, and does more for
  texture than anything else here.
- **multi-door preference** — a candidate that would close more than one
  currently-open door gets a bonus proportional to the count closed, when
  `prefer_multi_door` is set (default on). This is engine-side, not
  consumer-side: how many open doors a specific candidate would close is a
  property of one placement decision, evaluated fresh at every door — it has no
  meaningful value in a once-per-frame table computed before any of that
  frame's doors have been chosen. Multi-door connections are otherwise merely
  *allowed*, never required — a candidate that closes only the door being
  filled is always a valid placement.

Direction is not a weighting concern: candidates are filtered by door direction
and width before weights are consulted.

Sampling builds a cumulative range table over the filtered candidate set at each
placement. Having the consumer return ranges rather than weights would buy
nothing, since that table has to be rebuilt per door regardless.

### Weights cannot guarantee anything

Three independent ways a required room fails to appear. Weighting touches one.

1. **Never a candidate.** A room is eligible only at an open door whose
   direction and width its own door matches. If the frontier never produces one,
   weight is irrelevant.
2. **Never selected.** Weights are relative and each placement is independent.
3. **Never fits.** Selected, then rejected for overlapping existing geometry.

Under streaming there is no completion point, so there is nowhere to check an
invariant and nothing to retry. **Streaming offers no hard guarantees** —
"exactly one boss room" is not a meaningful statement about a dungeon that never
ends. `tag_max` gives *at most*; nothing gives *at least*.

Consumers needing a guarantee should treat it as a gameplay concern: check for
the room, and if the run has gone long enough without one, force it via the
per-frame weight table.

### Ceiling and graceful closure

`max_rooms` is a **ceiling, not a target**. On reaching it, generation stops —
but doors left open at the boundary are permanent visible gaps.

`closure_bias` exists for this: as the placed count approaches the ceiling, it
weights toward low-door-count rooms and finally single-door caps, so the frontier
shrinks instead of being cut off mid-expansion.

`CountBy` matters here. A hallway is a placement, so counting everything
conflates "500 rooms" with "500 things". The bundled catalog is 35 hallway
templates out of 44, so a ceiling of 500 under `CountBy::All` yields roughly a
hundred actual rooms and begins closing far too early.

### Dead doors

A door inside the generation radius that cannot be filled — no cap room for its
direction, or geometrically blocked on every attempt — otherwise stays open
forever. That is both a permanent visible gap and a source of wasted work on
every batch, the same failure class as the old `find_bridging_room` stall.

Each attempt to fill a door already exhausts its full eligible candidate pool
(weight, sample, try, remove-and-resample on failure — see "Resolved during
design") before counting as one failure. After N such exhausted attempts the
door is marked dead: removed from the open set and reported via
`DoorAbandoned { at, dir }` so the consumer can wall it off. It is not
reattempted every subsequent batch regardless — only when nearby geometry
changes — so a permanently blocked door costs one sweep, not one per frame
forever.

## 10. Non-goals

- **Not a level editor.** Authoring happens in LDtk.
- **No gameplay.** No enemies, loot, doors-as-mechanics. `RoomSpawned` is the
  seam; what you hang off it is yours.
- **No physics or collision response.** The wall IntGrid is exposed; colliders
  are the consumer's.
- **No save/load of generated layouts.** A seed plus a catalog reproduces a
  layout; serialising the result is out of scope.
- **No multi-threaded generation** (§5).
- **No graph-first generation.** Guaranteed topology — locked-door missions,
  deliberate cycles — is a different architecture (Dormans' cyclic generation,
  Nepozitek's Edgar), not a patch on greedy door-filling. Worth noting that the
  catalog would port directly: Edgar's "room template plus door positions" model
  is already what an LDtk level with `Door` entities is.
- **No runtime keybindings.** The old implementation registered an unconditional
  `R`-to-regenerate handler, stealing that key from every consumer. Regeneration
  is a debug affordance, triggered by an event the consumer chooses to send, and
  bound to a key only in examples.

---

## Deferred

Not rejected — unsound without bounded generation, so specified later alongside
it.

- **Bounded mode.** Fixed-size dungeon, generated once, completes.
- **Global invariants.** "Exactly one boss room." Requires a completion point at
  which to check, and a re-roll on failure.
- **Door selection order.** Which open door to fill next is what controls dungeon
  *shape* — breadth-first sprawls, depth-first snakes. Under streaming, locality
  is an invariant rather than a preference: every door within the generation
  radius must be filled before any door outside it, or the anchor outruns the
  dungeon and reaches a visible hole. That leaves door order as a tiebreak within
  the eligible set, where it has almost no effect. Shape control needs bounded
  mode to mean anything.
- **Spatial bounds.** "Fits within 100x100 tiles."
- **Hot-reload.** Editing the `.ldtk` at runtime rebuilding the catalog and
  regenerating live, rather than requiring a restart. Independent of bounded
  mode — deferred because it is not yet needed, not because anything blocks it.
  Worth adding if a consumer asks for it; nothing in this design forecloses it.

## Resolved during design

Recorded so the reasoning is not re-derived later.

- **Crate split.** One crate. The core/Bevy boundary is a module convention
  (`src/generator/` vs `src/plugin/`, see `ARCHITECTURE.md`), not a compiler
  boundary. Revisit only if headless test compile time becomes a real problem —
  splitting later is mechanical; unsplitting a published two-crate API is not.
- **Room-level tag storage.** Interned, not `String`. See "Tags" in §2 — bool
  tags intern against the `RoomTags` enum, float tags against the project's
  named `Float` field definitions. Both closed sets, known at catalog build.
- **Multi-door connections.** Allowed, never required, and preferred by default
  via an engine-side bonus (§9) proportional to doors closed. Not exposed to
  the consumer's per-frame table — see "multi-door preference" in §9 for why.
- **Bridging fallback.** The replacement strategy is: weight the full eligible
  candidate pool, sample, attempt placement; on failure, remove that candidate
  and resample from what remains. Only once the pool is exhausted does the
  door's failure counter increment (§9, "Dead doors"). The old bug was trying
  exactly one candidate with no resample step. Full algorithm in
  `ARCHITECTURE.md`.
