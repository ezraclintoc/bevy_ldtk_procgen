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

/// 44 rooms on Kenney-style 16px tiles: uniform 2-tile doors, full Spawn/Hallway/Room typing.
pub const TILEMAP_PACKED: RoomSet = RoomSet {
    ldtk_path: "ezraclintoc_tilemap-packed_16px_fixeddoors-roomtypes.ldtk",
    wall_layer: "WallGrid",
    grid_size: 16.0,
};
