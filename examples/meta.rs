//! Meta-generation demo (DESIGN.md §9): a bounded dungeon that starts
//! hallway-heavy and turns room-heavy as it approaches its ceiling, with
//! the ceiling closed off gracefully by closure bias instead of being cut
//! off mid-expansion.
//!
//! Run: `cargo run --example meta` (needs the devShell on NixOS — see
//! AGENTS.md).
//!
//! What to watch for on the HUD:
//! - **phase** flips EXPLORE → CLOSE at 80% of the ceiling — that's the
//!   consumer weight table switching, the per-frame half of meta-generation;
//! - **rooms** climbs to the ceiling and stops — the engine's closure bias
//!   (per-placement half) steers the frontier into single-door caps so the
//!   dungeon *finishes* rather than truncating;
//! - **open doors** should settle low — those are the frontier ahead of you;
//!   walk toward one and generation resumes around it.
//!
//! The two halves and why they're split that way:
//! - the consumer system (`phase_weights`) runs **once per frame** and
//!   returns a complete weight table — external/phase factors, cheap to
//!   recompute, nothing to reset between frames;
//! - the engine (`candidate_weight` in the library) adjusts **per
//!   placement** — closure bias, multi-door bonus — because a frame places
//!   many rooms and a table has no memory between them.
//!
//! Note the bundled catalog's rooms carry no tags, so this demo shapes
//! texture by *door count* (long narrow rooms are 6-door connectors) —
//! tag-based shaping (`tag_max`, `tag_min_depth`) needs a project authored
//! with the `RoomTags` schema, and is exercised in the library's unit tests
//! via synthetic definitions.

use bevy::prelude::*;
use bevy_ecs_ldtk::prelude::*;

use bevy_ldtk_procgen::prelude::*;

/// Rooms the dungeon may place. A ceiling, not a target: generation stops
/// near it, with the frontier capped rather than truncated.
const CEILING: usize = 60;

/// Fraction of the ceiling after which the consumer phase flips to CLOSE —
/// deliberately the same 0.8 at which the engine's closure bias engages, so
/// both halves push the same direction at the same time.
const CLOSE_PHASE_START: f32 = 0.8;

/// Multiplier for high-door-count rooms during the EXPLORE phase: long
/// multi-door connectors spread the dungeon outward, producing the long
/// spine the demo is named for. Low-door rooms are damped below 1.0
/// relative to it, not excluded — exclusion is the engine's job.
const HALLWAY_PUSH: f32 = 3.0;

/// Multiplier for high-door rooms during CLOSE: under 1.0 to help the
/// engine's bias shrink the frontier.
const HALLWAY_DAMP: f32 = 0.3;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))
        .add_plugins(LdtkPlugin)
        .add_plugins(
            GeneratorPlugin::new(
                "ezraclintoc_kenney-tiny-dungeon_16px_fixeddoors.ldtk",
                800.0,
            )
            .with_seed(424242)
            .with_max_rooms(CEILING, CountBy::All),
        )
        .add_systems(Startup, (spawn_camera, spawn_hud))
        .add_systems(
            Update,
            (
                // The consumer half: phase weights, once per frame, piped
                // into the library's sink inside GenerationSet::Weights —
                // the declared ordering that keeps this from racing
                // `generate` for the RoomWeights resource (ARCHITECTURE.md
                // §7).
                phase_weights
                    .pipe(apply_weights)
                    .in_set(GenerationSet::Weights),
                drift_anchor,
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

/// Which phase the consumer table is in — mirrored on the HUD.
#[derive(Resource, Default, Debug, PartialEq, Clone, Copy)]
enum Phase {
    #[default]
    Explore,
    Close,
}

/// The consumer's once-per-frame weight system (DESIGN.md §9's
/// `my_weights`): returns a **complete** table, entry `i` for `RoomId(i)`.
/// Returning the whole table every frame (rather than mutating one) means
/// there is no stale state to reset — a room the phase no longer favours
/// simply gets its base weight again.
///
/// Door counts come from the catalog (setup-time data, cheap to read);
/// the *phase* is external state this system owns.
#[allow(clippy::needless_pass_by_value)] // Bevy system parameters
fn phase_weights(
    catalog: Res<CatalogRes>,
    layout: Res<LayoutRes>,
    mut phase: ResMut<Phase>,
    mut cache: Local<Vec<f32>>,
) -> Vec<f32> {
    let max = if CEILING == 0 { 1 } else { CEILING };
    // Intentionally-lossy cast, warn-level lint: room counts are tiny
    // relative to f32's exact-integer range.
    #[allow(clippy::cast_precision_loss)]
    let fraction = layout.placed_count() as f32 / max as f32;
    *phase = if fraction >= CLOSE_PHASE_START {
        Phase::Close
    } else {
        Phase::Explore
    };

    // Reuse the allocation across frames — Local<Vec> persists.
    cache.clear();
    cache.extend(catalog.iter().map(|def| {
        // 3+ door rooms are the long connectors; single/double-door rooms
        // are ordinary chambers and caps. `connector` ramps 0.0 → 1.0 over
        // 1..=3 doors, so EXPLORE pushes multi-door rooms up to
        // HALLWAY_PUSH and CLOSE damps them toward HALLWAY_DAMP — while
        // caps and chambers keep weight ≈ 1.0 in both phases.
        #[allow(clippy::cast_precision_loss)] // tiny counts; see above
        let doors = def.doors.len().max(1) as f32;
        let connector = (doors / 3.0).min(1.0);
        match *phase {
            Phase::Explore => 1.0 + connector * (HALLWAY_PUSH - 1.0),
            Phase::Close => 1.0 + connector * (HALLWAY_DAMP - 1.0),
        }
    }));
    cache.clone()
}

/// Idle drift so the demo shows streaming without input: the anchor wanders
/// east at a steady clip, outrunning the dungeon edge and pulling new rooms
/// in ahead of it. Replace with the `full` example's WASD player for
/// interactive use. `Res<Time>` is a Bevy system parameter, taken by value.
#[allow(clippy::needless_pass_by_value)]
fn drift_anchor(time: Res<Time>, mut anchor: Query<&mut Transform, With<GenerationAnchor>>) {
    let Ok(mut transform) = anchor.single_mut() else {
        return;
    };
    transform.translation.x += 60.0 * time.delta_secs();
}

/// `MessageReader` must be taken by value as a system parameter.
#[allow(clippy::needless_pass_by_value)]
fn warn_on_abandoned_doors(mut abandoned: MessageReader<DoorAbandoned>) {
    for event in abandoned.read() {
        warn!(
            "door at {:?} facing {:?} could not be filled",
            event.at, event.dir
        );
    }
}

/// `MessageReader` must be taken by value as a system parameter.
#[allow(clippy::needless_pass_by_value)]
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
        Text::new(""),
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

/// `Res<LayoutRes>`/`Res<Phase>` are consumed only via interior reads here;
/// taking them by value is the Bevy system-param idiom.
#[allow(clippy::needless_pass_by_value)]
fn update_hud(
    phase: Res<Phase>,
    state: Res<State<GenerationState>>,
    layout: Res<LayoutRes>,
    mut hud: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = hud.single_mut() else {
        return;
    };

    let open = layout.open_doors().count();
    let rooms = layout.placed_count();
    text.0 = format!(
        "state: {:?}  phase: {:?}\nrooms: {rooms} / {CEILING}  open doors: {open}\n(ceiling closes via engine closure bias)",
        state.get(),
        *phase,
    );
}
