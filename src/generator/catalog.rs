//! `Catalog`, `RoomDef`, `DoorDef`, `RoomId`, `TagId`, `FloatTagId`, and
//! parsing plus validation against the authoring contract. See DESIGN.md §2
//! and §6, ARCHITECTURE.md §1 and §3.
//!
//! `build_catalog` takes `&LdtkJson` directly rather than a hand-rolled
//! mirror type: `bevy_ecs_ldtk` is already an unconditional dependency of
//! this crate (`plugin/` needs it), so there is no compile-time isolation to
//! buy by avoiding it here, and re-deriving LDtk's own JSON schema would
//! only risk getting it wrong. `LdtkJson`'s fields are plain `i32`/`String`/
//! `Vec<_>` — no Bevy math or asset types — so nothing here leaks into the
//! zero-glam types (`TilePos`, `TileSize`) the rest of `generator/` uses.
//!
//! This function embodies "Strict" validation (DESIGN.md §6): any problem
//! found means `Err`. A "Lenient" wrapper that drops offending rooms and
//! proceeds is a separate, not-yet-written layer on top of this one.
//! `NoStartRoomMatched` is not checked here — it depends on `StartRoom`
//! config, which is a placement-time concern, not a catalog-build one.

use std::collections::{HashMap, HashSet};

use bevy_ecs_ldtk::ldtk::{
    EntityInstance, FieldValue, LayerInstance, LdtkJson, Level, Type as LdtkLayerType,
};

use super::error::CatalogErrors;
use super::geom::{Dir, PixelPos, RoomId, TilePos, TileRect, TileSize};

/// Everything that can make a catalog fail to build, or generation to fail to
/// start. This enum is the replacement for the old implementation's panics
/// (`DESIGN.md` §6): every case that *has* a name in the project names it,
/// because "invalid room" without a name is not actionable.
///
/// `#[non_exhaustive]`: a consumer matching on this gets a compile error when
/// a variant is added, rather than silently mishandling a case it never saw.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CatalogError {
    /// The project produced no rooms at all — no levels, or levels whose
    /// `layerInstances` are absent (LDtk's separate-level-files mode, which
    /// the authoring contract does not cover yet).
    EmptyCatalog,
    /// `StartRoom` asked for a constraint nothing satisfies. Conditional by
    /// design (`DESIGN.md` §2): silence unless a constraint was requested.
    NoStartRoomMatched {
        /// The constraint as it was asked for, so the message says *what*
        /// had no match rather than just that something didn't.
        constraint: String,
    },
    /// A layer's grid disagrees with the project grid — every door on that
    /// layer would be placed at the wrong pitch.
    MixedGridSize {
        /// The offending level.
        level: String,
        /// Project-wide grid size, in pixels.
        expected: i32,
        /// The layer's own grid size, in pixels.
        found: i32,
    },
    /// A level's size isn't a whole number of tiles, so its footprint has no
    /// exact integer representation and rooms would not tile.
    LevelNotTileAligned {
        /// The offending level.
        level: String,
    },
    /// The level has no entity layer, so there is no layer doors could
    /// possibly be on.
    DoorLayerMissing {
        /// The offending level.
        level: String,
    },
    /// Door detection from IntGrid gaps (`DoorSource::IntGridGaps`) found an
    /// empty corner cell. A corner belongs to two edges at once, so the
    /// gap's direction is ambiguous; wall the corner off in the editor.
    DoorAtCorner {
        /// The offending level.
        level: String,
        /// The corner cell, in LDtk's y-down frame.
        at: TilePos,
    },
    /// A `Door` entity touches no level edge. Direction comes from the edge
    /// it sits on (`DESIGN.md` §2), so a door that touches none cannot be
    /// oriented — and inferring it from the entity's size is exactly the old
    /// `entity.width != 16` bug (`AGENTS.md`, invariants).
    DoorNotOnEdge {
        /// The offending level.
        level: String,
        /// The door's grid position, in LDtk's y-down frame.
        at: TilePos,
    },
    /// Two levels share an `iid`. Instance handles are `LevelSet`s of iids,
    /// so a duplicate makes one of the two unaddressable.
    DuplicateLevelIid {
        /// The repeated iid.
        iid: String,
    },
    /// No single-door room faces this direction, so a corridor run in it can
    /// never be terminated cleanly. Warning under `Lenient`, not necessarily
    /// fatal under `Strict`.
    NoCapRoomForDirection {
        /// The direction with no cap room.
        dir: Dir,
    },
    /// Grid size must be a positive integer — zero would make every
    /// tile-alignment check divide by zero, which the crate's deny-panic
    /// policy (`AGENTS.md`, lint policy) rules out as a code path entirely,
    /// so this is checked and reported instead of ever being reached.
    InvalidGridSize {
        /// What the project declared — zero or negative, either of which would
        /// make every tile-alignment check divide by zero.
        found: i32,
    },
    /// Not a `build_catalog` output — the `.ldtk` asset itself failed to
    /// load (missing file, I/O error), before there was any JSON to
    /// validate. Folded into this enum anyway rather than inventing a
    /// second failure type: `GenerationFailed(CatalogErrors)` is the one
    /// failure surface DESIGN.md §4 defines, and this still needs to reach
    /// it.
    AssetLoadFailed {
        /// The asset system's own error, verbatim — it knows more about why
        /// the load failed than this crate can.
        message: String,
    },
}

/// Where a room's doors come from.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DoorSource {
    /// `Door` entities flush against the level edge, on any Entities layer.
    /// Doors can carry fields this way, and an author can leave a gap in a
    /// wall that isn't a connection.
    #[default]
    Entities,
    /// Every gap in the level's outer ring of an `IntGrid` layer is a door:
    /// each maximal run of empty (`0`) cells along one edge, corners
    /// excluded, is one door that wide. Any nonzero value is wall. No
    /// `Door` entities needed, and doors can't drift out of sync with the
    /// walls. `layer` is the IntGrid layer's identifier.
    IntGridGaps {
        /// The `IntGrid` layer whose gaps are doors, e.g. `"WallGrid"`.
        layer: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DoorDef {
    pub local_pos: TilePos,
    pub dir: Dir,
    pub width: u8,
}

/// `pub(crate)` field, same reasoning as `RoomId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TagId(pub(crate) u16);

/// Interned float tag id — an index into each room's float tag vector, minted
/// only by the catalog. Like `TagId`, never a name: names are resolved once at
/// catalog build (`DESIGN.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FloatTagId(pub(crate) u16);

/// Tag values are stored densely, indexed by ordinal — not a `HashMap`.
#[derive(Debug, Clone)]
pub struct RoomDef {
    /// LDtk's instance id for the level — what an instance handle needs.
    pub iid: String,
    /// The level's LDtk identifier — what `StartRoom::Named` matches on
    /// (DESIGN.md §2, "Starting room").
    pub identifier: String,
    /// The level's size in TILES — the placement footprint.
    pub size: TileSize,
    /// Doors in LDtk's own order, which is the order `OpenDoor::door` indexes.
    pub doors: Vec<DoorDef>,
    /// Integration-only: read by `plugin/systems.rs`, never by placement code.
    pub world_offset_px: PixelPos,
    bool_tags: Vec<bool>,
    float_tags: Vec<f32>,
}

impl RoomDef {
    /// A bool tag's value; `false` for a tag this room doesn't carry (and for
    /// an id from a catalog that never declared it).
    pub fn bool_tag(&self, id: TagId) -> bool {
        self.bool_tags.get(id.0 as usize).copied().unwrap_or(false)
    }

    /// A float tag's value; `0.0` when absent — the one deliberate exception
    /// is a room's base selection weight, which defaults to `1.0` at the
    /// sampling site, not here (`DESIGN.md` §2).
    pub fn float_tag(&self, id: FloatTagId) -> f32 {
        self.float_tags.get(id.0 as usize).copied().unwrap_or(0.0)
    }

    /// Test-only constructor: builds a bare `RoomDef` with synthetic doors
    /// and tags, for weight/geometry tests that need a definition-shaped
    /// value without an LDtk level behind it. Not part of the public API —
    /// real definitions only come from `build_catalog` (authoring happens
    /// in LDtk, never in Rust, DESIGN.md §10).
    #[cfg(test)]
    pub(crate) fn test_def(
        doors: usize,
        door_dir: Dir,
        size: TileSize,
        bool_tags: Vec<bool>,
    ) -> RoomDef {
        RoomDef {
            iid: String::new(),
            identifier: String::new(),
            size,
            doors: (0..doors)
                .map(|_| DoorDef {
                    local_pos: TilePos { x: 0, y: 0 },
                    dir: door_dir,
                    width: 2,
                })
                .collect(),
            world_offset_px: PixelPos { x: 0.0, y: 0.0 },
            bool_tags,
            float_tags: Vec::new(),
        }
    }
}

/// Every room definition parsed from the project, plus the derived indices
/// placement reads (`DESIGN.md` §1). Immutable after load: nothing here is
/// ever mutated during generation, which is what makes a seed reproduce the
/// same dungeon regardless of what was visited or culled.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    rooms: Vec<RoomDef>,
    bool_tag_ids: HashMap<String, TagId>,
    float_tag_ids: HashMap<String, FloatTagId>,
    grid_size: u32,
    /// Largest room dimension in the catalog — the spatial hash's default
    /// cell size (ARCHITECTURE.md §5), computed once here rather than
    /// re-scanned by every caller that needs it.
    spatial_cell_size: u32,
    /// Per open-door `(direction, width)`: the footprint of the room reserved
    /// to fill it, as an offset from the door's position plus a size. Built
    /// once; only ever point-looked-up, never iterated during placement, so
    /// the `HashMap` here doesn't reintroduce §7's hazard.
    reservations: HashMap<(Dir, u8), TileRect>,
    /// Which rooms have a door facing each direction — a 4-element array
    /// indexed by discriminant, not `HashMap<Dir, _>`: iterating a `HashMap`
    /// during placement is a reproducibility hazard even with a fixed seed
    /// (`AGENTS.md`, invariants; DESIGN.md §7). A room with two doors facing
    /// the same direction appears once per bucket, not once per door — this
    /// answers "which rooms could fill a door facing this way", and
    /// weighting toward one twice for coincidentally having two same-facing
    /// doors would be a bug, not a feature.
    by_door_dir: [Vec<RoomId>; 4],
}

impl Catalog {
    /// The definition a `RoomId` refers to. `RoomId`s can only be minted here,
    /// so the `None` branch is unreachable for honestly-obtained ids — it
    /// exists because indexing is denied (`AGENTS.md`, lint policy), not
    /// because callers have to handle it.
    pub fn get(&self, id: RoomId) -> Option<&RoomDef> {
        self.rooms.get(id.0 as usize)
    }

    /// Rooms with at least one door facing `dir`. Filtered by *direction*
    /// only — a caller matching against a specific open door still needs to
    /// check width against that room's own `doors`, since one room's doors
    /// facing the same direction can differ in width.
    pub fn rooms_with_door_facing(&self, dir: Dir) -> &[RoomId] {
        self.by_door_dir
            .get(dir as usize)
            .map_or(&[], Vec::as_slice)
    }

    /// Resolve a bool tag name — setup-time only, never inside the placement
    /// loop (`DESIGN.md` §2). `None` means the project never declared it.
    pub fn tag_id(&self, name: &str) -> Option<TagId> {
        self.bool_tag_ids.get(name).copied()
    }

    /// Resolve a float tag name, same rule as [`Catalog::tag_id`].
    pub fn float_tag_id(&self, name: &str) -> Option<FloatTagId> {
        self.float_tag_ids.get(name).copied()
    }

    /// The space kept free beyond one open door so it can always be filled
    /// (`ARCHITECTURE.md` §6): the full footprint the door's *reserved room*
    /// would occupy if placed through it. The reserved room is the smallest
    /// cap (single-door room) that connects through a `(dir, width)` door,
    /// or — when no such cap exists — the smallest room of any door count
    /// that does.
    ///
    /// The whole footprint, not just the door's width: every connecting room
    /// is wider than its own door (it has walls either side), so protecting
    /// only the lanes straight out from the door lets a neighbour land beside
    /// them and seal the door anyway.
    ///
    /// Placement keeps every open door's reservation free of room bodies and
    /// disjoint from every other open door's reservation. With a cap as the
    /// reserved room, that makes every open door fillable for as long as it
    /// is open: the cap's body is exactly the reservation, and it has no
    /// other door whose own reservation could be blocked.
    ///
    /// `None` when no room in the catalog can connect through a
    /// `(dir, width)` door — such a door can never be filled by any
    /// candidate, so there is nothing to reserve.
    pub fn door_reservation(&self, door_pos: TilePos, dir: Dir, width: u8) -> Option<TileRect> {
        let r = self.reservations.get(&(dir, width)).copied()?;
        Some(TileRect {
            min: door_pos + r.min,
            size: r.size,
        })
    }

    /// Two of `def`'s own doors whose reservations overlap, as door indices —
    /// doors so close together that the rooms filling them would collide.
    /// Placement never lets two open doors' reservations overlap, so such a
    /// room can never be placed by `attempt_door`; and as the start room
    /// (placed unchecked) both doors could never be filled. `None` for a
    /// well-formed room. An authoring problem, reported by the plugin at
    /// load: move the doors apart in the editor.
    pub fn self_blocking_doors(&self, def: &RoomDef) -> Option<(usize, usize)> {
        let reserved: Vec<(usize, TileRect)> = def
            .doors
            .iter()
            .enumerate()
            .filter_map(|(i, d)| Some((i, self.door_reservation(d.local_pos, d.dir, d.width)?)))
            .collect();
        reserved.iter().enumerate().find_map(|(n, &(i, a))| {
            reserved
                .iter()
                .skip(n + 1)
                .find(|&&(_, b)| super::geom::overlaps(a, b))
                .map(|&(j, _)| (i, j))
        })
    }

    /// Pixels per tile, read once from the project — the only tile-size value
    /// in the crate, and the reason nothing has to assume 16 (`ARCHITECTURE.md`
    /// §2).
    pub fn grid_size(&self) -> u32 {
        self.grid_size
    }

    /// How many distinct bool tags the project defines — the `TagId` space
    /// every per-tag array (e.g. `PlacementCounters::tag_counts`) sizes
    /// itself to. Zero for projects predating the tags schema.
    pub fn tag_count(&self) -> usize {
        self.bool_tag_ids.len()
    }

    /// Bounds any room's rect to at most four spatial-hash cells regardless
    /// of grid size. See ARCHITECTURE.md §5.
    pub fn spatial_cell_size(&self) -> u32 {
        self.spatial_cell_size
    }

    /// How many rooms the catalog holds — the length a `RoomWeights` table
    /// should be, and the size of the `RoomId` space.
    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    /// Whether the catalog holds no rooms. Cannot happen for a catalog built
    /// successfully (`EmptyCatalog` is an error), so this exists for
    /// completeness of the `len`/`is_empty` pair.
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    /// Rooms in `RoomId` order — entry `i` is `RoomId(i)`, which is exactly the
    /// order a consumer weight table is indexed by (`DESIGN.md` §9).
    pub fn iter(&self) -> impl Iterator<Item = &RoomDef> {
        self.rooms.iter()
    }
}

/// [`build_catalog_with`] for `Door` entities — what every test fixture
/// uses. The plugin always goes through `build_catalog_with`.
#[cfg(test)]
pub(crate) fn build_catalog(project: &LdtkJson) -> Result<Catalog, CatalogErrors> {
    build_catalog_with(project, &DoorSource::Entities)
}

/// Parses and validates a whole LDtk project, with doors found the way
/// `doors` says. See this file's module doc for why `&LdtkJson` is the input
/// type, and for what "Strict" means here.
pub(crate) fn build_catalog_with(
    project: &LdtkJson,
    doors: &DoorSource,
) -> Result<Catalog, CatalogErrors> {
    let mut errors = Vec::new();

    let Some(grid_size) = find_grid_size(project) else {
        // Every level's layer_instances is None — either an empty project,
        // or LDtk's "separate level files" mode, which isn't supported yet
        // (DESIGN.md's authoring contract doesn't cover it).
        return Err(CatalogErrors(vec![CatalogError::EmptyCatalog]));
    };
    if grid_size <= 0 {
        return Err(CatalogErrors(vec![CatalogError::InvalidGridSize {
            found: grid_size,
        }]));
    }
    let Ok(grid_size_u32) = u32::try_from(grid_size) else {
        return Err(CatalogErrors(vec![CatalogError::InvalidGridSize {
            found: grid_size,
        }]));
    };

    let tags_field_id = find_tags_field_identifier(project);
    let bool_tag_names = find_room_tags_enum_values(project);
    let float_tag_names = find_float_tag_field_names(project);

    let mut rooms = Vec::new();
    let mut seen_iids = HashSet::new();

    for level in &project.levels {
        let (room, mut room_errors) = build_room(
            level,
            doors,
            grid_size,
            tags_field_id.as_deref(),
            &bool_tag_names,
            &float_tag_names,
        );
        errors.append(&mut room_errors);

        if !seen_iids.insert(room.iid.clone()) {
            errors.push(CatalogError::DuplicateLevelIid {
                iid: room.iid.clone(),
            });
            continue;
        }
        rooms.push(room);
    }

    if rooms.is_empty() {
        errors.push(CatalogError::EmptyCatalog);
    }

    for dir in [Dir::N, Dir::E, Dir::S, Dir::W] {
        let has_cap = rooms.iter().any(|r| match r.doors.as_slice() {
            [door] => door.dir == dir,
            _ => false,
        });
        if !has_cap {
            errors.push(CatalogError::NoCapRoomForDirection { dir });
        }
    }

    if !errors.is_empty() {
        return Err(CatalogErrors(errors));
    }

    let bool_tag_ids = bool_tag_names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), TagId(i as u16)))
        .collect();
    let float_tag_ids = float_tag_names
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), FloatTagId(i as u16)))
        .collect();

    let spatial_cell_size = rooms
        .iter()
        .map(|r| r.size.width.max(r.size.height))
        .max()
        .unwrap_or(1);
    let reservations = compute_reservations(&rooms);

    let mut by_door_dir: [Vec<RoomId>; 4] = Default::default();
    for (i, room) in rooms.iter().enumerate() {
        let id = RoomId(u32::try_from(i).unwrap_or(u32::MAX));
        for door in &room.doors {
            let Some(bucket) = by_door_dir.get_mut(door.dir as usize) else {
                continue;
            };
            if !bucket.contains(&id) {
                bucket.push(id);
            }
        }
    }

    Ok(Catalog {
        rooms,
        bool_tag_ids,
        float_tag_ids,
        grid_size: grid_size_u32,
        spatial_cell_size,
        reservations,
        by_door_dir,
    })
}

/// ARCHITECTURE.md §6: for every `(door direction, door width)` pair a room
/// can connect through, the footprint of the room reserved to fill it —
/// relative to the open door's position. See `Catalog::door_reservation`.
///
/// Keyed by the direction the OPEN door faces: a room connects through it
/// via its own door facing the opposite way, its door footprint landing one
/// tile beyond the open one (`place.rs`'s `candidate_origin`), so its body's
/// min corner sits at `step(open_dir) - local_pos` from the open door.
///
/// Preference, total and deterministic: caps before multi-door rooms (a cap
/// is what makes the "always fillable" guarantee hold), then smaller area,
/// then catalog order. Computed once here; read-only afterwards and only
/// ever point-looked-up, so the `HashMap` doesn't reintroduce DESIGN.md
/// §7's iteration-order hazard — and `rooms` is walked in catalog order, so
/// the tie-break never depends on one either.
fn compute_reservations(rooms: &[RoomDef]) -> HashMap<(Dir, u8), TileRect> {
    let mut best: HashMap<(Dir, u8), ((bool, u64), TileRect)> = HashMap::new();
    for room in rooms {
        let rank = (
            room.doors.len() != 1,
            u64::from(room.size.width) * u64::from(room.size.height),
        );
        for door in &room.doors {
            let open_dir = door.dir.opposite();
            let rect = TileRect {
                min: open_dir.step() - door.local_pos,
                size: room.size,
            };
            let key = (open_dir, door.width);
            // Strict `<`: the first room in catalog order wins ties.
            let slot = best.entry(key).or_insert((rank, rect));
            if rank < slot.0 {
                *slot = (rank, rect);
            }
        }
    }
    best.into_iter()
        .map(|(key, (_, rect))| (key, rect))
        .collect()
}

fn find_grid_size(project: &LdtkJson) -> Option<i32> {
    project
        .levels
        .iter()
        .filter_map(|l| l.layer_instances.as_ref())
        .flat_map(|layers| layers.iter())
        .map(|l| l.grid_size)
        .next()
}

fn find_tags_field_identifier(project: &LdtkJson) -> Option<String> {
    project
        .defs
        .level_fields
        .iter()
        .find(|f| f.field_definition_type == "Array<LocalEnum.RoomTags>")
        .map(|f| f.identifier.clone())
}

fn find_room_tags_enum_values(project: &LdtkJson) -> Vec<String> {
    project
        .defs
        .enums
        .iter()
        .find(|e| e.identifier == "RoomTags")
        .map(|e| e.values.iter().map(|v| v.id.clone()).collect())
        .unwrap_or_default()
}

fn find_float_tag_field_names(project: &LdtkJson) -> Vec<String> {
    project
        .defs
        .level_fields
        .iter()
        .filter(|f| f.field_definition_type == "Float")
        .map(|f| f.identifier.clone())
        .collect()
}

/// Always produces a `RoomDef` — even a badly-formed level contributes what
/// it can, with problems reported alongside rather than silently dropped, so
/// one bad door doesn't stop the others in the same level from being
/// checked. See this file's module doc on `build_catalog` being "Strict":
/// the caller discards this room if `room_errors` is non-empty.
fn build_room(
    level: &Level,
    source: &DoorSource,
    grid_size: i32,
    tags_field_id: Option<&str>,
    bool_tag_names: &[String],
    float_tag_names: &[String],
) -> (RoomDef, Vec<CatalogError>) {
    let mut errors = Vec::new();

    if level.px_wid % grid_size != 0 || level.px_hei % grid_size != 0 {
        errors.push(CatalogError::LevelNotTileAligned {
            level: level.identifier.clone(),
        });
    }
    let width_tiles = u32::try_from((level.px_wid / grid_size).max(0)).unwrap_or(0);
    let height_tiles = u32::try_from((level.px_hei / grid_size).max(0)).unwrap_or(0);
    let size = TileSize {
        width: width_tiles,
        height: height_tiles,
    };

    let mut doors = Vec::new();
    match &level.layer_instances {
        None => errors.push(CatalogError::DoorLayerMissing {
            level: level.identifier.clone(),
        }),
        Some(layers) => {
            let mut found_door_layer = false;
            for layer in layers {
                if layer.grid_size != grid_size {
                    errors.push(CatalogError::MixedGridSize {
                        level: level.identifier.clone(),
                        expected: grid_size,
                        found: layer.grid_size,
                    });
                }
                match source {
                    DoorSource::Entities => {
                        if layer.layer_instance_type != LdtkLayerType::Entities {
                            continue;
                        }
                        found_door_layer = true;
                        collect_doors(
                            layer,
                            level,
                            grid_size,
                            height_tiles,
                            &mut doors,
                            &mut errors,
                        );
                    }
                    DoorSource::IntGridGaps { layer: name } => {
                        if layer.layer_instance_type != LdtkLayerType::IntGrid
                            || layer.identifier != *name
                        {
                            continue;
                        }
                        found_door_layer = true;
                        collect_gap_doors(layer, level, size, &mut doors, &mut errors);
                    }
                }
            }
            if !found_door_layer {
                errors.push(CatalogError::DoorLayerMissing {
                    level: level.identifier.clone(),
                });
            }
        }
    }

    let bool_tags = resolve_bool_tags(level, tags_field_id, bool_tag_names);
    let float_tags = resolve_float_tags(level, float_tag_names);

    let room = RoomDef {
        iid: level.iid.clone(),
        identifier: level.identifier.clone(),
        size,
        doors,
        world_offset_px: PixelPos {
            x: level.world_x as f32,
            y: level.world_y as f32,
        },
        bool_tags,
        float_tags,
    };

    (room, errors)
}

fn collect_doors(
    layer: &LayerInstance,
    level: &Level,
    grid_size: i32,
    height_tiles: u32,
    doors: &mut Vec<DoorDef>,
    errors: &mut Vec<CatalogError>,
) {
    for entity in &layer.entity_instances {
        if entity.identifier != "Door" {
            continue;
        }
        match build_door(entity, level, grid_size, height_tiles) {
            Some(door) => doors.push(door),
            None => errors.push(CatalogError::DoorNotOnEdge {
                level: level.identifier.clone(),
                at: TilePos {
                    x: entity.grid.x,
                    y: entity.grid.y,
                },
            }),
        }
    }
}

/// `DoorSource::IntGridGaps`: one door per maximal run of empty cells along
/// each edge, in N, E, S, W order and along each edge in increasing `LDtk`
/// x or y, so door indices are stable for a given project.
fn collect_gap_doors(
    layer: &LayerInstance,
    level: &Level,
    size: TileSize,
    doors: &mut Vec<DoorDef>,
    errors: &mut Vec<CatalogError>,
) {
    let (w, h) = (layer.c_wid, layer.c_hei);
    // Out of range reads as wall: a short CSV can't invent a door.
    let open = |x: i32, y: i32| {
        usize::try_from(y * w + x)
            .ok()
            .and_then(|i| layer.int_grid_csv.get(i))
            .is_some_and(|v| *v == 0)
    };
    if w < 1 || h < 1 {
        return;
    }
    for (x, y) in [(0, 0), (w - 1, 0), (w - 1, h - 1), (0, h - 1)] {
        if open(x, y) {
            errors.push(CatalogError::DoorAtCorner {
                level: level.identifier.clone(),
                at: TilePos { x, y },
            });
        }
    }
    let height = i32::try_from(size.height).unwrap_or(0);
    // Runs of open cells at 1..len-1 along an edge: (first index, length).
    let runs = |len: i32, open_at: &dyn Fn(i32) -> bool| {
        let mut out = Vec::new();
        let mut start = None;
        for i in 1..len {
            match (start, i < len - 1 && open_at(i)) {
                (None, true) => start = Some(i),
                (Some(s), false) => {
                    out.push((s, i - s));
                    start = None;
                }
                _ => {}
            }
        }
        out
    };
    // Clamped like `build_door`: no real catalog has a 255-tile door.
    let width = |run: i32| u8::try_from(run.clamp(1, i32::from(u8::MAX))).unwrap_or(1);
    for (start, run) in runs(w, &|x| open(x, 0)) {
        doors.push(DoorDef {
            local_pos: TilePos {
                x: start,
                y: height - 1,
            },
            dir: Dir::N,
            width: width(run),
        });
    }
    // E/W: LDtk rows run top-down, so the run's min corner in the y-up frame
    // is its *last* row — same flip as `build_door`.
    for (start, run) in runs(h, &|y| open(w - 1, y)) {
        doors.push(DoorDef {
            local_pos: TilePos {
                x: w - 1,
                y: height - start - run,
            },
            dir: Dir::E,
            width: width(run),
        });
    }
    for (start, run) in runs(w, &|x| open(x, h - 1)) {
        doors.push(DoorDef {
            local_pos: TilePos { x: start, y: 0 },
            dir: Dir::S,
            width: width(run),
        });
    }
    for (start, run) in runs(h, &|y| open(0, y)) {
        doors.push(DoorDef {
            local_pos: TilePos {
                x: 0,
                y: height - start - run,
            },
            dir: Dir::W,
            width: width(run),
        });
    }
}

/// Direction comes from which level edge the door touches — never a
/// hardcoded tile width (DESIGN.md §2, "Why doors are entities").
fn build_door(
    entity: &EntityInstance,
    level: &Level,
    grid_size: i32,
    height_tiles: u32,
) -> Option<DoorDef> {
    let touches_north = entity.px.y == 0;
    let touches_south = entity.px.y + entity.height == level.px_hei;
    let touches_west = entity.px.x == 0;
    let touches_east = entity.px.x + entity.width == level.px_wid;

    let dir = if touches_north {
        Dir::N
    } else if touches_east {
        Dir::E
    } else if touches_south {
        Dir::S
    } else if touches_west {
        Dir::W
    } else {
        return None;
    };

    // N/S doors run horizontally along the edge (extent = width); E/W doors
    // run vertically (extent = height). Clamped, not `try_from`-and-bail:
    // an out-of-range door width shouldn't make an otherwise-valid door
    // vanish, and realistic catalogs never approach `u8::MAX` tiles wide.
    let extent_px = match dir {
        Dir::N | Dir::S => entity.width,
        Dir::E | Dir::W => entity.height,
    };
    let width = u8::try_from((extent_px / grid_size).clamp(1, i32::from(u8::MAX))).unwrap_or(1);

    // y-up local frame: LDtk's row 0 (top) becomes the highest row here, so
    // an N-facing door's footprint sits at max y — ARCHITECTURE.md §2's
    // y-flip rule. `local_pos` is always the footprint's MIN corner: for
    // N/S doors the flip is single-row and x is untouched, so entity.grid.x
    // is already the min column. For E/W doors the footprint spans
    // `width` LDtk rows starting at entity.grid.y; flipping reverses their
    // order, so the LDtk-side *top* row (smallest raw grid.y, entity.grid.y
    // itself) becomes the generator-side *max* row - the min is on the
    // *other* end, `height_tiles - width - entity.grid.y`. Using the N/S
    // formula here instead would silently point at the wrong corner for
    // any door wider than one tile.
    let height_tiles_i32 = i32::try_from(height_tiles).unwrap_or(0);
    let local_pos = match dir {
        Dir::N | Dir::S => TilePos {
            x: entity.grid.x,
            y: height_tiles_i32 - 1 - entity.grid.y,
        },
        Dir::E | Dir::W => TilePos {
            x: entity.grid.x,
            y: height_tiles_i32 - i32::from(width) - entity.grid.y,
        },
    };

    Some(DoorDef {
        local_pos,
        dir,
        width,
    })
}

fn resolve_bool_tags(level: &Level, tags_field_id: Option<&str>, names: &[String]) -> Vec<bool> {
    let mut out = vec![false; names.len()];
    let Some(field_id) = tags_field_id else {
        return out;
    };
    let Some(instance) = level
        .field_instances
        .iter()
        .find(|f| f.identifier == field_id)
    else {
        return out;
    };
    let FieldValue::Enums(values) = &instance.value else {
        return out;
    };
    for value in values.iter().flatten() {
        if let Some(idx) = names.iter().position(|n| n == value) {
            if let Some(slot) = out.get_mut(idx) {
                *slot = true;
            }
        }
    }
    out
}

fn resolve_float_tags(level: &Level, names: &[String]) -> Vec<f32> {
    names
        .iter()
        .map(|name| {
            // The one deliberate exception to float tags' 0.0 default —
            // DESIGN.md §2: a room with no stated weight should be
            // ordinary, not unselectable.
            let default = if name == "weight" { 1.0 } else { 0.0 };
            level
                .field_instances
                .iter()
                .find(|f| &f.identifier == name)
                .and_then(|f| match &f.value {
                    FieldValue::Float(v) => *v,
                    _ => None,
                })
                .unwrap_or(default)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled catalog predates the tags redesign (DESIGN.md §2): it has
    /// no `RoomTags` enum and no `Float` fields, so every room parses with
    /// zero tags here — correct per the new schema, but this fixture cannot
    /// exercise tag resolution. It can exercise door/room/grid-size parsing.
    fn load_kenney_catalog() -> LdtkJson {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk"
        );
        let raw = std::fs::read_to_string(path).expect("fixture must exist");
        serde_json::from_str(&raw).expect("fixture must be valid LDtk JSON")
    }

    #[test]
    fn parses_the_bundled_kenney_catalog_cleanly() {
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");

        assert_eq!(catalog.grid_size(), 16);
        assert_eq!(catalog.len(), 44);
    }

    #[test]
    fn spawn_room_1_doors_match_hand_verified_geometry() {
        // Verified by hand against the raw JSON: Room_Spawn_1 is 192x128px
        // (12x8 tiles) with three doors at px [112,0]/[176,64]/[128,112],
        // touching the north, east, and south edges respectively.
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");

        let room = catalog
            .iter()
            .find(|r| r.iid == find_level_iid(&project, "Room_Spawn_1"))
            .expect("Room_Spawn_1 must exist in the bundled catalog");

        assert_eq!(
            room.size,
            TileSize {
                width: 12,
                height: 8
            }
        );
        assert_eq!(room.doors.len(), 3);

        let mut dirs: Vec<Dir> = room.doors.iter().map(|d| d.dir).collect();
        dirs.sort_by_key(|d| *d as u8);
        assert_eq!(dirs, vec![Dir::N, Dir::E, Dir::S]);

        for door in &room.doors {
            assert_eq!(door.width, 2, "every door in this room is 2 tiles wide");
        }

        // local_pos is always the footprint's min corner (both this file's
        // module doc and build_door's comment). Hand-derived from the same
        // raw JSON as the direction/width checks above:
        //   N: grid=[7,0]  -> x=7,  y=8-1-0=7
        //   E: grid=[11,4] -> x=11, y=8-width-4=8-2-4=2  (not 8-1-4=3 - that
        //      would be the footprint's max row, the bug this test guards)
        //   S: grid=[8,7]  -> x=8,  y=8-1-7=0
        let door_at = |dir: Dir| {
            room.doors
                .iter()
                .find(|d| d.dir == dir)
                .unwrap_or_else(|| panic!("Room_Spawn_1 must have a {dir:?} door"))
        };
        assert_eq!(door_at(Dir::N).local_pos, TilePos { x: 7, y: 7 });
        assert_eq!(door_at(Dir::E).local_pos, TilePos { x: 11, y: 2 });
        assert_eq!(door_at(Dir::S).local_pos, TilePos { x: 8, y: 0 });
    }

    fn find_level_iid(project: &LdtkJson, identifier: &str) -> String {
        project
            .levels
            .iter()
            .find(|l| l.identifier == identifier)
            .map(|l| l.iid.clone())
            .expect("test fixture level must exist")
    }

    #[test]
    fn door_reservation_is_the_smallest_caps_full_footprint() {
        // Hand-checked against the fixture's raw JSON: the single-door
        // rooms are Hallway_Vertical_W2_3 (4x3, S door), Hallway_Vertical_
        // W2_4 (4x2, N door), Hallway_Horizontal_W2_5 (2x5, E door),
        // Hallway_Horizontal_W2_6 (2x5, W door), plus three 16-wide rooms
        // that lose on area. An open door facing `dir` is filled by a room
        // whose door faces the opposite way, so that is the cap reserved.
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");
        let pos = TilePos { x: 10, y: 20 };

        for (dir, cap_name) in [
            (Dir::N, "Hallway_Vertical_W2_3"),
            (Dir::S, "Hallway_Vertical_W2_4"),
            (Dir::E, "Hallway_Horizontal_W2_6"),
            (Dir::W, "Hallway_Horizontal_W2_5"),
        ] {
            let cap = catalog
                .iter()
                .find(|r| r.identifier == cap_name)
                .expect("fixture cap room must exist");
            let [cap_door] = cap.doors.as_slice() else {
                panic!("{cap_name} must be a single-door room");
            };
            assert_eq!(cap_door.dir, dir.opposite());

            let reserved = catalog
                .door_reservation(pos, dir, 2)
                .unwrap_or_else(|| panic!("{dir:?} must have a reservation"));
            // Exactly where the cap's body lands when it connects: its door
            // footprint one tile beyond the open door.
            assert_eq!(
                reserved,
                TileRect {
                    min: pos + dir.step() - cap_door.local_pos,
                    size: cap.size,
                },
                "wrong reservation for {dir:?}"
            );
            // The whole body, not a door-width strip: wider than the door.
            assert!(reserved.size.width > 2 || reserved.size.height > 2);

            // Width 1 has no doors in this catalog: nothing can connect.
            assert!(catalog.door_reservation(pos, dir, 1).is_none());
        }
    }

    #[test]
    fn by_door_dir_index_is_exact_in_both_directions() {
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");

        // Round-trip check, not a hand-picked example: every room listed in
        // a direction's bucket really has a door facing that way, and every
        // room with a door facing a direction really is listed there.
        for dir in [Dir::N, Dir::E, Dir::S, Dir::W] {
            let bucket = catalog.rooms_with_door_facing(dir);

            for &id in bucket {
                let def = catalog.get(id).expect("bucket must only hold real ids");
                assert!(
                    def.doors.iter().any(|d| d.dir == dir),
                    "room {id:?} is listed for {dir:?} but has no door facing it"
                );
            }

            for (i, def) in catalog.iter().enumerate() {
                if def.doors.iter().any(|d| d.dir == dir) {
                    let id = RoomId(i as u32);
                    assert!(
                        bucket.contains(&id),
                        "room {i} has a door facing {dir:?} but isn't in its bucket"
                    );
                }
            }
        }

        // Room_Spawn_1 (N/E/S doors, per the geometry test above) must
        // appear in exactly those three buckets and not W.
        let spawn_1 = find_level_iid(&project, "Room_Spawn_1");
        let spawn_1_id = RoomId(
            catalog
                .iter()
                .position(|r| r.iid == spawn_1)
                .expect("Room_Spawn_1 must exist") as u32,
        );
        assert!(catalog.rooms_with_door_facing(Dir::N).contains(&spawn_1_id));
        assert!(catalog.rooms_with_door_facing(Dir::E).contains(&spawn_1_id));
        assert!(catalog.rooms_with_door_facing(Dir::S).contains(&spawn_1_id));
        assert!(!catalog.rooms_with_door_facing(Dir::W).contains(&spawn_1_id));
    }

    fn gap_source() -> DoorSource {
        DoorSource::IntGridGaps {
            layer: "WallGrid".to_owned(),
        }
    }

    /// The bundled map has both: `Door` entities and a `WallGrid` with a gap
    /// under each one. Reading either must give the same doors, room by room.
    #[test]
    fn intgrid_gaps_find_the_same_doors_as_entities() {
        let project = load_kenney_catalog();
        let entities = build_catalog(&project).unwrap();
        let gaps = build_catalog_with(&project, &gap_source()).unwrap();
        assert_eq!(entities.len(), gaps.len());
        for (a, b) in entities.iter().zip(gaps.iter()) {
            let sorted = |d: &RoomDef| {
                let mut v: Vec<_> = d
                    .doors
                    .iter()
                    .map(|d| (d.dir as u8, d.local_pos.x, d.local_pos.y, d.width))
                    .collect();
                v.sort_unstable();
                v
            };
            assert_eq!(sorted(a), sorted(b), "level {}", a.identifier);
        }
    }

    #[test]
    fn intgrid_open_corner_is_an_error_naming_the_level() {
        let mut project = load_kenney_catalog();
        let level = project.levels.first_mut().unwrap();
        let name = level.identifier.clone();
        let layer = level
            .layer_instances
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|l| l.identifier == "WallGrid")
            .unwrap();
        *layer.int_grid_csv.first_mut().unwrap() = 0;
        let errors = build_catalog_with(&project, &gap_source()).unwrap_err();
        assert!(errors.0.iter().any(|e| matches!(
            e,
            CatalogError::DoorAtCorner { level, at: TilePos { x: 0, y: 0 } } if *level == name
        )));
    }

    #[test]
    fn intgrid_missing_layer_is_reported() {
        let project = load_kenney_catalog();
        let source = DoorSource::IntGridGaps {
            layer: "NoSuchLayer".to_owned(),
        };
        let errors = build_catalog_with(&project, &source).unwrap_err();
        assert!(
            errors
                .0
                .iter()
                .any(|e| matches!(e, CatalogError::DoorLayerMissing { .. }))
        );
    }
}
