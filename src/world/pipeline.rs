use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, block_on, poll_once};
use bevy_ecs_ldtk::{ldtk::Level, prelude::*};
use rand::seq::SliceRandom;
use rand::{RngExt, SeedableRng};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use super::types::*;
use crate::world::LdtkHandle;

const MAX_ROOMS_PER_FRAME: usize = 1000;
const SPAWNS_PER_FRAME: usize = 5;

pub fn is_ldtk_loaded(
    asset_server: Res<AssetServer>,
    ldtk_handle: Res<LdtkHandle>,
    mut next_state: ResMut<NextState<GenerationState>>,
) {
    // Check if the specific LDtk file has finished loading into memory
    if asset_server.is_loaded_with_dependencies(&ldtk_handle.0) {
        next_state.set(GenerationState::Indexing);
    }
}

pub fn create_room_index(
    projects: Query<&LdtkProjectHandle>,
    project_assets: Res<Assets<LdtkProject>>,
    mut room_idx: ResMut<RoomIndex>,
    mut next_state: ResMut<NextState<GenerationState>>,
) {
    let Ok(handle) = projects.single() else {
        return;
    };
    let Some(project) = project_assets.get(handle) else {
        return;
    };

    *room_idx = build_room_index(project.iter_raw_levels());
    info!("Room index built: {} rooms", room_idx.rooms.len());
    next_state.set(GenerationState::Ready);
}

/// Parses raw LDtk levels into RoomIndex. Doesn't need BevyApp
pub fn build_room_index<'a>(levels: impl Iterator<Item = &'a Level>) -> RoomIndex {
    let mut rooms = Vec::new();

    for level in levels {
        let mut doors = Vec::new();
        let Some(layers) = &level.layer_instances else {
            continue;
        };

        for layer in layers {
            if layer.identifier != "Entities" {
                continue;
            }
            for entity in &layer.entity_instances {
                if entity.identifier != "Door" {
                    continue;
                }

                // TODO: magic number
                let dir = if entity.width != 16 {
                    if entity.grid.y == 0 { Dir::N } else { Dir::S }
                } else {
                    if entity.grid.x == 0 { Dir::W } else { Dir::E }
                };
                let width = if entity.width == 16 {
                    entity.height / 16
                } else {
                    entity.width / 16
                };

                doors.push(DoorDef {
                    local_pos: entity.grid * IVec2::new(1, -1)
                        + if dir == Dir::S {
                            IVec2::new(1, 0)
                        } else if dir == Dir::W {
                            IVec2::new(0, 0)
                        } else if dir == Dir::N {
                            IVec2::new(1, 0)
                        } else {
                            IVec2::new(1, 0)
                        },
                    width: width,
                    dir,
                });
            }
        }

        let weight = level
            .field_instances
            .iter()
            .find(|f| f.identifier == "weight")
            .and_then(|f| match &f.value {
                FieldValue::Float(Some(v)) => Some(*v),
                _ => None,
            })
            .unwrap_or(1.0);

        let room_type = RoomType::from_str(
            level
                .field_instances
                .iter()
                .find(|f| f.identifier == "room_type")
                .and_then(|f| match &f.value {
                    FieldValue::Enum(Some(v)) => Some(v.clone()),
                    _ => {
                        warn!("Invalid type");
                        None
                    }
                })
                .unwrap_or_else(|| "Hallway".to_owned())
                .as_str(),
        )
        .unwrap_or(RoomType::Hallway);

        rooms.push(RoomDef {
            iid: level.iid.clone(),
            size: IVec2::new(level.px_wid, level.px_hei),
            offset: Vec2::new(level.world_x as f32, level.world_y as f32),
            doors,
            weight,
            room_type,
        });
    }

    // build the direction index
    let mut by_door_dir: HashMap<Dir, Vec<usize>> = HashMap::new();
    for (i, room) in rooms.iter().enumerate() {
        for door in &room.doors {
            by_door_dir.entry(door.dir).or_default().push(i);
        }
    }

    RoomIndex { rooms, by_door_dir }
}

pub fn spawn_if_idle(
    mut task: ResMut<GenTask>,
    state: Res<WorldState>,
    camera: Query<&GlobalTransform, With<Camera2d>>,
    room_idx: Res<RoomIndex>,
    mut gen_rng: ResMut<GenRng>,
    config: Res<GenerationConfig>,
) {
    if task.0.is_some() {
        return;
    }

    let mut state = state.clone();

    let pool = AsyncComputeTaskPool::get();

    let cam_pos = camera
        .single()
        .unwrap_or(&GlobalTransform::default())
        .translation()
        .truncate();

    let room_idx = room_idx.clone();

    let mut rng = rand::rngs::SmallRng::seed_from_u64(gen_rng.0.random());

    let config = *config;

    task.0 = Some(pool.spawn(async move {
        generate_batch(
            &mut state,
            &room_idx,
            cam_pos,
            config.camera_spawn_dist,
            config.max_rooms,
            &mut rng,
        )
    }));
}

/// Runs one generation batch, mutating `state` in place so later doors see earlier placements.
pub fn generate_batch(
    state: &mut WorldState,
    room_idx: &RoomIndex,
    cam_pos: Vec2,
    search_dist: f32,
    max_rooms: usize,
    rng: &mut rand::rngs::SmallRng,
) -> Vec<Room> {
    let mut placed_rooms: Vec<Room> = Vec::new();

    {
        if state.rooms.is_empty() {
            let mut spawn_rooms: Vec<&RoomDef> = room_idx
                .rooms
                .iter()
                .filter(|rd| rd.room_type == RoomType::Spawn)
                .collect();
            spawn_rooms.shuffle(rng);

            // TODO: Allow no spawn rooms
            placed_rooms.push(Room::new(
                *spawn_rooms.first().expect("No spawn room found."),
                Vec2::ZERO,
            ));
        } else {
            while placed_rooms.len() < MAX_ROOMS_PER_FRAME && state.open_doors.len() != 0 {
                let placed_before_pass = placed_rooms.len();
                let mut nearby_doors: Vec<Door> = state
                    .open_doors
                    .iter()
                    .filter(|d| d.world_pos.distance(cam_pos) <= search_dist)
                    .cloned()
                    .collect();

                nearby_doors.sort_by(|a, b| {
                    a.world_pos
                        .distance(cam_pos)
                        .partial_cmp(&b.world_pos.distance(cam_pos))
                        .unwrap()
                });

                for door in &nearby_doors {
                    if !state
                        .open_doors
                        .iter()
                        .any(|d| d.world_pos == door.world_pos)
                    {
                        continue;
                    }

                    if state.rooms.len() + placed_rooms.len() >= max_rooms {
                        break;
                    }

                    let dir = door.door.dir;

                    let Some(room_indices) = room_idx.by_door_dir.get(&dir.opposite()) else {
                        continue;
                    };

                    let mut candidates: Vec<usize> = room_indices
                        .iter()
                        .flat_map(|&i| {
                            let weight = (room_idx.rooms[i].weight * 10.0).round() as usize;
                            std::iter::repeat(i).take(weight.max(1))
                        })
                        .collect();

                    candidates.shuffle(rng);

                    let mut tried: HashSet<usize> = HashSet::new();
                    for &room_idx_pick in &candidates {
                        if tried.contains(&room_idx_pick) {
                            continue;
                        }
                        tried.insert(room_idx_pick);

                        let roomdef = &room_idx.rooms[room_idx_pick];
                        let Some(matching_door) =
                            roomdef.doors.iter().find(|d| d.dir == dir.opposite())
                        else {
                            continue;
                        };

                        let matching_door_vec =
                            (matching_door.local_pos.as_vec2() - dir.as_vec()) * 16.0;
                        let room_world_pos = door.world_pos - matching_door_vec
                            + if dir == Dir::S {
                                Vec2::new(0.0, 16.0)
                            } else if dir == Dir::E {
                                Vec2::new(-16.0, 16.0)
                            } else if dir == Dir::W {
                                Vec2::new(16.0, 16.0)
                            } else {
                                Vec2::ZERO
                            };

                        let room = Room::new(&roomdef, room_world_pos);

                        let Some(rooms_to_place) = try_place_room(&room, &state, &room_idx, 0)
                        else {
                            continue;
                        };

                        for placed in &rooms_to_place {
                            state.add_room(placed.clone());
                            placed_rooms.push(placed.clone());
                        }

                        break;
                    }
                }

                if placed_rooms.len() == placed_before_pass {
                    break; // no door got filled this pass - remaining ones can't be placed right now
                }
            }
        }
    }
    placed_rooms
}

pub fn poll_task(
    mut task: ResMut<GenTask>,
    mut state: ResMut<WorldState>,
    mut spawn_queue: ResMut<SpawnQueue>,
    mut commands: Commands,
    ldtk_handle: Res<LdtkHandle>,
    mut batch: Local<usize>,
) {
    if let Some(t) = &mut task.0 {
        if let Some(new_rooms) = block_on(poll_once(t)) {
            *batch += 1;
            for room in new_rooms {
                state.add_room(room.clone());
                spawn_queue.0.push_back(room);
            }
            task.0 = None;
        }
    }

    for _ in 0..SPAWNS_PER_FRAME {
        let Some(room) = spawn_queue.0.pop_front() else {
            break;
        };
        let level_set = LevelSet::from_iids([room.room.iid.clone()]);
        commands.spawn((LdtkWorldBundle {
            ldtk_handle: ldtk_handle.0.clone().into(),
            level_set,
            transform: Transform::from_xyz(
                room.world_pos.x - room.room.offset.x as f32,
                room.world_pos.y + room.room.offset.y as f32,
                50.0,
            ),
            ..default()
        },));
    }
}

pub const MAX_BRIDGE_DEPTH: usize = 5;

pub fn try_place_room(
    room: &Room,
    state: &WorldState,
    room_idx: &RoomIndex,
    depth: usize,
) -> Option<Vec<Room>> {
    if depth > 3 {
        warn!("try_place_room recursion depth {} exceeds 3", depth);
    }
    if depth > MAX_BRIDGE_DEPTH {
        return None;
    }

    let mut result = vec![room.clone()];

    // 1. room rect vs placed rooms
    if state
        .room_grid
        .query(room.world_pos, room.room.size.as_vec2())
        .any(|idx| {
            rects_collide_tl(
                room.world_pos,
                room.room.size.as_vec2(),
                state.rooms[idx].world_pos,
                state.rooms[idx].room.size.as_vec2(),
            )
        })
    {
        return None;
    }

    // 2. new room's doors vs placed rooms - queried per door
    for doordef in &room.room.doors {
        let door = Door::new(room, doordef);

        // door completing a connection to an already-open door - no clearance check needed
        let is_active_connection = state
            .open_doors
            .iter()
            .any(|d| d.world_pos == door.world_pos && d.door.dir == doordef.dir.opposite());
        if is_active_connection {
            continue;
        }

        let (bbox_center, bbox_size) = door.get_bounding_box();
        // center -> top-left for SpatialHash::query; y is +size/2 here, not -size/2 (y-up-but-extends-down)
        let bbox_top_left = Vec2::new(
            bbox_center.x - bbox_size.x / 2.0,
            bbox_center.y + bbox_size.y / 2.0,
        );

        let collides = state.room_grid.query(bbox_top_left, bbox_size).any(|idx| {
            let r = &state.rooms[idx];
            if !rects_collide(bbox_center, bbox_size, r.world_pos, r.room.size.as_vec2()) {
                return false;
            }
            let is_door_spot = r
                .room
                .doors
                .iter()
                .any(|d| Door::new(r, d).world_pos == door.world_pos);
            !is_door_spot
        });
        if collides {
            return None;
        }
    }

    // 3. existing open doors vs new room's whole footprint
    for existing in &state.open_doors {
        if !rects_collide(
            existing.get_bounding_box().0,
            existing.get_bounding_box().1,
            room.world_pos,
            room.room.size.as_vec2(),
        ) {
            continue;
        }
        let is_own_door = room.room.doors.iter().any(|d| {
            d.dir == existing.door.dir.opposite()
                && Door::new(room, d).world_pos == existing.world_pos
        });
        if !is_own_door {
            return None;
        }
    }

    // 4. new room's doors vs open doors - collision triggers bridging
    for doordef in &room.room.doors {
        let new_door = Door::new(room, doordef);
        for existing in &state.open_doors {
            if new_door.world_pos == existing.world_pos {
                continue; // this is the connection itself
            }
            let (nc, ns) = new_door.get_bounding_box();
            let (ec, es) = existing.get_bounding_box();
            if rects_collide_center(nc, ns, ec, es) {
                // pretend `room` is placed, then validate the bridge
                let mut pretend = state.clone();
                pretend.add_room(room.clone());

                // try every candidate, not just the first, since one may fail validation
                let bridge_candidates = find_bridging_room(&new_door, existing, room_idx);
                let bridge_rooms = bridge_candidates
                    .iter()
                    .find_map(|bridge| try_place_room(bridge, &pretend, room_idx, depth + 1));
                let Some(mut bridge_rooms) = bridge_rooms else {
                    return None;
                };
                result.append(&mut bridge_rooms);
            }
        }
    }

    Some(result)
}
