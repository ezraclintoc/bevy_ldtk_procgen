use super::spatial_hash::SpatialHash;
use bevy::prelude::*;
use bevy::tasks::Task;
use rand::rngs::SmallRng;
use std::collections::{HashMap, VecDeque};
use std::str::FromStr;

//TODO: derive cell size from ldtk level
const DEFAULT_CELL_SIZE: f32 = 128.0;

/// Generation State
#[derive(States, Debug, Clone, Copy, Eq, PartialEq, Hash, Default)]
pub enum GenerationState {
    #[default]
    AssetLoading,
    Indexing,
    Ready,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoomType {
    Spawn,
    Room,
    Hallway,
}

impl FromStr for RoomType {
    type Err = ();

    fn from_str(input: &str) -> Result<RoomType, Self::Err> {
        match input {
            "Hallway" => Ok(RoomType::Hallway),
            "Room" => Ok(RoomType::Room),
            "Spawn" => Ok(RoomType::Spawn),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoomDef {
    pub iid: String,
    pub size: IVec2,
    pub offset: Vec2,
    pub doors: Vec<DoorDef>,
    pub weight: f32,
    pub room_type: RoomType,
}

#[derive(Debug, Clone)]
pub struct Room {
    pub room: RoomDef,
    pub world_pos: Vec2,
}

impl Room {
    pub fn new(roomdef: &RoomDef, world_pos: Vec2) -> Self {
        Room {
            world_pos,
            room: roomdef.clone(),
        }
    }
}

#[derive(Resource, Default)]
pub struct GenTask(pub Option<Task<Vec<Room>>>);

/// Generation knobs configurable via `WorldPlugin`; other tuning consts stay internal.
#[derive(Resource, Clone, Copy)]
pub struct GenerationConfig {
    pub camera_spawn_dist: f32,
    pub max_rooms: usize,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            camera_spawn_dist: 10000.0,
            max_rooms: 10000,
        }
    }
}

#[derive(Resource, Default)]
pub struct SpawnQueue(pub VecDeque<Room>);

#[derive(Resource)]
pub struct GenRng(pub SmallRng);

#[derive(Debug, Clone, PartialEq)]
pub struct DoorDef {
    pub local_pos: IVec2,
    pub width: i32,
    pub dir: Dir,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Door {
    pub door: DoorDef,
    pub world_pos: Vec2,
}

impl Door {
    pub fn new(room: &Room, door: &DoorDef) -> Self {
        Door {
            door: door.clone(),
            world_pos: room.world_pos
                + door.local_pos.as_vec2() * 16.0
                + door.dir.door_offset(door.width as f32) * Vec2::new(0.0, 1.0),
        }
    }

    /// Minimum clearance zone for a door cap, centered on the door's width axis.
    /// N reserves 3 tiles of depth (2 tiles of wall thickness + 1 for the door);
    /// S/E/W reserve 2 (1 tile of wall + 1 for the door) - intentional per the
    /// tilemap's wall thickness, not a bug (an earlier attempt to make N match S
    /// was wrong and got reverted).
    pub fn get_bounding_box(&self) -> (Vec2, Vec2) {
        let pos = self.world_pos + self.door.dir.as_vec() * 16.0;
        match self.door.dir {
            Dir::N => (pos + Vec2::new(0.0, 8.0), Vec2::new(64.0, 48.0)),
            Dir::S => (pos, Vec2::new(64.0, 32.0)),
            Dir::E => (pos + Vec2::new(0.0, 8.0), Vec2::new(32.0, 80.0)),
            Dir::W => (pos + Vec2::new(0.0, 8.0), Vec2::new(32.0, 80.0)),
        }
    }
}

#[derive(Resource, Clone)]
pub struct WorldState {
    pub open_doors: Vec<Door>,
    pub rooms: Vec<Room>,
    pub room_grid: SpatialHash,
}

impl Default for WorldState {
    fn default() -> Self {
        WorldState {
            open_doors: Vec::new(),
            rooms: Vec::new(),
            room_grid: SpatialHash::new(DEFAULT_CELL_SIZE),
        }
    }
}

impl WorldState {
    pub fn add_room(&mut self, room: Room) {
        let index = self.rooms.len();
        let world_pos = room.world_pos;
        let size = room.room.size.as_vec2();
        self.rooms.push(room.clone());
        for doordef in &room.room.doors {
            let door = Door::new(&room, doordef);
            if let Some(idx) = self
                .open_doors
                .iter()
                .position(|d| d.world_pos == door.world_pos)
            {
                self.open_doors.swap_remove(idx);
            } else {
                self.open_doors.push(door);
            }
        }
        self.room_grid.insert(index, world_pos, size);
    }
}

#[derive(Resource, Default, Clone)]
pub struct RoomIndex {
    pub rooms: Vec<RoomDef>,
    pub by_door_dir: HashMap<Dir, Vec<usize>>, // direction -> indices into rooms
}

#[derive(Eq, Hash, PartialEq, Clone, Copy, Debug)]
pub enum Dir {
    N,
    S,
    E,
    W,
}

impl Dir {
    pub fn as_vec(&self) -> Vec2 {
        match self {
            Dir::N => Vec2::from_array([0.0, 1.0]),
            Dir::S => Vec2::from_array([0.0, -1.0]),
            Dir::E => Vec2::from_array([1.0, 0.0]),
            Dir::W => Vec2::from_array([-1.0, 0.0]),
        }
    }

    pub fn opposite(&self) -> Dir {
        match self {
            Dir::N => Dir::S,
            Dir::S => Dir::N,
            Dir::E => Dir::W,
            Dir::W => Dir::E,
        }
    }

    // pub fn rotate_cl(&self) -> Dir {
    //     match self {
    //         Dir::N => Dir::E,
    //         Dir::S => Dir::W,
    //         Dir::E => Dir::S,
    //         Dir::W => Dir::N,
    //     }
    // }

    pub fn door_offset(&self, width: f32) -> Vec2 {
        let wh = width / 2.0;
        let door_offset = match self {
            Dir::N => Vec2::new(16.0 * wh, 0.0),
            Dir::S => Vec2::new(16.0 * wh, -16.0),
            Dir::E => Vec2::new(16.0, -16.0 * wh),
            Dir::W => Vec2::new(0.0, -16.0 * wh),
        };
        door_offset
    }

    // pub fn is_vertical(&self) -> bool {
    //     match self {
    //         Dir::N => true,
    //         Dir::S => true,
    //         Dir::E => false,
    //         Dir::W => false,
    //     }
    // }

    // pub fn is_horizontal(&self) -> bool {
    //     !self.is_vertical()
    // }
}

pub fn rects_collide(center_a: Vec2, size_a: Vec2, top_left_b: Vec2, size_b: Vec2) -> bool {
    // rect A from center
    let a_left = center_a.x - size_a.x / 2.0;
    let a_right = center_a.x + size_a.x / 2.0;
    let a_top = center_a.y + size_a.y / 2.0;
    let a_bottom = center_a.y - size_a.y / 2.0;

    // rect B from top-left, extending right and down (y decreasing)
    let b_left = top_left_b.x;
    let b_right = top_left_b.x + size_b.x;
    let b_top = top_left_b.y;
    let b_bottom = top_left_b.y - size_b.y;

    a_left < b_right && a_right > b_left && a_bottom < b_top && a_top > b_bottom
}

pub fn rects_collide_tl(top_left_a: Vec2, size_a: Vec2, top_left_b: Vec2, size_b: Vec2) -> bool {
    let a_left = top_left_a.x;
    let a_right = top_left_a.x + size_a.x;
    let a_top = top_left_a.y;
    let a_bottom = top_left_a.y - size_a.y;

    let b_left = top_left_b.x;
    let b_right = top_left_b.x + size_b.x;
    let b_top = top_left_b.y;
    let b_bottom = top_left_b.y - size_b.y;

    a_left < b_right && a_right > b_left && a_bottom < b_top && a_top > b_bottom
}

pub fn rects_collide_center(center_a: Vec2, size_a: Vec2, center_b: Vec2, size_b: Vec2) -> bool {
    let a_left = center_a.x - size_a.x / 2.0;
    let a_right = center_a.x + size_a.x / 2.0;
    let a_top = center_a.y + size_a.y / 2.0;
    let a_bottom = center_a.y - size_a.y / 2.0;

    let b_left = center_b.x - size_b.x / 2.0;
    let b_right = center_b.x + size_b.x / 2.0;
    let b_top = center_b.y + size_b.y / 2.0;
    let b_bottom = center_b.y - size_b.y / 2.0;

    a_left < b_right && a_right > b_left && a_bottom < b_top && a_top > b_bottom
}

/// Returns every catalog room that can fill both door_a and door_b, not just the first.
pub fn find_bridging_room(door_a: &Door, door_b: &Door, room_idx: &RoomIndex) -> Vec<Room> {
    let mut candidates = Vec::new();
    for candidate in &room_idx.rooms {
        for d1 in &candidate.doors {
            if d1.dir != door_a.door.dir.opposite() {
                continue;
            }

            // solve Door::new's formula for room.world_pos so d1 lands on door_a
            let room_world_pos = door_a.world_pos
                - d1.local_pos.as_vec2() * 16.0
                - d1.dir.door_offset(d1.width as f32) * Vec2::new(0.0, 1.0);

            let placed = Room::new(candidate, room_world_pos);

            for d2 in &candidate.doors {
                if d2 == d1 {
                    continue;
                }
                let d2_door = Door::new(&placed, d2);
                if d2_door.world_pos == door_b.world_pos && d2.dir == door_b.door.dir.opposite() {
                    candidates.push(placed.clone());
                }
            }
        }
    }
    candidates
}
