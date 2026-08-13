//! Security, gone bad.
//!
//! [`crate::security`] is neutral: it only ever responds to suspicion the
//! [`crate::antagonist`] thread built. This is the opposite kind of threat —
//! an officer who preys on the chemist directly, no illicit delivery
//! required. It arms off the same [`Department::Security`] standing every
//! ordinary delivery already moves, rather than a bespoke hidden counter:
//! treat Security badly (botched deliveries, a raid or two) and their
//! standing sinks low enough to turn one of their own against you; treat
//! them well for long enough afterwards and they turn back, with a reward.
//!
//! Standing is the only gate, in both directions — the reward is not an
//! ending. Let Security's opinion sour a second time and the visits resume,
//! which is what the [`Deterrent`] earned on the way out is *for*. The reward
//! itself is still once per career.
//!
//! A visit is a shakedown: hand over product to make them go away cheaply,
//! or refuse and pay a steeper standing cost — occasionally, if you refuse
//! while things are already bad, a real one. That is the one new thing this
//! module adds to the whole game: the first time an NPC ever calls
//! [`chem_sim::Vitals::apply`] on the player rather than the player doing it
//! to themselves. It reuses `body::handle_collapse`/`MedbayRetrieval`
//! entirely unmodified by going through the same replicated [`Body`] — there
//! is exactly one new call site, in [`expire_rogue_encounters`].

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::{Damage, DamageKind, ReagentId, Units};
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::body::{ApplyHeldRequested, Body};
use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy};
use crate::crew::{spawn_crew_member, CrewDef, CrewPhase, CrewRoute};
use crate::hazards::{HazardFelt, HazardKind};
use crate::interaction::{InteractRequested, Interactable};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::orders::{Department, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

/// Seconds before the very first check, before `Shift` has moved at all.
/// Harmless to make short — nothing spawns until standing is already bad.
const INITIAL_GAP_SECONDS: (f32, f32) = (30.0, 60.0);

/// How long a visit waits at the counter before a non-answer counts as a
/// refusal. Its own range rather than reusing the ordinary difficulty's —
/// unlike an antagonist's illicit visit, there is nothing to hide here, so
/// there is no "statistical tell" to avoid.
const ROGUE_PATIENCE_SECONDS: (f32, f32) = (45.0, 75.0);

/// Standing cost of refusing a shakedown outright.
const ROGUE_REFUSE_PENALTY: i32 = -3;
/// Standing cost of paying up. Smaller than refusing, and never positive —
/// paying a corrupt officer never earns trust, only avoids the worse cost.
const ROGUE_COMPLY_PENALTY: i32 = -1;
/// A shove/baton hit, not violence meant to be lethal — survivable several
/// times over from full health.
const ROGUE_ASSAULT_BRUTE: i32 = 20;
/// How many uses the reward is worth before it is spent.
const DETERRENT_CHARGES: u32 = 2;

pub struct RogueSecurityPlugin;

impl Plugin for RogueSecurityPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<RogueSecurityScript>::new(&[
            "rogue_security.ron",
        ]))
        .init_resource::<RogueRedeemed>()
        .add_systems(Startup, start_loading)
        .add_systems(
            Update,
            (
                (
                    promote_script,
                    schedule_rogue_encounter,
                    handle_rogue_delivery,
                    expire_rogue_encounters,
                    handle_deterrent_use,
                    check_redemption,
                )
                    .chain()
                    .run_if(is_authority),
                // Presentation, everywhere: the reward prop needs a mesh on
                // whichever peer it appears for, same split every other
                // pickable thing in the lab already uses.
                dress_deterrent,
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

// ---------------------------------------------------------------------------
// Hidden-ish state
// ---------------------------------------------------------------------------

/// Whether the reward has already been earned. Not itself secret — there is
/// nothing to hide about Security's disposition, unlike the antagonist
/// thread — but a one-shot flag so a standing that dips back down and climbs
/// again can never spawn a second [`Deterrent`]. Persisted: this is a career
/// fact, the same as [`crate::antagonist::UnderworldStanding`].
#[derive(Resource, Default, Clone, Copy)]
pub struct RogueRedeemed(pub bool);

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.rogue_security.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct RogueSecurityScript {
    /// The one recurring officer — a name, not a role drawn from the roster,
    /// so every visit reads as the same person turning up again.
    pub officer_name: String,
    pub gap_multiplier: (f32, f32),
    /// At or below this [`Department::Security`] standing, visits start
    /// arming. Above it, this whole module is silent. Checked live every time,
    /// so a career that sours again after being redeemed re-arms here.
    pub hostile_below: i32,
    /// At or above this standing, the reward fires — once per career, bounded
    /// by [`RogueRedeemed`]. It does not disarm the visits: only standing
    /// climbing back above `hostile_below` does that, and only for as long as
    /// it stays there.
    pub redeemed_at: i32,
    /// Chance a refused, `physical: true` encounter actually turns violent —
    /// the third of three independent gates (see
    /// [`expire_rogue_encounters`]), so the real in-practice frequency is a
    /// content dial, not a code change.
    pub physical_chance: f64,
    pub encounters: Vec<RogueEncounterDef>,
    pub redemption_line: String,
    pub reward_line: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RogueEncounterDef {
    pub reagent: String,
    pub amounts: Vec<u32>,
    pub pretext: String,
    pub refusal_line: String,
    pub compliance_line: String,
    /// Whether refusing this one can ever turn physical. Most should not —
    /// see the module doc.
    #[serde(default)]
    pub physical: bool,
}

#[derive(Resource)]
struct PendingRogueSecurityScript(Handle<RogueSecurityScript>);

#[derive(Resource, Deref)]
struct Script(RogueSecurityScript);

#[derive(Resource)]
struct RogueSpawner {
    timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingRogueSecurityScript(
        assets.load("data/station.rogue_security.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingRogueSecurityScript>>,
    mut scripts: ResMut<Assets<RogueSecurityScript>>,
    spawner: Option<Res<RogueSpawner>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    if spawner.is_none() {
        let gap = rand::rng().random_range(INITIAL_GAP_SECONDS.0..=INITIAL_GAP_SECONDS.1);
        commands.insert_resource(RogueSpawner {
            timer: Timer::from_seconds(gap, TimerMode::Once),
        });
    }
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingRogueSecurityScript>();
}

// ---------------------------------------------------------------------------
// The officer
// ---------------------------------------------------------------------------

/// Marks the shakedown visit and carries its demand. Built inline from a
/// synthetic [`CrewDef`] exactly like `security::RaidOfficer` — never drawn
/// from `station.crew.ron`, so the legitimate roster's own picks can never
/// select "the rogue officer" as a requester.
#[derive(Component)]
struct RogueOfficer {
    reagent: ReagentId,
    amount: Units,
    patience: f32,
    waited: f32,
    physical: bool,
}

#[allow(clippy::too_many_arguments)]
fn schedule_rogue_encounter(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<RogueSpawner>>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    active: Query<(), With<RogueOfficer>>,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    // Deliberately not gated on `RogueRedeemed`. Standing is the only gate —
    // see the check below — so losing Security's trust a second time brings
    // them back, and the [`Deterrent`] earned the first time round is the
    // insurance against exactly that. Disarming permanently here is what used
    // to make the reward unreachable by construction: it could only appear at
    // `redeemed_at`, and appearing guaranteed no officer would ever exist to
    // point it at. `RogueRedeemed` still bounds the *reward* to one per career
    // — see [`check_redemption`] and the flag's own doc comment.
    if !shift.accepting_orders || !active.is_empty() {
        return;
    }
    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    let rules = current_rules(&station.config, &shift);
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let multiplier = rng.random_range(script.gap_multiplier.0..=script.gap_multiplier.1);
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    // The gate: silence unless Security standing has actually soured.
    if shift.standing(Department::Security) > script.hostile_below {
        return;
    }

    let Some(encounter) = script.encounters.choose(&mut rng) else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&encounter.reagent) else {
        warn!("rogue security encounter names unknown reagent '{}'", encounter.reagent);
        return;
    };
    let Some(&amount) = encounter.amounts.choose(&mut rng) else {
        return;
    };
    let patience = rng.random_range(ROGUE_PATIENCE_SECONDS.0..=ROGUE_PATIENCE_SECONDS.1);

    let officer_def = CrewDef {
        name: script.officer_name.clone(),
        role: "Security".to_string(),
        color: [0.55, 0.18, 0.20],
    };
    let officer = spawn_crew_member(&mut commands, &officer_def, 0.0);
    commands.entity(officer).insert((
        RogueOfficer {
            reagent,
            amount: Units::whole(amount as i32),
            patience,
            waited: 0.0,
            physical: encounter.physical,
        },
        Interactable::new(format!("{} — {}", script.officer_name, encounter.pretext)),
    ));

    radio.push(RadioEntry {
        channel: "SEC".to_string(),
        text: encounter.pretext.clone(),
        good: false,
    });

    info!(
        "rogue security: {} demands {}u {}",
        script.officer_name, amount, encounter.reagent
    );
}

/// Finds the encounter definition a live [`RogueOfficer`] was built from, by
/// its reagent — the one field that round-trips back to content.
fn encounter_for<'a>(script: &'a RogueSecurityScript, db: &ChemDb, reagent: ReagentId) -> Option<&'a RogueEncounterDef> {
    script
        .encounters
        .iter()
        .find(|encounter| db.reagents.id_of(&encounter.reagent) == Some(reagent))
}

/// Paying up. Reuses `Interactable`/`InteractRequested` exactly like an
/// ordinary delivery, but stands entirely apart from `orders::Order` and
/// `complete_delivery` — a shakedown is not a request the department ever
/// credits.
#[allow(clippy::too_many_arguments)]
fn handle_rogue_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    mut officers: Query<(&mut CrewRoute, &RogueOfficer)>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    containers: Query<(Entity, &Container, &HeldBy)>,
    chemists: Query<(Entity, &Chemist)>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((mut route, officer)) = officers.get_mut(request.target) else {
            continue;
        };
        let Some((container_entity, container, _)) = containers
            .iter()
            .find(|(_, _, holder)| holder.0 == player)
        else {
            continue;
        };
        if container.solution.volume_of(officer.reagent) < officer.amount {
            // Wrong or insufficient — nothing happens; the patience clock is
            // still the only thing that decides a refusal.
            continue;
        }

        shift.adjust(Department::Security, ROGUE_COMPLY_PENALTY);
        if let Some(script) = &script {
            if let Some(encounter) = encounter_for(script, &db, officer.reagent) {
                radio.push(RadioEntry {
                    channel: "SEC".to_string(),
                    text: encounter.compliance_line.clone(),
                    good: false,
                });
            }
        }

        commands.entity(container_entity).despawn();
        commands
            .entity(request.target)
            .remove::<RogueOfficer>()
            .remove::<Interactable>();
        route.leave();
    }
}

/// The patience clock, and the one place a refusal — and, rarely, a real
/// consequence — happens.
///
/// Three independent gates decide whether a refusal turns physical:
/// `encounter.physical`, Security standing already at or below
/// `hostile_below`, and a roll against `physical_chance`. A player who
/// always complies never reaches any of the three, so they can finish a
/// career having taken zero rogue damage — compliance is always available
/// and always strictly safer than refusal.
#[allow(clippy::too_many_arguments)]
fn expire_rogue_encounters(
    time: Res<Time>,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    mut commands: Commands,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
    mut officers: Query<(Entity, &mut RogueOfficer, &mut CrewRoute)>,
    chemists: Query<(Entity, &Chemist)>,
    mut bodies: Query<&mut Body>,
) {
    let Some(script) = script else {
        return;
    };
    let dt = time.delta_secs();

    for (entity, mut officer, mut route) in &mut officers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        officer.waited += dt;
        if officer.waited < officer.patience {
            continue;
        }

        shift.adjust(Department::Security, ROGUE_REFUSE_PENALTY);
        if let Some(encounter) = encounter_for(&script, &db, officer.reagent) {
            radio.push(RadioEntry {
                channel: "SEC".to_string(),
                text: encounter.refusal_line.clone(),
                good: false,
            });
        }

        if officer.physical
            && shift.standing(Department::Security) <= script.hostile_below
            && rand::rng().random_bool(script.physical_chance)
        {
            let mut rng = rand::rng();
            let candidates: Vec<(Entity, ClientId)> = chemists
                .iter()
                .map(|(entity, chemist)| (entity, chemist.client))
                .collect();
            if let Some(&(target, client)) = candidates.choose(&mut rng) {
                if let Ok(mut body) = bodies.get_mut(target) {
                    body.0
                        .apply(Damage::of(DamageKind::Brute, Units::whole(ROGUE_ASSAULT_BRUTE)));
                    felt.write(ToClients {
                        targets: SendTargets::Single(client),
                        message: HazardFelt {
                            kind: HazardKind::Assault,
                            strength: 0.8,
                        },
                    });
                }
            }
        }

        commands.entity(entity).remove::<RogueOfficer>().remove::<Interactable>();
        route.leave();
    }
}

// ---------------------------------------------------------------------------
// The reward
// ---------------------------------------------------------------------------

/// A defensive tool, not a weapon that deals damage back — the game has no
/// aim/targeting beyond the crosshair raycast, so a true offense system
/// would be a much larger addition than this module's one new damage call
/// site already is. Composes with [`HeldBy`] exactly like [`Container`] does
/// (`containers::carry_held_containers`/`handle_pickup` already treat any
/// held entity generically), without being one — it holds no `Solution` and
/// wants none of `ContainerKind`'s dose semantics.
#[derive(Component, Clone, Copy, Serialize, Deserialize)]
pub struct Deterrent {
    pub charges: u32,
}

fn check_redemption(
    mut commands: Commands,
    script: Option<Res<Script>>,
    shift: Res<Shift>,
    mut redeemed: ResMut<RogueRedeemed>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(script) = script else {
        return;
    };
    if redeemed.0 || shift.standing(Department::Security) < script.redeemed_at {
        return;
    }
    redeemed.0 = true;
    radio.push(RadioEntry {
        channel: "SEC".to_string(),
        text: script.redemption_line.clone(),
        good: true,
    });
    radio.push(RadioEntry {
        channel: "SEC".to_string(),
        text: script.reward_line.clone(),
        good: true,
    });

    commands.spawn((
        Deterrent {
            charges: DETERRENT_CHARGES,
        },
        Transform::from_xyz(
            crate::lab::COUNTER_SPOT.x - 0.4,
            crate::lab::COUNTER_TOP,
            crate::lab::COUNTER_DROP_Z,
        ),
        Replicated,
    ));
}

/// Uses a held [`Deterrent`] on a hostile [`RogueOfficer`]. An independent
/// reader of `ApplyHeldRequested`, the same message the syringe's own F-key
/// dispatch reads — Bevy gives every system its own cursor, so this needs no
/// change to `body::handle_apply_held` at all. Never deals damage back to
/// the officer, which is what keeps this defensive.
fn handle_deterrent_use(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<ApplyHeldRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy)>,
    mut deterrents: Query<&mut Deterrent>,
    mut officers: Query<&mut CrewRoute, With<RogueOfficer>>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some(target) = request.target else {
            continue;
        };
        let Ok(mut route) = officers.get_mut(target) else {
            continue;
        };
        let Some((deterrent_entity, _)) = held.iter().find(|(_, holder)| holder.0 == player) else {
            continue;
        };
        let Ok(mut deterrent) = deterrents.get_mut(deterrent_entity) else {
            continue;
        };
        if deterrent.charges == 0 {
            continue;
        }
        deterrent.charges -= 1;
        commands.entity(target).remove::<RogueOfficer>().remove::<Interactable>();
        route.leave();
    }
}

/// Builds the deterrent's mesh wherever it appears — the same
/// spawn(authority)/dress(everywhere) split every pickable prop in the lab
/// uses.
fn dress_deterrent(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    added: Query<Entity, Added<Deterrent>>,
) {
    for entity in &added {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(0.10, 0.045, 0.20))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.14, 0.16, 0.20),
                perceptual_roughness: 0.35,
                metallic: 0.6,
                ..default()
            })),
            Interactable::new("Security-issue deterrent"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::OrderConfig;

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> RogueSecurityScript {
        ron::from_str(include_str!("../../assets/data/station.rogue_security.ron")).unwrap()
    }

    fn config() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap()
    }

    fn crew() -> Vec<CrewDef> {
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap()
    }

    fn schedule_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(StationData {
                crew: crew(),
                config: config(),
            })
            .insert_resource(Script(script()))
            .insert_resource(RogueSpawner {
                timer: Timer::from_seconds(0.01, TimerMode::Once),
            })
            .insert_resource(RogueRedeemed(false))
            .init_resource::<Shift>()
            .init_resource::<Time>()
            .init_resource::<RadioLog>()
            .add_systems(Update, schedule_rogue_encounter);
        app
    }

    fn tick(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn a_clean_player_never_sees_a_rogue_officer() {
        let mut app = schedule_app();
        // Default Shift standing is 0, well above any sane `hostile_below`.
        tick(&mut app, 0.1);

        let mut officers = app.world_mut().query::<&RogueOfficer>();
        assert_eq!(
            officers.iter(app.world()).count(),
            0,
            "standing has never soured, so nothing should have spawned"
        );
    }

    #[test]
    fn hostile_standing_arms_a_visit() {
        let mut app = schedule_app();
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(Department::Security, -20);

        tick(&mut app, 0.1);

        let mut officers = app.world_mut().query::<&RogueOfficer>();
        assert_eq!(officers.iter(app.world()).count(), 1);
    }

    #[test]
    fn standing_souring_again_after_redemption_brings_them_back() {
        // The reward used to be unreachable by construction: `check_redemption`
        // spawns the `Deterrent` at `redeemed_at` (+15) and set a flag that
        // stopped `schedule_rogue_encounter` forever, while the item only works
        // pointed at a live officer — which could then never exist again. Now
        // standing is the only gate in both directions, so the deterrent earned
        // on the way out is insurance against the next fall.
        let mut app = schedule_app();
        app.insert_resource(RogueRedeemed(true));
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(Department::Security, -20);

        tick(&mut app, 0.1);

        let mut officers = app.world_mut().query::<&RogueOfficer>();
        assert_eq!(
            officers.iter(app.world()).count(),
            1,
            "a redeemed career that sours again must be able to see an officer, \
             or the reward it earned can never be used"
        );
    }

    #[test]
    fn redemption_still_keeps_them_away_while_standing_holds_up() {
        // The other half of the same rule: redemption is not what stops the
        // visits, good standing is — so a redeemed career in good standing
        // stays quiet exactly as before.
        let mut app = schedule_app();
        app.insert_resource(RogueRedeemed(true));
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(Department::Security, 20);

        tick(&mut app, 0.1);

        let mut officers = app.world_mut().query::<&RogueOfficer>();
        assert_eq!(officers.iter(app.world()).count(), 0);
    }

    #[test]
    fn rogue_security_ron_parses_and_names_real_illicit_reagents() {
        let data = data();
        let script = script();
        assert!(!script.officer_name.trim().is_empty());
        assert!(!script.encounters.is_empty());
        assert!(
            script.encounters.iter().any(|e| e.physical),
            "at least one encounter should carry real physical risk, or the mechanic is dead data"
        );
        for encounter in &script.encounters {
            let reagent = data
                .reagents
                .id_of(&encounter.reagent)
                .unwrap_or_else(|| panic!("'{}' names no real reagent", encounter.reagent));
            assert!(
                data.reagents
                    .get(reagent)
                    .categories
                    .contains(&chem_sim::Category::Illicit),
                "'{}' is demanded by a rogue officer but is not Category::Illicit",
                encounter.reagent
            );
            assert!(!encounter.amounts.is_empty());
            assert!(!encounter.pretext.trim().is_empty());
            assert!(!encounter.refusal_line.trim().is_empty());
            assert!(!encounter.compliance_line.trim().is_empty());
        }
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(script()))
            .init_resource::<Shift>()
            .init_resource::<Time>()
            .init_resource::<RadioLog>()
            .add_message::<ToClients<HazardFelt>>()
            .add_systems(Update, expire_rogue_encounters);
        app
    }

    fn spawn_officer(app: &mut App, reagent: &str, patience: f32, physical: bool) -> Entity {
        let id = app.world().resource::<ChemDb>().reagent(reagent);
        let mut route = CrewRoute::arrival(0.0);
        route.phase = CrewPhase::Waiting;
        app.world_mut()
            .spawn((
                RogueOfficer {
                    reagent: id,
                    amount: Units::whole(10),
                    patience,
                    waited: 0.0,
                    physical,
                },
                route,
                Interactable::new("test"),
            ))
            .id()
    }

    #[test]
    fn refusing_costs_more_standing_than_complying() {
        const { assert!(ROGUE_REFUSE_PENALTY < ROGUE_COMPLY_PENALTY) };
    }

    #[test]
    fn a_refused_encounter_costs_security_standing_and_leaves() {
        let mut app = resolution_app();
        let officer = spawn_officer(&mut app, "space_drugs", 1.0, false);

        tick(&mut app, 1.5);

        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Security),
            ROGUE_REFUSE_PENALTY
        );
        assert!(
            app.world().get::<RogueOfficer>(officer).is_none(),
            "a resolved encounter should not linger"
        );
    }

    #[test]
    fn a_non_physical_refusal_never_deals_damage() {
        let mut app = resolution_app();
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(Department::Security, -20);
        let chemist = app
            .world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Server,
                },
                Body::default(),
            ))
            .id();
        spawn_officer(&mut app, "space_drugs", 1.0, false);

        tick(&mut app, 1.5);

        assert!(!app.world().get::<Body>(chemist).unwrap().0.is_hurt());
    }

    #[test]
    fn a_physical_encounter_never_fires_above_the_hostile_threshold() {
        // Standing has not actually soured, so even a `physical: true`
        // encounter cannot turn violent regardless of the dice.
        let mut app = resolution_app();
        let chemist = app
            .world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Server,
                },
                Body::default(),
            ))
            .id();
        spawn_officer(&mut app, "bath_salts", 1.0, true);

        tick(&mut app, 1.5);

        assert!(!app.world().get::<Body>(chemist).unwrap().0.is_hurt());
    }

    #[test]
    fn redemption_spawns_the_deterrent_exactly_once() {
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<RogueRedeemed>()
            .add_systems(Update, check_redemption);
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(Department::Security, 30);

        app.update();
        app.update();
        app.update();

        let mut deterrents = app.world_mut().query::<&Deterrent>();
        assert_eq!(
            deterrents.iter(app.world()).count(),
            1,
            "redemption must fire exactly once even across several frames"
        );
        assert!(app.world().resource::<RogueRedeemed>().0);
    }

    #[test]
    fn a_low_standing_grants_no_reward() {
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<RogueRedeemed>()
            .add_systems(Update, check_redemption);

        app.update();

        let mut deterrents = app.world_mut().query::<&Deterrent>();
        assert_eq!(deterrents.iter(app.world()).count(), 0);
    }
}
