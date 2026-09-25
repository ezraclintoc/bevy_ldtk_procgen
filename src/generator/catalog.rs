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
use super::geom::{Dir, PixelPos, RoomId, TilePos, TileSize};

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CatalogError {
    EmptyCatalog,
    NoStartRoomMatched {
        constraint: String,
    },
    MixedGridSize {
        level: String,
        expected: i32,
        found: i32,
    },
    LevelNotTileAligned {
        level: String,
    },
    DoorLayerMissing {
        level: String,
    },
    DoorNotOnEdge {
        level: String,
        at: TilePos,
    },
    DuplicateLevelIid {
        iid: String,
    },
    /// Warning under `Lenient`, not necessarily fatal under `Strict`.
    NoCapRoomForDirection {
        dir: Dir,
    },
    /// Grid size must be a positive integer — zero would make every
    /// tile-alignment check divide by zero, which the crate's deny-panic
    /// policy (`AGENTS.md`, lint policy) rules out as a code path entirely,
    /// so this is checked and reported instead of ever being reached.
    InvalidGridSize {
        found: i32,
    },
    /// Not a `build_catalog` output — the `.ldtk` asset itself failed to
    /// load (missing file, I/O error), before there was any JSON to
    /// validate. Folded into this enum anyway rather than inventing a
    /// second failure type: `GenerationFailed(CatalogErrors)` is the one
    /// failure surface DESIGN.md §4 defines, and this still needs to reach
    /// it.
    AssetLoadFailed {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FloatTagId(pub(crate) u16);

/// Tag values are stored densely, indexed by ordinal — not a `HashMap`.
#[derive(Debug, Clone)]
pub struct RoomDef {
    pub iid: String,
    pub size: TileSize,
    pub doors: Vec<DoorDef>,
    /// Integration-only: read by `plugin/systems.rs`, never by placement code.
    pub world_offset_px: PixelPos,
    bool_tags: Vec<bool>,
    float_tags: Vec<f32>,
}

impl RoomDef {
    pub fn bool_tag(&self, id: TagId) -> bool {
        self.bool_tags.get(id.0 as usize).copied().unwrap_or(false)
    }

    pub fn float_tag(&self, id: FloatTagId) -> f32 {
        self.float_tags.get(id.0 as usize).copied().unwrap_or(0.0)
    }
}

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

    pub fn tag_id(&self, name: &str) -> Option<TagId> {
        self.bool_tag_ids.get(name).copied()
    }

    pub fn float_tag_id(&self, name: &str) -> Option<FloatTagId> {
        self.float_tag_ids.get(name).copied()
    }

    pub fn min_clearance(&self, dir: Dir, width: u8) -> Option<TileSize> {
        self.min_clearance.get(&(dir, width)).copied()
    }

    pub fn grid_size(&self) -> u32 {
        self.grid_size
    }

    /// Bounds any room's rect to at most four spatial-hash cells regardless
    /// of grid size. See ARCHITECTURE.md §5.
    pub fn spatial_cell_size(&self) -> u32 {
        self.spatial_cell_size
    }

    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

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
        // Depends on the placement loop's actual usage, which doesn't exist
        // yet (place.rs) — computing real values now risks guessing wrong.
        // See ARCHITECTURE.md §6.
        min_clearance: HashMap::new(),
        by_door_dir,
    })
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
