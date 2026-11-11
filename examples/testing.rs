//! The debug harness: `DefaultPlugins` + `LdtkPlugin` + `GeneratorPlugin`, a
//! camera that *is* the anchor, and a menu of overlays for watching the
//! generator work.
//!
//! Run: `cargo run --example testing` (needs the devShell on NixOS — see
//! AGENTS.md). Pass a project path to try another map:
//! `cargo run --example testing -- other.ldtk`.
//!
//! Controls:
//! - **WASD / arrows** — move the camera, which is the `GenerationAnchor`
//! - **Mouse wheel** — zoom; panning speed scales with it
//! - **R** — throw the dungeon away and generate a new one around you;
//!   **Shift+R** regenerates the same seed (shown in the HUD)
//! - **F1** — show or hide the debug menu
//!
//! The menu:
//! - **Radius slider** — one value for every ring, in the plugin's default
//!   ratios: spawn at the value, despawn at 1.25×, generation at 2×. Tick
//!   **Separate rings** to swap it for three sliders, one per ring of the
//!   three `StreamingRadii` holds. Either way the values are written into
//!   the library resource, so they take effect next frame: **generation** (yellow — open doors inside it get
//!   filled; the "math" ring, pure Layout bookkeeping), **spawn** (green — a
//!   room whose nearest point is inside it gets entities) and **despawn**
//!   (red — a room entirely beyond it is unloaded). Between green and red is
//!   the hysteresis band, where a room keeps whatever state it is in. Drag
//!   red inside green and the plugin pushes it back out (and logs it): the
//!   drawn rings are the ones actually in force, read back from the
//!   resource.
//! - **Rings** — draw them, centred on the anchor.
//! - **Room bounds** — every placement's footprint, from the Layout. Rooms
//!   outside the green ring are placed but not loaded: math only.
//! - **Open door boxes** — every open door, and the box it reserves: the
//!   footprint of the smallest room that can always fill it
//!   (`Catalog::door_reservation`). No placement may touch a reservation it
//!   doesn't connect to, which is what keeps every open door fillable.
//! - **Halls only** (on by default) — `common::hall_weights`, the
//!   corridors-only weight table the `halls` example uses, which keeps the
//!   dungeon growing indefinitely; untick for the whole catalog, unweighted
//!   (regenerates either way).
//! - **Deterministic** — regenerate with `GenerationOrder::FromOrigin`: the
//!   dungeon starts at world (0, 0) and grows as a disc (cyan ring, centred
//!   there) whose radius is the furthest you've been plus the generation
//!   ring. Any route through the same seed yields the same dungeon.
//!
//! The HUD shows generation throughput: rooms placed this frame, the peak
//! frame over the last second, and rooms placed and loaded per second —
//! counted from `RoomPlaced`/`RoomSpawned`, the messages any consumer gets.
//!
//! Every overlay is drawn from `Layout` and `Catalog` alone — the resources a
//! consumer gets — because the point of an example is what the public API can
//! express, not what engine internals happen to hold.
//!
//! The rest of the set: `minimal` adds the output API (events, point → room),
//! `halls` adds a player and wall collision in a corridors-only dungeon,
//! `meta` adds meta-generation weights.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::feathers::FeathersPlugins;
use bevy::feathers::controls::{SliderProps, checkbox, slider};
use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::theme::{ThemedText, UiTheme};
use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::ui::Checked;
use bevy::ui_widgets::{
    SliderPrecision, SliderRange, SliderStep, SliderValue, checkbox_self_update, observe,
    slider_self_update,
};
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

mod common;

/// Starting rings, px, for a 16px-grid map: rooms are placed out to
/// `GENERATION`, loaded within `SPAWN`, unloaded beyond `DESPAWN`. Pixel
/// radii mean nothing without the tile size — 800px is 50 tiles at 16px but
/// 1.5 at 512px — so `scale_to_grid` multiplies these, and the slider range,
/// by `grid_size / 16` once the map has loaded.
const GENERATION: f32 = 1600.0;
const SPAWN: f32 = 800.0;
const DESPAWN: f32 = 1000.0;

/// The single slider's ring ratios over the spawn ring — the same ones
/// `GeneratorPlugin::new` uses.
const GENERATION_RATIO: f32 = GENERATION / SPAWN;
const DESPAWN_RATIO: f32 = DESPAWN / SPAWN;

/// Every ring slider's range, px (16px grid; scaled like the rings).
const RADIUS_MIN: f32 = 50.0;
const RADIUS_MAX: f32 = 6000.0;

/// Zoom limits, as orthographic projection scale (1 = one world px per
/// screen px).
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 16.0;

fn main() {
    // No seed: this one is the smoke test, so a random dungeon each run is the
    // point. The demos with a fixed seed say so in their own `main`.
    info!("testing: random seed — press R for a fresh dungeon, F1 for the menu");
    App::new()
        .add_plugins((
            DefaultPlugins.set(ImagePlugin::default_nearest()),
            FeathersPlugins,
            FrameTimeDiagnosticsPlugin::default(),
        ))
        .insert_resource(UiTheme(create_dark_theme()))
        .add_plugins(LdtkPlugin)
        .add_plugins(
            GeneratorPlugin::new(common::map_path(), SPAWN)
                .with_generation_radius(GENERATION)
                .with_culling(CullingPolicy::Radius {
                    spawn: SPAWN,
                    despawn: DESPAWN,
                }),
        )
        .init_resource::<Overlays>()
        .init_resource::<Throughput>()
        .add_systems(Startup, (spawn, spawn_menu))
        .add_systems(OnEnter(GenerationState::Ready), scale_to_grid)
        .add_systems(
            Update,
            weights.pipe(apply_weights).in_set(GenerationSet::Weights),
        )
        .add_systems(
            Update,
            (
                pan_and_zoom,
                common::regenerate_on_key,
                common::show_generation_failure,
                toggle_menu,
                read_menu,
                draw_overlays,
                (count_throughput, update_hud).chain(),
            ),
        )
        .run();
}

fn spawn(mut commands: Commands) {
    commands.spawn((Camera2d, GenerationAnchor));
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: 16.0,
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

/// Which overlays are on, mirrored from the menu's checkboxes every frame.
#[derive(Resource)]
struct Overlays {
    circles: bool,
    rooms: bool,
    doors: bool,
    halls: bool,
    deterministic: bool,
    separate: bool,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            circles: true,
            rooms: false,
            doors: true,
            // Corridors-only by default: the bundled catalog's big rooms
            // are mostly dead ends, so unweighted dungeons often close off,
            // while the halls table keeps the frontier growing.
            halls: true,
            deterministic: false,
            separate: false,
        }
    }
}

/// Tags each checkbox with the overlay it controls.
#[derive(Component, Clone, Copy)]
enum Toggle {
    Circles,
    Rooms,
    Doors,
    Halls,
    Deterministic,
    Separate,
}

#[derive(Component)]
struct Menu;

/// The two slider groups; one is shown at a time.
#[derive(Component)]
struct AllRingsGroup;
#[derive(Component)]
struct SeparateRingsGroup;

/// Tags each slider with the `StreamingRadii` ring it sets.
#[derive(Component, Clone, Copy)]
enum Ring {
    /// The single slider: every ring, in the default ratios.
    All,
    Generation,
    Spawn,
    Despawn,
}

fn spawn_menu(mut commands: Commands, overlays: Res<Overlays>) {
    let mut check = |toggle: Toggle, label: &'static str, on: bool| {
        let mut entity = commands.spawn((
            checkbox(toggle, Spawn((Text::new(label), ThemedText))),
            observe(checkbox_self_update),
        ));
        if on {
            entity.insert(Checked);
        }
        entity.id()
    };
    let boxes = [
        check(Toggle::Circles, "Rings", overlays.circles),
        check(Toggle::Rooms, "Room bounds", overlays.rooms),
        check(Toggle::Doors, "Open door boxes", overlays.doors),
        check(Toggle::Halls, "Halls only (regenerates)", overlays.halls),
        check(
            Toggle::Deterministic,
            "Deterministic (regenerates)",
            overlays.deterministic,
        ),
    ];
    let separate = check(Toggle::Separate, "Separate rings", overlays.separate);

    // A labelled ring slider, as two entities: label, then slider.
    let mut ring_slider = |ring: Ring, label: &'static str, value: f32| {
        let label = commands.spawn((Text::new(label), ThemedText)).id();
        let slider = commands
            .spawn((
                slider(
                    SliderProps {
                        value,
                        min: RADIUS_MIN,
                        max: RADIUS_MAX,
                    },
                    // `SliderPrecision` is what feathers' label and fill
                    // update query for — without it the widget never
                    // redraws.
                    (SliderStep(50.0), SliderPrecision(0), ring),
                ),
                observe(slider_self_update),
            ))
            .id();
        [label, slider]
    };
    let all = ring_slider(Ring::All, "Radius, px (all rings)", SPAWN);
    let mut each = Vec::new();
    for (ring, label, value) in [
        (Ring::Generation, "Generation ring, px (yellow)", GENERATION),
        (Ring::Spawn, "Spawn ring, px (green)", SPAWN),
        (Ring::Despawn, "Despawn ring, px (red)", DESPAWN),
    ] {
        each.extend(ring_slider(ring, label, value));
    }

    let group = |visible: bool| Node {
        display: if visible {
            Display::Flex
        } else {
            Display::None
        },
        flex_direction: FlexDirection::Column,
        row_gap: Val::Px(6.0),
        ..default()
    };
    let all_group = commands
        .spawn((AllRingsGroup, group(!overlays.separate)))
        .add_children(&all)
        .id();
    let each_group = commands
        .spawn((SeparateRingsGroup, group(overlays.separate)))
        .add_children(&each)
        .id();

    commands
        .spawn((
            Menu,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(10.0),
                right: Val::Px(10.0),
                width: Val::Px(260.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(10.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.1, 0.1, 0.12, 0.85)),
        ))
        .add_children(&[all_group, each_group, separate])
        .add_children(&boxes);
}

/// Rescales every ring slider for the loaded map's tile size (see
/// `SPAWN`). Writing the values goes through `read_menu` like any slider
/// move, so `StreamingRadii` follows next frame.
fn scale_to_grid(
    catalog: Res<CatalogRes>,
    sliders: Query<(Entity, &Ring)>,
    mut commands: Commands,
) {
    #[allow(clippy::cast_precision_loss)] // tile sizes are small
    let scale = catalog.grid_size() as f32 / 16.0;
    if (scale - 1.0).abs() < f32::EPSILON {
        return;
    }
    for (entity, ring) in sliders.iter() {
        let value = match ring {
            Ring::All | Ring::Spawn => SPAWN,
            Ring::Generation => GENERATION,
            Ring::Despawn => DESPAWN,
        };
        commands.entity(entity).insert((
            SliderRange::new(RADIUS_MIN * scale, RADIUS_MAX * scale),
            SliderStep(50.0 * scale),
            SliderValue(value * scale),
        ));
    }
}

fn toggle_menu(keys: Res<ButtonInput<KeyCode>>, mut menu: Query<&mut Visibility, With<Menu>>) {
    if !keys.just_pressed(KeyCode::F1) {
        return;
    }
    for mut visibility in menu.iter_mut() {
        *visibility = match *visibility {
            Visibility::Hidden => Visibility::Inherited,
            _ => Visibility::Hidden,
        };
    }
}

/// Mirrors the widgets into `Overlays`, `StreamingRadii` and
/// `GenerationOrder`. Toggling "halls only" or "deterministic" regenerates:
/// weights only shape rooms placed from then on, and a dungeon is only
/// deterministic if it was generated that way from its first room.
#[allow(clippy::too_many_arguments)] // a Bevy system's parameters are its inputs
fn read_menu(
    boxes: Query<(&Toggle, Has<Checked>)>,
    sliders: Query<(Entity, &Ring, Ref<SliderValue>)>,
    mut all_group: Query<&mut Node, (With<AllRingsGroup>, Without<SeparateRingsGroup>)>,
    mut each_group: Query<&mut Node, (With<SeparateRingsGroup>, Without<AllRingsGroup>)>,
    mut overlays: ResMut<Overlays>,
    mut radii: ResMut<StreamingRadii>,
    mut order: ResMut<GenerationOrder>,
    mut regenerate: MessageWriter<Regenerate>,
    mut commands: Commands,
) {
    let mut regen = false;
    let mut switched = false;
    for (toggle, on) in boxes.iter() {
        match toggle {
            Toggle::Circles => overlays.circles = on,
            Toggle::Rooms => overlays.rooms = on,
            Toggle::Doors => overlays.doors = on,
            Toggle::Halls => {
                regen |= overlays.halls != on;
                overlays.halls = on;
            }
            Toggle::Deterministic => {
                regen |= overlays.deterministic != on;
                overlays.deterministic = on;
                *order = if on {
                    GenerationOrder::FromOrigin
                } else {
                    GenerationOrder::NearestAnchor
                };
            }
            Toggle::Separate => {
                switched = overlays.separate != on;
                overlays.separate = on;
            }
        }
    }
    if regen {
        regenerate.write(Regenerate::default());
    }

    if switched {
        // Show the other group, and carry the rings in force over to its
        // sliders, so switching modes doesn't jump the rings. Writing the
        // values marks them changed, which applies them next frame.
        if let Ok(mut node) = all_group.single_mut() {
            node.display = if overlays.separate {
                Display::None
            } else {
                Display::Flex
            };
        }
        if let Ok(mut node) = each_group.single_mut() {
            node.display = if overlays.separate {
                Display::Flex
            } else {
                Display::None
            };
        }
        let spawn = radii.culling.spawn_radius().unwrap_or(SPAWN);
        for (entity, ring, _) in sliders.iter() {
            let value = match ring {
                Ring::All => spawn,
                Ring::Generation => radii.generation_radius,
                Ring::Spawn => spawn,
                Ring::Despawn => radii.culling.despawn_radius().unwrap_or(DESPAWN),
            };
            commands.entity(entity).insert(SliderValue(value));
        }
        return;
    }

    // Recompute from the active sliders whenever one of them moved: the
    // plugin may have pushed the despawn ring out, and a later change
    // should re-derive from what the sliders say, not from that correction.
    let active = |ring: &Ring| matches!(ring, Ring::All) != overlays.separate;
    let moved = sliders
        .iter()
        .any(|(_, ring, value)| active(ring) && value.is_changed() && !value.is_added());
    if !moved {
        return;
    }
    let (mut generation, mut spawn, mut despawn) = (GENERATION, SPAWN, DESPAWN);
    for (_, ring, value) in sliders.iter().filter(|(_, ring, _)| active(ring)) {
        match ring {
            Ring::All => {
                spawn = value.0;
                generation = value.0 * GENERATION_RATIO;
                despawn = value.0 * DESPAWN_RATIO;
            }
            Ring::Generation => generation = value.0,
            Ring::Spawn => spawn = value.0,
            Ring::Despawn => despawn = value.0,
        }
    }
    *radii = StreamingRadii {
        generation_radius: generation,
        culling: CullingPolicy::Radius { spawn, despawn },
    };
}

/// The weight table: halls-only when toggled, otherwise none (an empty table
/// means every room weighs its own base weight).
fn weights(catalog: Res<CatalogRes>, overlays: Res<Overlays>) -> Vec<f32> {
    if overlays.halls {
        common::hall_weights(&catalog)
    } else {
        Vec::new()
    }
}

/// WASD pans, the mouse wheel zooms. Pan speed scales with zoom, so crossing
/// the screen takes the same time at any zoom level.
fn pan_and_zoom(
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    mut camera: Query<(&mut Transform, &mut Projection), With<Camera>>,
) {
    let Ok((mut transform, mut projection)) = camera.single_mut() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection.as_mut() else {
        return;
    };
    // Lines from a wheel, pixels from a touchpad: normalise to notches.
    let notches = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 32.0,
    };
    if notches.abs() > f32::EPSILON {
        ortho.scale = (ortho.scale * 0.85_f32.powf(notches)).clamp(MIN_ZOOM, MAX_ZOOM);
    }
    let direction = common::wasd_direction(&keys);
    transform.translation +=
        direction.extend(0.0) * common::WALK_SPEED * ortho.scale * time.delta_secs();
}

#[allow(clippy::cast_precision_loss)] // tile coordinates are small
fn draw_overlays(
    overlays: Res<Overlays>,
    radii: Res<StreamingRadii>,
    order: Res<GenerationOrder>,
    reach: Res<OriginReach>,
    catalog: Res<CatalogRes>,
    layout: Res<LayoutRes>,
    anchor: Query<&GlobalTransform, With<GenerationAnchor>>,
    mut gizmos: Gizmos,
) {
    let grid = catalog.grid_size() as f32;
    let tile_rect = |rect: TileRect| {
        let size = Vec2::new(rect.size.width as f32, rect.size.height as f32) * grid;
        let min = Vec2::new(rect.min.x as f32, rect.min.y as f32) * grid;
        (min + size / 2.0, size)
    };

    if overlays.circles
        && let Ok(anchor) = anchor.single()
    {
        let center = anchor.translation().xy();
        let mut circle = |radius: f32, color: Color| {
            gizmos.circle_2d(center, radius, color).resolution(128);
        };
        // Under `FromOrigin` generation doesn't follow the anchor — its ring
        // is the origin disc, drawn below.
        if *order == GenerationOrder::NearestAnchor {
            circle(radii.generation_radius, Color::srgb(1.0, 0.85, 0.2));
        }
        if let Some(radius) = radii.culling.spawn_radius() {
            circle(radius, Color::srgb(0.3, 1.0, 0.4));
        }
        if let Some(radius) = radii.culling.despawn_radius() {
            circle(radius, Color::srgb(1.0, 0.3, 0.3));
        }
    }
    if overlays.circles && *order == GenerationOrder::FromOrigin {
        let cyan = Color::srgb(0.3, 0.9, 1.0);
        gizmos
            .circle_2d(Vec2::ZERO, reach.radius, cyan)
            .resolution(256);
        gizmos.cross_2d(Vec2::ZERO, grid * 2.0, cyan);
    }

    if overlays.rooms {
        for (_, placement) in layout.iter() {
            let Some(def) = catalog.get(placement.room) else {
                continue;
            };
            let (center, size) = tile_rect(TileRect {
                min: placement.origin,
                size: def.size,
            });
            gizmos.rect_2d(center, size, Color::srgba(0.6, 0.65, 0.85, 0.6));
        }
    }

    if overlays.doors {
        for (_, open) in layout.open_doors() {
            let Some(door) = layout
                .get(open.placement)
                .and_then(|p| catalog.get(p.room))
                .and_then(|def| def.doors.get(open.door as usize))
            else {
                continue;
            };
            let Some(pos) = open_door_pos(&layout, &catalog, *open) else {
                continue;
            };
            // The door's own tile, then the space it reserves beyond it.
            let (center, _) = tile_rect(TileRect {
                min: pos,
                size: TileSize {
                    width: 1,
                    height: 1,
                },
            });
            gizmos.circle_2d(center, grid * 0.45, Color::srgb(1.0, 0.45, 0.2));
            if let Some(reserved) = catalog.door_reservation(pos, door.dir, door.width) {
                let (center, size) = tile_rect(reserved);
                gizmos.rect_2d(center, size, Color::srgb(1.0, 0.45, 0.2));
            }
        }
    }
}

#[derive(Component)]
struct HudText;

/// Rooms placed and loaded, per frame and over a trailing second.
#[derive(Resource, Default)]
struct Throughput {
    /// Rooms placed this frame.
    frame: usize,
    /// `(time, placed, loaded)` for every frame in the last second.
    window: std::collections::VecDeque<(f32, usize, usize)>,
}

impl Throughput {
    fn per_second(&self) -> (usize, usize) {
        self.window
            .iter()
            .fold((0, 0), |(p, l), &(_, placed, loaded)| {
                (p + placed, l + loaded)
            })
    }

    fn peak_frame(&self) -> usize {
        self.window
            .iter()
            .map(|&(_, placed, _)| placed)
            .max()
            .unwrap_or(0)
    }
}

fn count_throughput(
    time: Res<Time>,
    mut placed: MessageReader<RoomPlaced>,
    mut loaded: MessageReader<RoomSpawned>,
    mut throughput: ResMut<Throughput>,
) {
    let now = time.elapsed_secs();
    let placed = placed.read().count();
    let loaded = loaded.read().count();
    throughput.frame = placed;
    throughput.window.push_back((now, placed, loaded));
    while throughput
        .window
        .front()
        .is_some_and(|&(at, _, _)| now - at > 1.0)
    {
        throughput.window.pop_front();
    }
}

fn update_hud(
    throughput: Res<Throughput>,
    seed: Res<GenerationSeed>,
    state: Res<State<GenerationState>>,
    order: Res<GenerationOrder>,
    reach: Res<OriginReach>,
    layout: Res<LayoutRes>,
    diagnostics: Res<DiagnosticsStore>,
    rooms: Query<(), With<LevelIid>>,
    camera: Query<&Projection, With<Camera>>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|fps| fps.smoothed())
        .unwrap_or(0.0);
    let zoom = match camera.single() {
        Ok(Projection::Orthographic(ortho)) => ortho.scale,
        _ => 1.0,
    };
    let (placed_per_s, loaded_per_s) = throughput.per_second();
    // Every number right-aligned in a fixed width (the default font is
    // monospace), so text after a number doesn't jump as its digit count
    // changes.
    text.0 = format!(
        "FPS: {fps:>4.0}   seed: {}   state: {:?}   order: {:?}{}\n\
         rooms: {:>7} placed, {:>5} loaded   open doors: {:>5}   zoom: {zoom:>5.2}\n\
         placing: {:>3} this frame, peak {:>3}/frame, {placed_per_s:>6}/s   \
         loading: {loaded_per_s:>4}/s\n\
         WASD move · wheel zoom · R regenerate · Shift+R same seed · F1 menu",
        seed.0,
        state.get(),
        *order,
        if *order == GenerationOrder::FromOrigin {
            format!(" (reach {:>7.0}px)", reach.radius)
        } else {
            String::new()
        },
        layout.placed_count(),
        rooms.iter().count(),
        layout.open_doors().count(),
        throughput.frame,
        throughput.peak_frame(),
    );
}
