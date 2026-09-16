//! Target API for the rewrite, written as a real example before the
//! implementation exists.
//!
//! This is deliberately not wired into the build yet (see the `autoexamples`
//! note in `Cargo.toml`) — nothing under `bevy_ldtk_procgen::` exists to
//! import. Getting this file to compile and run is the acceptance criterion
//! for the rewrite: once `src/generator/` and `src/plugin/` exist, add an
//! `[[example]]` entry back and this should just work.
//!
//! Deliberately excludes meta-generation (`DESIGN.md` §9) — that's a second,
//! more advanced example once this one is settled.

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

fn main() {
    // No `insert_resource(LdtkSettings { .. })` here: GeneratorPlugin enforces
    // `UseWorldTranslation` and `load_level_neighbors: false` itself. See
    // ARCHITECTURE.md §2.
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(
            GeneratorPlugin::new(
                "ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk",
                800.0,
            )
            .with_seed(1234)
            .with_max_rooms(500, CountBy::All),
        )
        .add_systems(Startup, (spawn_camera, spawn_hud))
        .add_systems(
            Update,
            (
                mark_chest_rooms,
                warn_on_abandoned_doors,
                show_generation_failure,
                update_hud,
            ),
        )
        .run();
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn((Camera2d, GenerationAnchor));
}

/// `RoomSpawned` (`DESIGN.md` §4) is the seam for attaching gameplay to a
/// generated room. `Catalog::tag_id` is a by-name lookup, resolved once here
/// via `Local` rather than by name every event, per `DESIGN.md` §2.
fn mark_chest_rooms(
    mut spawned: MessageReader<RoomSpawned>,
    mut chest_tag: Local<Option<Option<TagId>>>,
    catalog: Res<CatalogRes>,
    mut commands: Commands,
) {
    let chest_tag = *chest_tag.get_or_insert_with(|| catalog.tag_id("chest"));
    let Some(chest_tag) = chest_tag else {
        return;
    };

    for event in spawned.read() {
        let Some(def) = catalog.get(event.room) else {
            continue;
        };
        if def.bool_tag(chest_tag) {
            commands.entity(event.entity).insert(PointLight::default());
        }
    }
}

/// A dead door is a permanent visible gap (`DESIGN.md` §9, "Dead doors") —
/// a real game walls it off; this example just surfaces that it happened.
fn warn_on_abandoned_doors(mut abandoned: MessageReader<DoorAbandoned>) {
    for event in abandoned.read() {
        warn!(
            "door at {:?} facing {:?} could not be filled",
            event.at, event.dir
        );
    }
}

/// The library must not panic on a bad catalog (`AGENTS.md`, lint policy) —
/// `GenerationFailed` is how that surfaces instead.
fn show_generation_failure(mut failed: MessageReader<GenerationFailed>) {
    for GenerationFailed(errors) in failed.read() {
        for error in errors.iter() {
            error!("catalog invalid: {error:?}");
        }
    }
}

#[derive(Component)]
struct HudText;

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("state: -\nrooms: -\nhere: -"),
        TextFont {
            font_size: 18.0,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(10.0),
            ..default()
        },
        HudText,
    ));
}

/// Exercises the two outputs `DESIGN.md` §4 calls out as needed "constantly":
/// `Layout`'s room count, and the point -> room query, here against the
/// anchor's own position rather than the player's.
///
/// `Layout::room_at` takes a `TilePos` (`generator/` is tiles-only), so the
/// anchor's pixel `Transform` is converted via `Catalog::grid_size` first —
/// that conversion belongs here, at the plugin boundary, not inside `generator/`.
fn update_hud(
    state: Res<State<GenerationState>>,
    catalog: Res<CatalogRes>,
    layout: Res<LayoutRes>,
    anchor: Query<&GlobalTransform, With<GenerationAnchor>>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let grid_size = catalog.grid_size() as f32;
    let here = anchor
        .single()
        .ok()
        .map(|t| t.translation().truncate())
        .map(|px| TilePos {
            x: (px.x / grid_size).floor() as i32,
            y: (px.y / grid_size).floor() as i32,
        })
        .and_then(|tile| layout.room_at(&catalog, tile))
        .and_then(|id| catalog.get(id))
        .map_or_else(|| "-".to_string(), |def| format!("{:?}", def.iid));

    text.0 = format!(
        "state: {:?}\nrooms: {}\nhere: {here}",
        state.get(),
        layout.placed_count(),
    );
}
