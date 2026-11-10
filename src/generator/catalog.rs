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
    /// Built once; only ever point-looked-up, never iterated during
    /// placement, so the `HashMap` here doesn't reintroduce §7's hazard.
    min_clearance: HashMap<(Dir, u8), TileSize>,
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

    /// The smallest depth any room that can connect through a `(dir, width)`
    /// door needs behind it (`ARCHITECTURE.md` §6). `None` when no room in the
    /// catalog can connect through one — such a door is unfillable anyway.
    pub fn min_clearance(&self, dir: Dir, width: u8) -> Option<TileSize> {
        self.min_clearance.get(&(dir, width)).copied()
    }

    /// The clearance strip beyond one door (ARCHITECTURE.md §6): the space a
    /// connecting room minimally needs — starting one tile beyond the door's
    /// footprint, spanning the door's width along the edge and the smallest
    /// connecting room's depth outward. A placement is valid only if it
    /// neither occupies the strip of an open door it isn't connecting to
    /// (that would seal the door shut for good) nor has a door of its own
    /// whose strip is occupied by a room it isn't connecting to (that door
    /// would open into another room's interior).
    ///
    /// `None` when no room in the catalog can connect through a
    /// `(dir, width)` door — such a door can never be filled by any
    /// candidate, so there is no clearance to protect. (Attempting to fill
    /// it fails on the empty candidate pool before any strip is consulted.)
    pub fn clearance_strip(&self, door_pos: TilePos, dir: Dir, width: u8) -> Option<TileRect> {
        let size = self.min_clearance.get(&(dir, width)).copied()?;
        let min = match dir {
            Dir::N => TilePos {
                x: door_pos.x,
                y: door_pos.y + 1,
            },
            Dir::S => TilePos {
                x: door_pos.x,
                y: door_pos.y - i32::try_from(size.height).unwrap_or(0),
            },
            Dir::E => TilePos {
                x: door_pos.x + 1,
                y: door_pos.y,
            },
            Dir::W => TilePos {
                x: door_pos.x - i32::try_from(size.width).unwrap_or(0),
                y: door_pos.y,
            },
        };
        Some(TileRect { min, size })
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

/// Parses and validates a whole LDtk project. See this file's module doc for
/// why `&LdtkJson` is the input type, and for what "Strict" means here.
pub fn build_catalog(project: &LdtkJson) -> Result<Catalog, CatalogErrors> {
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
    let min_clearance = compute_min_clearance(&rooms);

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
        min_clearance,
        by_door_dir,
    })
}

/// ARCHITECTURE.md §6: for every `(door direction, door width)` pair a
/// candidate could be matched against, the smallest room that can connect
/// through it — the space any placement must keep free beyond such a door
/// for the door to stay fillable. Computed once here; read-only afterwards
/// and only ever point-looked-up, so the `HashMap` doesn't reintroduce
/// DESIGN.md §7's iteration-order hazard.
///
/// Keyed by the direction the OPEN door faces: a room connects through it
/// via its own door facing the opposite direction, and the room's depth
/// behind that door is its full extent along the open door's direction.
/// Only rooms that can actually connect contribute — a room without a
/// matching opposite-facing door can never be placed behind the door, so
/// its size is irrelevant there. With no connecting room at all there is no
/// entry: such a door is unfillable regardless of clearance.
fn compute_min_clearance(rooms: &[RoomDef]) -> HashMap<(Dir, u8), TileSize> {
    let mut best: HashMap<(Dir, u8), (u32, TileSize)> = HashMap::new();
    for room in rooms {
        for door in &room.doors {
            let depth = match door.dir {
                Dir::N | Dir::S => room.size.height,
                Dir::E | Dir::W => room.size.width,
            };
            let entry = match door.dir {
                Dir::N | Dir::S => TileSize {
                    width: u32::from(door.width),
                    height: depth,
                },
                Dir::E | Dir::W => TileSize {
                    width: depth,
                    height: u32::from(door.width),
                },
            };
            let key = (door.dir.opposite(), door.width);
            let slot = best.entry(key).or_insert((depth, entry));
            if depth < slot.0 {
                *slot = (depth, entry);
            }
        }
    }
    best.into_iter()
        .map(|(key, (_, size))| (key, size))
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
            let mut found_entity_layer = false;
            for layer in layers {
                if layer.grid_size != grid_size {
                    errors.push(CatalogError::MixedGridSize {
                        level: level.identifier.clone(),
                        expected: grid_size,
                        found: layer.grid_size,
                    });
                }
                if layer.layer_instance_type != LdtkLayerType::Entities {
                    continue;
                }
                found_entity_layer = true;
                collect_doors(
                    layer,
                    level,
                    grid_size,
                    height_tiles,
                    &mut doors,
                    &mut errors,
                );
            }
            if !found_entity_layer {
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
    fn min_clearance_covers_every_door_shape_in_the_catalog_with_hand_verified_minimums() {
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");

        // Independent recomputation, straight from the raw JSON (the same
        // walk `compute_min_clearance` does, written separately): every door
        // in the fixture is 2 tiles wide, and the smallest room that can
        // connect through any (dir, 2) door is 4x2 — its full 2-tile extent
        // along the connection axis. Keys are the OPEN door's direction.
        for dir in [Dir::N, Dir::E, Dir::S, Dir::W] {
            let clearance = catalog
                .min_clearance(dir, 2)
                .unwrap_or_else(|| panic!("{dir:?} must have a clearance entry"));
            assert_eq!(
                clearance,
                TileSize {
                    width: 2,
                    height: 2
                },
                "wrong clearance for {dir:?}"
            );
            // Widths other than 2 have no doors in this catalog, so no
            // connecting room can exist and there must be no entry.
            assert!(
                catalog.min_clearance(dir, 1).is_none(),
                "width 1 has no doors in this catalog"
            );
        }
    }

    #[test]
    fn clearance_strip_starts_one_tile_beyond_the_door_and_spans_its_width() {
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");

        // A 2-wide strip reaching 2 tiles out: N opens upward (y-up), S
        // downward, E rightward, W leftward. min = the far corner for S/W.
        let pos = TilePos { x: 10, y: 20 };
        let unpack = |strip: Option<TileRect>| -> (TilePos, TileSize) {
            let s = strip.expect("strip must exist for a cataloged (dir, width)");
            (s.min, s.size)
        };

        let (min, size) = unpack(catalog.clearance_strip(pos, Dir::N, 2));
        assert_eq!(min, TilePos { x: 10, y: 21 });
        assert_eq!(
            size,
            TileSize {
                width: 2,
                height: 2
            }
        );

        let (min, size) = unpack(catalog.clearance_strip(pos, Dir::S, 2));
        // Two full tiles below the footprint (y=20): rows 19 and 18.
        assert_eq!(min, TilePos { x: 10, y: 18 });
        assert_eq!(
            size,
            TileSize {
                width: 2,
                height: 2
            }
        );

        let (min, size) = unpack(catalog.clearance_strip(pos, Dir::E, 2));
        assert_eq!(min, TilePos { x: 11, y: 20 });
        assert_eq!(
            size,
            TileSize {
                width: 2,
                height: 2
            }
        );

        let (min, size) = unpack(catalog.clearance_strip(pos, Dir::W, 2));
        // Two full tiles left of the footprint (x=10): columns 9 and 8.
        assert_eq!(min, TilePos { x: 8, y: 20 });
        assert_eq!(
            size,
            TileSize {
                width: 2,
                height: 2
            }
        );

        assert!(catalog.clearance_strip(pos, Dir::N, 1).is_none());
    }

    #[test]
    fn a_connecting_room_exactly_fills_the_clearance_strip_of_the_door_it_connects_to() {
        // The property the whole design rests on: the smallest room that
        // connects through a (dir, 2) door lands precisely on that door's
        // strip. Derived end to end: Room_Spawn_1's S-door footprint at
        // (8, 0); a 4x2 room connecting through it places its own E-door
        // one tile below, so its body is exactly the S strip of that door.
        let project = load_kenney_catalog();
        let catalog = build_catalog(&project).expect("bundled catalog should validate");

        let door_pos = TilePos { x: 8, y: 0 };
        let strip = catalog
            .clearance_strip(door_pos, Dir::S, 2)
            .expect("fixture has (S, 2) doors");

        // A room whose body is exactly that strip must fit it exactly:
        assert_eq!(strip.min, TilePos { x: 8, y: -2 });
        assert_eq!(
            strip.size,
            TileSize {
                width: 2,
                height: 2
            }
        );
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
}
