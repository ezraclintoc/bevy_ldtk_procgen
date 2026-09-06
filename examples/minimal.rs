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
    // `UseWorldTranslation` and `load_level_neighbors: false` itself, because
    // both are load-bearing for its placement math and its culling invariant,
    // not a consumer choice. See ARCHITECTURE.md §2.
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(setup_generator)
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

/// Deferred so `main` doesn't need direct `AssetServer` access before the app
/// exists. `GeneratorPlugin::new` takes the two inputs `DESIGN.md` §3 lists
/// with no default — everything else is a builder call on top.
fn setup_generator(app: &mut App) {
    let project = app.world().resource::<AssetServer>().load(
        "ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk",
    );

    app.add_plugins(
        GeneratorPlugin::new(project, 800.0)
            .with_seed(1234)
            .with_max_rooms(500, CountBy::All),
    );
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn((Camera2d, GenerationAnchor));
}

/// `RoomSpawned` (`DESIGN.md` §4) is the seam for attaching gameplay to a
/// generated room. `Catalog::tag_id` is a by-name lookup, resolved once here
/// via `Local` rather than by name every event, per `DESIGN.md` §2.
///
/// `None` is cached too, not just retried: a catalog either has a `chest` tag
/// or it never will (tags are fixed at load), so failing once means skip
/// forever rather than re-querying by name on every future room.
fn mark_chest_rooms(
    mut spawned: EventReader<RoomSpawned>,
    mut chest_tag: Local<Option<Option<TagId>>>,
    catalog: Res<Catalog>,
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
fn warn_on_abandoned_doors(mut abandoned: EventReader<DoorAbandoned>) {
    for event in abandoned.read() {
        warn!("door at {:?} facing {:?} could not be filled", event.at, event.dir);
    }
}

/// The library must not panic on a bad catalog (`AGENTS.md`, lint policy) —
/// `GenerationFailed` is how that surfaces instead. A real game would show a
/// proper error screen; this just logs every collected error (`DESIGN.md` §6
/// collects all of them, not just the first).
fn show_generation_failure(mut failed: EventReader<GenerationFailed>) {
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
fn update_hud(
    state: Res<State<GenerationState>>,
    catalog: Res<Catalog>,
    layout: Res<Layout>,
    anchor: Query<&GlobalTransform, With<GenerationAnchor>>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let here = anchor
        .single()
        .ok()
        .and_then(|t| layout.room_at(&catalog, t.translation().truncate()))
        .and_then(|id| catalog.get(id))
        .map_or_else(|| "-".to_string(), |def| format!("{:?}", def.iid));

    text.0 = format!(
        "state: {:?}\nrooms: {}\nhere: {here}",
        state.get(),
        layout.placed_count(),
    );
}
