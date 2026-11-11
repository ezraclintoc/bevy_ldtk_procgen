//! The acceptance criterion `examples/minimal.rs`'s original header named:
//! the full loop — asset load → catalog → start room → door filling →
//! spawned entities → culling — driven through the **public API only**,
//! in a real `App` with no GPU. `MinimalPlugins` + `AssetPlugin` stand in
//! for `DefaultPlugins`; nothing here renders.
//!
//! Runs against the `_notileset` fixture: bevy 0.18's image load pipeline
//! does not complete without a live renderer (the `.ldtk` itself loads
//! fine), so a headless e2e needs a project with no texture dependencies —
//! same 44 rooms and 97 doors as the real fixture, tileset references
//! stripped. Windowed coverage of the real asset is the examples' job.
//!
//! Asserts are loose lower bounds, never exact counts: the seed is fixed,
//! but the point of this test is "the pipeline works", not "the layout is
//! exactly this" — tighter reproducibility claims live in the `generator/`
//! unit tests where they can be exact.

use bevy::app::SubApp;
use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::render::RenderApp;
use bevy::state::app::StatesPlugin;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

/// `bevy_ecs_ldtk` pulls in `bevy_ecs_tilemap` with its `render` feature,
/// whose plugin does `app.sub_app_mut(RenderApp)` at build time — a hard
/// requirement for a render sub-app even when nothing will ever be
/// extracted. A bare `SubApp` satisfies it: with no `update_schedule` it
/// never runs anything, so tilemap's render systems stay dormant; only
/// their *registration* had to succeed.
fn stub_render_app(app: &mut App) {
    let render_app = SubApp::new();
    app.insert_sub_app(RenderApp, render_app);
}

/// What the consumer side of the API observes, collected by the system at
/// the bottom of this file.
#[derive(Resource, Default)]
struct Observed {
    placed: Vec<RoomId>,
    spawned: Vec<RoomId>,
    despawned: Vec<RoomId>,
    abandoned: usize,
    failed: usize,
    settled: usize,
}

fn record_events(
    mut placed: MessageReader<RoomPlaced>,
    mut spawned: MessageReader<RoomSpawned>,
    mut despawned: MessageReader<RoomDespawned>,
    mut abandoned: MessageReader<DoorAbandoned>,
    mut failed: MessageReader<GenerationFailed>,
    mut settled: MessageReader<GenerationSettled>,
    mut observed: ResMut<Observed>,
) {
    for event in placed.read() {
        observed.placed.push(event.room);
    }
    for event in spawned.read() {
        observed.spawned.push(event.room);
    }
    for event in despawned.read() {
        observed.despawned.push(event.room);
    }
    observed.abandoned += abandoned.read().count();
    observed.failed += failed.read().count();
    observed.settled += settled.read().count();
}

/// Moves the anchor so the cull side of the hysteresis band has something
/// to do, without leaving every room behind.
fn move_anchor(
    mut anchors: Query<&mut Transform, With<GenerationAnchor>>,
    layout: Res<LayoutRes>,
    catalog: Res<CatalogRes>,
    mut moved: Local<bool>,
) {
    if *moved || layout.placed_count() < 5 {
        return;
    }
    let grid_size = catalog.grid_size() as f32;
    // Half a room-width east — enough to push the earliest rooms toward the
    // despawn threshold while generation continues ahead.
    let east = (catalog.spatial_cell_size() as f32 * grid_size) / 2.0;
    for mut transform in anchors.iter_mut() {
        transform.translation.x += east;
    }
    *moved = true;
}

/// The headless app every test here drives, with `plugin` as the generator.
fn headless_app(plugin: GeneratorPlugin) -> App {
    let mut app = App::new();

    // Headless stand-in for DefaultPlugins: task pools (asset I/O), assets,
    // state machinery, and the asset types `bevy_ecs_ldtk`'s spawn path
    // touches. No render plugins — nothing in this test needs a GPU.
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin::default(),
        ImagePlugin::default_nearest(),
        StatesPlugin::default(),
    ));
    stub_render_app(&mut app);
    // `ImagePlugin` inits `Image` but not the atlas layout type that
    // `bevy_ecs_ldtk`'s level spawn path writes to.
    app.init_asset::<TextureAtlasLayout>();
    // bevy_ecs_tilemap 0.20 loads its shader at plugin build, and with no
    // render plugins nothing else registers the asset type.
    app.init_asset::<bevy::shader::Shader>();
    app.insert_resource(ClearColor::default())
        .add_plugins(LdtkPlugin)
        .add_plugins(plugin);
    // The anchor is the camera in the real examples; here just a transform
    // at the origin. GlobalTransform arrives via required components.
    app.world_mut()
        .spawn((Transform::default(), GenerationAnchor));
    app
}

const MAP: &str = "ezraclintoc_kenney-tiny-dungeon_16px_notileset.ldtk";

#[test]
fn streaming_generation_runs_end_to_end() {
    let mut app = headless_app(GeneratorPlugin::new(MAP, 800.0).with_seed(1234));
    app.init_resource::<Observed>()
        .add_systems(Update, (record_events, move_anchor));

    // Plenty of frames for: load (a few), catalog build, start room, then
    // streaming fills and spawns (throttled per frame by design).
    for _ in 0..300 {
        app.update();
    }

    // All mutable world access ends here — every `Res` borrow below is
    // taken afterward, so they can't overlap.
    let anchor_px = {
        // Filtered, not plain `query::<&GlobalTransform>`: spawned room
        // entities carry transforms too.
        let mut q = app
            .world_mut()
            .query_filtered::<&GlobalTransform, With<GenerationAnchor>>();
        q.single(app.world())
            .expect("test spawned exactly one anchor")
            .translation()
            .truncate()
    };

    let observed = app.world().resource::<Observed>();
    let state = app.world().resource::<State<GenerationState>>();
    assert_eq!(
        state.get(),
        &GenerationState::Ready,
        "generation must reach Ready and stay there"
    );
    assert_eq!(
        observed.failed, 0,
        "nothing should fail on the bundled catalog"
    );
    assert_eq!(
        observed.abandoned, 0,
        "no door should be abandoned on the bundled catalog — abandoned doors are \
         the dead-door path firing (DESIGN.md §9), i.e. doors the validity checks \
         could never fill"
    );

    let layout = app.world().resource::<LayoutRes>();
    assert!(
        layout.placed_count() >= 5,
        "the dungeon must grow well past the start room, got {}",
        layout.placed_count()
    );
    assert_eq!(
        observed.placed.len(),
        layout.placed_count(),
        "every placement must have raised RoomPlaced exactly once"
    );
    assert!(
        !observed.spawned.is_empty(),
        "placed rooms must become spawned entities"
    );
    assert!(
        observed.settled >= 1,
        "with the anchor still, generation must fill its ring and report \
         GenerationSettled"
    );

    // Point → room: the anchor sits inside the start room it spawned in
    // (start rooms are placed centered on their anchor).
    let catalog = app.world().resource::<CatalogRes>();
    let grid_size = catalog.grid_size() as f32;
    let tile = TilePos {
        x: (anchor_px.x / grid_size).floor() as i32,
        y: (anchor_px.y / grid_size).floor() as i32,
    };
    let here = layout.room_at(catalog, tile);
    assert!(
        here.is_some(),
        "the anchor's own tile must resolve to a room via the point query"
    );

    // The start room was placed centered on the anchor's ORIGINAL position
    // (the origin); the moved anchor has since wandered east. Both facts
    // are visible through the same query at different tiles.
    let origin_room = layout.room_at(catalog, TilePos { x: 0, y: 0 });
    assert_eq!(
        origin_room, here,
        "the moved anchor's tile and the start tile should still be the same room"
    );

    // Layout invariants over the whole streamed dungeon (same assertions as
    // generator/place.rs's streamed test, but against everything the real
    // plugin produced across 300 frames):
    //   1. footprints pairwise disjoint — no room was ever placed on
    //      another;
    //   2. every open door's reservation is free of rooms and of every
    //      other open door's reservation — no placement sealed a door shut,
    //      no two open doors compete for the same space, and no room was
    //      born with a door opening into another room's interior.
    let placements: Vec<(RoomId, TilePos)> =
        layout.iter().map(|(_, p)| (p.room, p.origin)).collect();
    for (i, &(room_a, origin_a)) in placements.iter().enumerate() {
        let def_a = catalog.get(room_a).expect("placed room must exist");
        let rect_a = TileRect {
            min: origin_a,
            size: def_a.size,
        };
        for &(room_b, origin_b) in placements.iter().skip(i + 1) {
            let def_b = catalog.get(room_b).expect("placed room must exist");
            let rect_b = TileRect {
                min: origin_b,
                size: def_b.size,
            };
            assert!(
                !overlaps(rect_a, rect_b),
                "rooms must never overlap: {rect_a:?} vs {rect_b:?}"
            );
        }
    }

    let mut reserved: Vec<TileRect> = Vec::new();
    for (_, open) in layout.open_doors().collect::<Vec<_>>() {
        let pos = open_door_pos(layout, catalog, *open).expect("open door of a live placement");
        let placement = layout.get(open.placement).expect("live placement");
        let def = catalog.get(placement.room).expect("live room");
        let door = def.doors.get(open.door as usize).expect("live door");
        let Some(area) = catalog.door_reservation(pos, door.dir, door.width) else {
            continue;
        };
        for (_, other) in layout.iter() {
            let other_def = catalog.get(other.room).expect("placed room");
            assert!(
                !overlaps(
                    area,
                    TileRect {
                        min: other.origin,
                        size: other_def.size,
                    },
                ),
                "open door's reservation at {pos:?} is occupied — the door was sealed"
            );
        }
        for &earlier in &reserved {
            assert!(
                !overlaps(area, earlier),
                "open door's reservation at {pos:?} overlaps another open door's"
            );
        }
        reserved.push(area);
    }
    let open_doors = reserved.len();
    assert!(
        open_doors > 0,
        "a streaming dungeon must still have open doors ahead of the anchor"
    );
}

/// Room area (tiles) of the room roots spawned each frame.
#[derive(Resource, Default)]
struct SpawnedPerFrame(Vec<Vec<u32>>);

fn record_spawn_area(
    added: Query<&Name, Added<LevelSet>>,
    catalog: Res<CatalogRes>,
    mut frames: ResMut<SpawnedPerFrame>,
) {
    let areas = added
        .iter()
        .filter_map(|name| catalog.iter().find(|d| d.identifier == name.as_str()))
        .map(|d| d.size.width * d.size.height)
        .collect();
    frames.0.push(areas);
}

#[test]
fn fixed_spawn_budget_caps_tiles_per_frame() {
    const BUDGET: u32 = 100;
    let mut app = headless_app(
        GeneratorPlugin::new(MAP, 800.0)
            .with_seed(1234)
            .with_spawn_budget(SpawnBudget::Fixed(BUDGET)),
    );
    app.init_resource::<SpawnedPerFrame>()
        .add_systems(Last, record_spawn_area);
    for _ in 0..300 {
        app.update();
    }
    let frames = &app.world().resource::<SpawnedPerFrame>().0;
    let total: usize = frames.iter().map(Vec::len).sum();
    assert!(total > 20, "rooms must keep spawning, got {total}");
    for areas in frames {
        let sum: u32 = areas.iter().sum();
        assert!(
            sum <= BUDGET || areas.len() == 1,
            "frame spawned {areas:?} = {sum} tiles over a {BUDGET}-tile budget"
        );
    }
    assert_eq!(app.world().resource::<SpawnRate>().tiles_per_frame, BUDGET);
}
