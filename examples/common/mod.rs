/// A room catalog plus the bits of it an example needs to know about.
///
/// Asset files are named `<author>_<tilemap>_<px>px_<flags>.ldtk`, where the flags
/// describe generator-relevant complexity, so `ldtk_path` doubles as documentation.
#[derive(Debug)]
pub struct RoomSet {
    /// Path passed to `WorldPlugin`, relative to `assets/`.
    pub ldtk_path: &'static str,
    /// IntGrid layer whose value-1 cells are solid.
    pub wall_layer: &'static str,
    /// Edge length of one tile, in pixels.
    pub grid_size: f32,
}

/// 44 rooms on Kenney's 16px Tiny Dungeon tiles: uniform 2-tile doors, full Spawn/Hallway/Room typing.
pub const TINY_DUNGEON: RoomSet = RoomSet {
    ldtk_path: "ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk",
    wall_layer: "WallGrid",
    grid_size: 16.0,
};
