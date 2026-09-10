//! What an NPC can perceive, and what it remembers afterwards.
//!
//! Before this module the station had four independent answers to "did anyone
//! notice?" — a non-spatial scan, a bare radius, a radius plus room, and a
//! radius plus occlusion — and `docs/npc-ai.md` records the intent to
//! generalise the strongest rather than add a fifth. So sight here is the
//! `speech::place_bubbles` pairing: distance, room identity, and the real
//! [`crate::interaction::authority_segment_blocked`] occlusion test against the
//! same `Solid` boxes movement already uses. There is no second geometry
//! system and no raycasting, which keeps it correct on a headless authority.
//!
//! The rule this module exists to enforce: an NPC acts on what it learned, not
//! on world truth. Scoring reads [`NpcMemory`], never a global query.

use bevy::prelude::*;

use super::{stable_text_key, UtilityAgent};
use crate::body::Bloodstream;
use crate::lab::{Solid, WalkableAreas};

/// Eye height above an actor's origin, in metres.
///
/// The three existing occlusion callers each picked their own (1.3, 0.65, and
/// the origin). Perception commits to one so a witness result cannot depend on
/// which system asked.
pub const EYE_HEIGHT: f32 = 1.3;

/// How far an NPC can recognise what someone is doing, in metres.
///
/// Matches the existing `social::OBSERVATION_RANGE`, which is the precedent
/// for recognising an *action* rather than merely hearing a noise.
pub const SIGHT_RANGE: f32 = 8.0;

/// How far a shout or a cry carries, in metres.
///
/// Matches `speech::EARSHOT`. Hearing deliberately ignores occlusion — a wall
/// muffles a shout, it does not silence it — but it is still bounded, so a
/// cry on the far side of the station reaches nobody.
pub const EARSHOT: f32 = 14.0;

/// How far a subject registers, once chemical concealment is accounted for.
///
/// Factored out of [`witness_stimuli`] because two different systems now ask
/// this question and they must not be able to disagree. A covert actor
/// estimating whether it is being watched, and a witness deciding whether it
/// saw something, are the same geometry read from opposite ends — if one of
/// them applied concealment and the other did not, an actor could be certain it
/// was unobserved by someone who was in fact looking straight at it, or hold
/// back from someone who could not possibly have seen.
///
/// `concealment` is the same `Bloodstream` aggregate the cult guards consult,
/// clamped at 0.95 so no chemistry makes a person entirely invisible.
/// `strength` floors at 0.2 so a faint event still registers at close range.
pub fn concealed_sight_range(concealment: f32, strength: f32) -> f32 {
    SIGHT_RANGE * (1.0 - concealment.clamp(0.0, 0.95)) * strength.max(0.2)
}

/// What kind of thing was perceived.
///
/// Deliberately coarse. A memory records *that* something happened and how
/// sure the witness is, not a private score or an exact intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StimulusKind {
    /// A body went down, or is visibly hurt.
    Casualty,
    /// A cry for help or a spoken report.
    CallForHelp,
    /// Fire, smoke, a spill, or a machine fault.
    Hazard,
    /// Handling that looked wrong. Deliberately ambiguous: innocent and covert
    /// food handling produce the same kind, and only later evidence separates
    /// them.
    SuspiciousHandling,
    /// A meal was served, eaten, or linked to symptoms.
    Food,
}

impl StimulusKind {
    /// How long a memory of this kind stays useful, in seconds.
    ///
    /// A body on the floor is remembered far longer than a glimpse of someone
    /// handling a tray.
    fn retention_seconds(self) -> f32 {
        match self {
            Self::Casualty => 300.0,
            Self::CallForHelp => 120.0,
            Self::Hazard => 240.0,
            Self::SuspiciousHandling => 420.0,
            Self::Food => 90.0,
        }
    }

    /// Whether this kind can be perceived without seeing it. A shout carries
    /// through a wall; a glimpse of someone palming a vial does not.
    fn audible(self) -> bool {
        matches!(self, Self::CallForHelp | Self::Casualty | Self::Hazard)
    }
}

/// Something perceivable that just happened.
///
/// Emitted by whichever system caused it and consumed the same frame by
/// [`witness_stimuli`]. It is not a queue: an unwitnessed stimulus is simply
/// unwitnessed, which is the entire point.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct Stimulus {
    pub kind: StimulusKind,
    /// Who or what the memory will be *about*.
    pub subject: Option<Entity>,
    /// Who caused it, when that differs from the subject.
    pub actor: Option<Entity>,
    pub at: Vec3,
    /// 0..1. Scales confidence and, for audible kinds, how far it carries.
    pub strength: f32,
}

impl Stimulus {
    pub fn new(kind: StimulusKind, at: Vec3) -> Self {
        Self {
            kind,
            subject: None,
            actor: None,
            at,
            strength: 1.0,
        }
    }

    pub fn about(mut self, subject: Entity) -> Self {
        self.subject = Some(subject);
        self
    }

    pub fn by(mut self, actor: Entity) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn with_strength(mut self, strength: f32) -> Self {
        self.strength = strength.clamp(0.0, 1.0);
        self
    }
}

/// How a witness came to know something. Preserved because a Security
/// interview cares about the difference between "I saw it" and "someone told
/// me", and because a report can be wrong in ways a sighting cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modality {
    Seen,
    Heard,
    /// Told by another crew member. The teller is kept on the fact.
    Told,
}

/// One thing an NPC learned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryFact {
    pub kind: StimulusKind,
    pub subject: Option<Entity>,
    pub actor: Option<Entity>,
    /// Where the witness believes it happened — where it was perceived from,
    /// not where the subject is now. A memory does not track a moving body.
    pub at: Vec3,
    /// Last-known room name, hashed for a cheap stable comparison. `None` when
    /// perceived in a doorway.
    pub room_key: Option<u64>,
    pub modality: Modality,
    /// Who reported it, for `Modality::Told`.
    pub source: Option<Entity>,
    /// 0..1 at the moment of learning. Decays with age.
    pub confidence: f32,
    pub learned_at: f32,
}

impl MemoryFact {
    /// Confidence now, after decay. Reaches zero at the kind's retention
    /// horizon, so an old memory fades rather than vanishing abruptly.
    pub fn confidence_at(&self, now: f32) -> f32 {
        let age = (now - self.learned_at).max(0.0);
        let horizon = self.kind.retention_seconds();
        if age >= horizon {
            return 0.0;
        }
        (self.confidence * (1.0 - age / horizon)).clamp(0.0, 1.0)
    }
}

/// Bounded per-NPC episodic memory. Authority-only: this is knowledge, and
/// replicating it would tell a client what a witness saw.
#[derive(Component, Clone, Debug, Default)]
pub struct NpcMemory {
    facts: Vec<MemoryFact>,
}

/// Keeps one witness's memory bounded regardless of how eventful a shift is.
const MAX_FACTS: usize = 24;

impl NpcMemory {
    /// Records a fact, merging with an existing memory of the same event.
    ///
    /// Re-seeing the same casualty refreshes the memory instead of stacking a
    /// duplicate — otherwise a body lying in view would fill the whole buffer
    /// and evict everything else.
    pub fn remember(&mut self, fact: MemoryFact) {
        if let Some(existing) = self.facts.iter_mut().find(|held| {
            held.kind == fact.kind && held.subject == fact.subject && held.actor == fact.actor
        }) {
            // Direct sight supersedes hearsay about the same event.
            if fact.confidence >= existing.confidence || existing.modality == Modality::Told {
                *existing = fact;
            } else {
                existing.learned_at = fact.learned_at;
            }
            return;
        }
        if self.facts.len() >= MAX_FACTS {
            // Drop the least certain rather than the oldest: a faint rumour is
            // worth less than a solid older sighting.
            if let Some((index, _)) = self
                .facts
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.confidence.total_cmp(&b.confidence))
            {
                self.facts.swap_remove(index);
            }
        }
        self.facts.push(fact);
    }

    /// Every fact still believed, most confident first.
    pub fn recall(&self, now: f32) -> impl Iterator<Item = &MemoryFact> {
        self.facts
            .iter()
            .filter(move |fact| fact.confidence_at(now) > 0.0)
    }

    /// The most confident current memory of one kind.
    pub fn best(&self, kind: StimulusKind, now: f32) -> Option<&MemoryFact> {
        self.recall(now)
            .filter(|fact| fact.kind == kind)
            .max_by(|a, b| a.confidence_at(now).total_cmp(&b.confidence_at(now)))
    }

    /// Whether this NPC believes anything about a given subject.
    pub fn knows_about(&self, subject: Entity, now: f32) -> bool {
        self.recall(now).any(|fact| fact.subject == Some(subject))
    }

    /// The most confident current memory of one kind *about a given subject*.
    ///
    /// Distinct from [`Self::best`], which ignores the subject: deciding
    /// whether someone needs telling about a specific casualty is not the same
    /// question as what the worst thing they know about is.
    pub fn best_about(&self, subject: Entity, kind: StimulusKind, now: f32) -> Option<&MemoryFact> {
        self.recall(now)
            .filter(|fact| fact.kind == kind && fact.subject == Some(subject))
            .max_by(|a, b| a.confidence_at(now).total_cmp(&b.confidence_at(now)))
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// Drops fully decayed facts. Called on a slow cadence; `recall` already
    /// hides them, so this only reclaims space.
    fn forget_expired(&mut self, now: f32) {
        self.facts.retain(|fact| fact.confidence_at(now) > 0.0);
    }
}

/// Whether an agent may act on a job ticket, given what it knows.
///
/// This is the seam that makes perception load-bearing rather than decorative.
/// A ticket naming a `subject` is a response to a *specific body*, and only a
/// worker who saw or heard the event may answer it — otherwise the whole crew
/// converges on a collapse nobody witnessed, which is the omniscience the
/// utility rewrite exists to remove.
///
/// Deliberately permissive in two directions:
///
/// - A ticket with no `subject` is station work at a known place (a fault
///   light, a delivery). It needs no witness and is never gated.
/// - An agent with **no** [`NpcMemory`] component at all is not blind, it is
///   un-modelled — a scripted or test agent. Gating those would silently
///   disable every department that has not opted in.
///
/// Only an agent that *has* a memory and does not hold the fact is refused.
pub fn may_respond_to(memory: Option<&NpcMemory>, subject: Option<Entity>, now: f32) -> bool {
    let (Some(subject), Some(memory)) = (subject, memory) else {
        return true;
    };
    memory.knows_about(subject, now)
}

/// Whether `observer` can see `point`, using the same three questions
/// `speech::place_bubbles` asks: near enough, same room, and no wall between.
///
/// A doorway (`room_at` returns `None`) is treated as visible from either
/// side rather than blind, because the alternative makes stepping into a
/// threshold a reliable way to vanish — the exact failure `docs/npc-ai.md`
/// warns about.
pub fn can_see(
    observer: Vec3,
    point: Vec3,
    areas: &WalkableAreas,
    solids: &[(Vec3, Vec3)],
    range: f32,
) -> bool {
    if !observer.is_finite() || !point.is_finite() {
        return false;
    }
    if observer.distance_squared(point) > range * range {
        return false;
    }
    // A threshold belongs to neither room, so it is visible from both.
    let observer_room = areas.room_at(observer);
    let point_room = areas.room_at(point);
    if let (Some(here), Some(there)) = (observer_room, point_room) {
        if here != there {
            return false;
        }
    }
    let eye = observer + Vec3::Y * EYE_HEIGHT;
    let target = point + Vec3::Y * EYE_HEIGHT * 0.5;
    !solids.iter().any(|(center, half_extents)| {
        crate::interaction::authority_segment_blocked(eye, target, *center, *half_extents)
    })
}

/// Turns this frame's stimuli into memories for whoever could perceive them.
///
/// Sight and hearing are checked separately: an audible event heard through a
/// wall is remembered with lower confidence than one seen directly, which is
/// what lets a Security interview distinguish a witness from someone who only
/// heard a bang.
#[allow(clippy::type_complexity)]
pub(super) fn witness_stimuli(
    time: Res<Time>,
    mut stimuli: MessageReader<Stimulus>,
    areas: Option<Res<WalkableAreas>>,
    solids: Query<(&Transform, &Solid)>,
    concealed: Query<&Bloodstream>,
    mut witnesses: Query<
        (Entity, &Transform, &mut NpcMemory, Option<&Bloodstream>),
        With<UtilityAgent>,
    >,
) {
    let events: Vec<Stimulus> = stimuli.read().copied().collect();
    if events.is_empty() {
        return;
    }
    let Some(areas) = areas else {
        return;
    };
    let boxes: Vec<(Vec3, Vec3)> = solids
        .iter()
        .map(|(transform, solid)| (transform.translation, solid.half_extents))
        .collect();
    let now = time.elapsed_secs();

    // Chemical concealment shortens how far a subject registers, reusing the
    // same aggregate the cult guards already consult rather than inventing a
    // second stealth rule. Resolved once per event, not per witness.
    let concealment: Vec<f32> = events
        .iter()
        .map(|event| {
            event
                .subject
                .or(event.actor)
                .and_then(|who| concealed.get(who).ok())
                .map_or(0.0, |blood| blood.0.concealment().clamp(0.0, 0.95))
        })
        .collect();

    for (witness, transform, mut memory, blood) in &mut witnesses {
        // An unconscious witness perceives nothing at all.
        if blood.is_some_and(|blood| blood.0.incapacitated()) {
            continue;
        }
        let here = transform.translation;
        for (event, hidden) in events.iter().zip(&concealment) {
            // An actor does not learn their own deed by witnessing it; they
            // already know their own state and intent.
            if event.actor == Some(witness) {
                continue;
            }
            let sight_range = concealed_sight_range(*hidden, event.strength);

            let (modality, confidence) = if can_see(here, event.at, &areas, &boxes, sight_range) {
                (Modality::Seen, event.strength)
            } else if event.kind.audible()
                && here.distance_squared(event.at)
                    <= (EARSHOT * event.strength) * (EARSHOT * event.strength)
            {
                // Heard, not seen: enough to know something happened and
                // roughly where, not enough to identify who did it.
                (Modality::Heard, event.strength * 0.5)
            } else {
                continue;
            };

            memory.remember(MemoryFact {
                kind: event.kind,
                subject: event.subject,
                // A witness who only heard it cannot name the actor.
                actor: if modality == Modality::Seen {
                    event.actor
                } else {
                    None
                },
                at: event.at,
                room_key: areas.room_at(event.at).map(stable_text_key),
                modality,
                source: None,
                confidence,
                learned_at: now,
            });
        }
    }
}

/// Reclaims space from fully decayed memories on a slow cadence.
fn prune_memories(time: Res<Time>, mut memories: Query<&mut NpcMemory>) {
    let now = time.elapsed_secs();
    for mut memory in &mut memories {
        if memory.facts.len() > MAX_FACTS / 2 {
            memory.forget_expired(now);
        }
    }
}

pub(super) fn register(app: &mut App) {
    app.add_message::<Stimulus>().add_systems(
        Update,
        (witness_stimuli, prune_memories)
            .chain()
            .in_set(super::UtilityAiSet::Observe)
            .run_if(crate::net::is_authority),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wall between two points, sized so it spans the gap.
    fn wall_at(x: f32) -> (Vec3, Vec3) {
        (Vec3::new(x, 1.0, 0.0), Vec3::new(0.2, 2.0, 8.0))
    }

    fn open_areas() -> WalkableAreas {
        WalkableAreas::from_floor_plan()
    }

    #[test]
    fn sight_needs_range_and_an_unblocked_line() {
        let areas = open_areas();
        let observer = Vec3::new(-2.0, 0.0, 0.0);
        let near = Vec3::new(0.0, 0.0, 0.0);

        assert!(
            can_see(observer, near, &areas, &[], SIGHT_RANGE),
            "an open, nearby point must be visible"
        );
        assert!(
            !can_see(observer, near, &areas, &[wall_at(-1.0)], SIGHT_RANGE),
            "a wall between the two must block sight"
        );
        assert!(
            !can_see(
                observer,
                observer + Vec3::new(SIGHT_RANGE + 1.0, 0.0, 0.0),
                &areas,
                &[],
                SIGHT_RANGE
            ),
            "a point beyond the range must not be visible"
        );
    }

    /// The doorway rule `docs/npc-ai.md` calls out: a threshold belongs to
    /// neither room, so treating it as blind would make stepping into a
    /// doorway a reliable way to vanish.
    #[test]
    fn a_doorway_is_visible_from_either_side_rather_than_blind() {
        // Two named rooms with a gap between them. The gap belongs to neither,
        // exactly as a real threshold does.
        let mut areas = WalkableAreas::default();
        let room = |min_x: f32, max_x: f32| crate::lab::Bounds {
            min_x,
            max_x,
            min_z: -2.0,
            max_z: 2.0,
        };
        areas.push(room(-6.0, -1.0), Some("West".into()));
        areas.push(room(1.0, 6.0), Some("East".into()));

        let west = Vec3::new(-2.0, 0.0, 0.0);
        let east = Vec3::new(2.0, 0.0, 0.0);
        let doorway = Vec3::ZERO;

        assert!(
            areas.room_at(doorway).is_none(),
            "this test only means anything if the chosen point really is a threshold"
        );
        assert!(
            can_see(west, doorway, &areas, &[], SIGHT_RANGE),
            "a doorway must be visible from the room on one side"
        );
        assert!(
            can_see(east, doorway, &areas, &[], SIGHT_RANGE),
            "a doorway must be visible from the room on the other side too"
        );
        // Someone standing in the threshold can still be seen, so stepping
        // into a doorway is not a way to vanish.
        assert!(
            can_see(doorway, west, &areas, &[], SIGHT_RANGE),
            "an actor in a doorway must still perceive the rooms it joins"
        );
        // But two different rooms remain separated.
        assert!(
            !can_see(west, east, &areas, &[], SIGHT_RANGE),
            "sight must not pass between two distinct rooms"
        );
    }

    /// A stimulus written *after* `Observe` has already run is still witnessed
    /// on the following frame.
    ///
    /// This is not a hypothetical ordering: Service emits `Food` from the
    /// `Resolve` set, which runs later in the frame than `Observe`, so the
    /// write always misses the current frame's read. The event is delivered on
    /// the next frame's read instead of being dropped, and one frame of latency
    /// is correct anyway — nobody witnesses an event before it finishes.
    ///
    /// Measured rather than assumed: the message is consumed exactly once, on
    /// the first read that follows the write, and is not re-read on any later
    /// frame. That once-only property comes from the reader's cursor, not from
    /// buffer expiry — an idle frame between the write and the read does not
    /// discard it. Pinned here because the failure mode is silent: the emitter
    /// looks right and the witness simply never learns anything.
    #[test]
    fn a_stimulus_emitted_after_the_observe_set_is_witnessed_next_frame() {
        let mut app = App::new();
        let mut areas = WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: -20.0,
                max_x: 20.0,
                min_z: -20.0,
                max_z: 20.0,
            },
            Some("Hall".into()),
        );
        // Counts stimuli actually consumed, per frame, so the test can assert
        // *when* delivery happened rather than merely that it eventually did.
        #[derive(Resource, Default)]
        struct WitnessReads(usize);

        fn count_reads(mut stimuli: MessageReader<Stimulus>, mut reads: ResMut<WitnessReads>) {
            reads.0 += stimuli.read().count();
        }

        app.init_resource::<Time>()
            .init_resource::<WitnessReads>()
            .insert_resource(areas)
            .add_message::<Stimulus>()
            .add_systems(Update, (witness_stimuli, count_reads).chain());

        let diner = app
            .world_mut()
            .spawn((
                Transform::from_xyz(1.0, 0.0, 0.0),
                UtilityAgent::new(1, 0),
                NpcMemory::default(),
            ))
            .id();
        let meal = app.world_mut().spawn(Transform::default()).id();

        // Frame one: the witness runs first and there is nothing to see. The
        // meal is served afterwards, exactly as `Resolve` follows `Observe`.
        app.update();
        assert!(
            !app.world()
                .get::<NpcMemory>(diner)
                .unwrap()
                .knows_about(meal, 0.0),
            "nothing has happened yet"
        );
        app.world_mut()
            .write_message(Stimulus::new(StimulusKind::Food, Vec3::ZERO).about(meal));

        // Frame two: the witness reads the message written after it last ran.
        //
        // Counting reads rather than checking `knows_about` is deliberate. A
        // formed memory persists across later frames, so a presence check
        // cannot tell "witnessed on frame two" from "witnessed on frame five",
        // and an early version of this test passed even when extra frame swaps
        // were inserted — it proved nothing. The read counter moves on exactly
        // the frame the message is consumed.
        app.update();
        let after_delivery = app.world().resource::<WitnessReads>().0;
        assert_eq!(
            after_delivery, 1,
            "a stimulus written after Observe must survive the frame swap, not be dropped"
        );
        assert!(
            app.world()
                .get::<NpcMemory>(diner)
                .unwrap()
                .knows_about(meal, 0.0),
            "and it must actually reach the witness's memory"
        );

        // Further frames consume nothing. Without this, a stimulus left
        // readable would be re-witnessed every frame, and a single served meal
        // would keep refreshing every bystander's memory of it for as long as
        // the buffer held — which is exactly how a one-off event turns into a
        // permanent one. Several frames, not one, because a message that
        // lingered for a bounded few would slip past a single extra check.
        for _ in 0..4 {
            app.update();
            assert_eq!(
                app.world().resource::<WitnessReads>().0,
                after_delivery,
                "the message must be consumed once, not re-read on later frames"
            );
        }
    }

    /// The whole point of the module, end to end: a nearby witness learns
    /// about a casualty, and one behind a wall does not. `SuspiciousHandling`
    /// is used because it is inaudible, so only sight can carry it.
    #[test]
    fn only_a_witness_who_could_perceive_it_remembers_it() {
        let mut app = App::new();
        let mut areas = WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: -20.0,
                max_x: 20.0,
                min_z: -20.0,
                max_z: 20.0,
            },
            Some("Hall".into()),
        );
        app.init_resource::<Time>()
            .insert_resource(areas)
            .add_message::<Stimulus>()
            .add_systems(Update, witness_stimuli);

        // A wall standing between the far witness and the event.
        app.world_mut().spawn((
            Transform::from_xyz(3.0, 1.0, 0.0),
            Solid {
                half_extents: Vec3::new(0.2, 2.0, 8.0),
            },
        ));

        let spawn_witness = |app: &mut App, at: Vec3| {
            app.world_mut()
                .spawn((
                    Transform::from_translation(at),
                    UtilityAgent::new(1, 0),
                    NpcMemory::default(),
                ))
                .id()
        };
        let close = spawn_witness(&mut app, Vec3::new(1.0, 0.0, 0.0));
        let behind_wall = spawn_witness(&mut app, Vec3::new(6.0, 0.0, 0.0));
        let far_away = spawn_witness(&mut app, Vec3::new(-30.0, 0.0, 0.0));
        let victim = app.world_mut().spawn(Transform::default()).id();

        app.world_mut().write_message(
            Stimulus::new(StimulusKind::SuspiciousHandling, Vec3::ZERO).about(victim),
        );
        app.update();

        let remembers = |app: &App, who: Entity| {
            app.world()
                .get::<NpcMemory>(who)
                .unwrap()
                .knows_about(victim, 0.0)
        };
        assert!(
            remembers(&app, close),
            "a witness in the open must learn it"
        );
        assert!(
            !remembers(&app, behind_wall),
            "a wall must stop an inaudible event from being witnessed"
        );
        assert!(
            !remembers(&app, far_away),
            "an event out of range must not be witnessed"
        );
    }

    /// An audible event carries through a wall, but the witness only knows
    /// that something happened — not who did it.
    #[test]
    fn hearing_something_is_weaker_knowledge_than_seeing_it() {
        let mut app = App::new();
        let mut areas = WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: -20.0,
                max_x: 20.0,
                min_z: -20.0,
                max_z: 20.0,
            },
            Some("Hall".into()),
        );
        app.init_resource::<Time>()
            .insert_resource(areas)
            .add_message::<Stimulus>()
            .add_systems(Update, witness_stimuli);
        app.world_mut().spawn((
            Transform::from_xyz(3.0, 1.0, 0.0),
            Solid {
                half_extents: Vec3::new(0.2, 2.0, 8.0),
            },
        ));

        let seer = app
            .world_mut()
            .spawn((
                Transform::from_xyz(1.0, 0.0, 0.0),
                UtilityAgent::new(1, 0),
                NpcMemory::default(),
            ))
            .id();
        let hearer = app
            .world_mut()
            .spawn((
                Transform::from_xyz(6.0, 0.0, 0.0),
                UtilityAgent::new(2, 0),
                NpcMemory::default(),
            ))
            .id();
        let culprit = app.world_mut().spawn(Transform::default()).id();

        app.world_mut()
            .write_message(Stimulus::new(StimulusKind::CallForHelp, Vec3::ZERO).by(culprit));
        app.update();

        let seen = *app
            .world()
            .get::<NpcMemory>(seer)
            .unwrap()
            .best(StimulusKind::CallForHelp, 0.0)
            .expect("the near witness saw it");
        let heard = *app
            .world()
            .get::<NpcMemory>(hearer)
            .unwrap()
            .best(StimulusKind::CallForHelp, 0.0)
            .expect("a shout carries through a wall");

        assert_eq!(seen.modality, Modality::Seen);
        assert_eq!(heard.modality, Modality::Heard);
        assert!(
            heard.confidence < seen.confidence,
            "hearsay must be less certain than a sighting"
        );
        assert_eq!(
            seen.actor,
            Some(culprit),
            "someone who saw it can name who did it"
        );
        assert_eq!(
            heard.actor, None,
            "someone who only heard it must not be able to name the actor"
        );
    }

    /// The second real stimulus source, alongside Medical casualties.
    ///
    /// `Hazard` was modelled from the start but never emitted, so a whole
    /// stimulus kind sat unexercised. An Engineering fault is loud and visible
    /// where it happens, so this asserts the same property the casualty test
    /// does: nearby crew learn it, distant crew do not.
    #[test]
    fn an_engineering_fault_is_a_hazard_only_the_nearby_crew_witness() {
        let mut app = App::new();
        let mut areas = WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: -40.0,
                max_x: 40.0,
                min_z: -40.0,
                max_z: 40.0,
            },
            Some("Hall".into()),
        );
        app.init_resource::<Time>()
            .insert_resource(areas)
            .add_message::<Stimulus>()
            .add_systems(Update, witness_stimuli);

        let nearby = app
            .world_mut()
            .spawn((
                crate::utility_ai::UtilityControlBundle::new(UtilityAgent::new(90, 0)),
                Transform::from_xyz(2.0, 0.0, 0.0),
            ))
            .id();
        let across_the_station = app
            .world_mut()
            .spawn((
                crate::utility_ai::UtilityControlBundle::new(UtilityAgent::new(91, 0)),
                Transform::from_xyz(35.0, 0.0, 0.0),
            ))
            .id();

        // Exactly what `apply_engineering_workplace_risk` writes on commit.
        app.world_mut()
            .write_message(Stimulus::new(StimulusKind::Hazard, Vec3::ZERO).with_strength(0.6));
        app.update();

        assert!(
            app.world()
                .get::<NpcMemory>(nearby)
                .unwrap()
                .best(StimulusKind::Hazard, 0.0)
                .is_some(),
            "a fault must be remembered by whoever was standing near it"
        );
        assert!(
            app.world()
                .get::<NpcMemory>(across_the_station)
                .unwrap()
                .best(StimulusKind::Hazard, 0.0)
                .is_none(),
            "a fault must not be known station-wide by fiat"
        );
    }

    /// Perception must not be inert infrastructure: a real Medical incident
    /// has to reach a nearby crew member's memory, and only a nearby one.
    #[test]
    fn a_real_casualty_becomes_a_memory_for_the_crew_who_could_see_it() {
        use crate::utility_ai::{
            IncidentKind, IncidentLedger, MedicalCaseLedger, Normalized, ReservationBook,
            UtilityControlBundle,
        };

        let mut app = App::new();
        let mut areas = WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: -40.0,
                max_x: 40.0,
                min_z: -40.0,
                max_z: 40.0,
            },
            Some("Hall".into()),
        );
        app.init_resource::<Time>()
            .init_resource::<IncidentLedger>()
            .init_resource::<MedicalCaseLedger>()
            .init_resource::<ReservationBook>()
            .insert_resource(areas)
            .add_message::<Stimulus>()
            .add_systems(
                Update,
                (
                    crate::utility_ai::medical::open_cases_from_incidents,
                    ApplyDeferred,
                    witness_stimuli,
                )
                    .chain(),
            );

        let patient = app
            .world_mut()
            .spawn((
                UtilityControlBundle::new(UtilityAgent::new(70, 0)),
                Transform::from_xyz(0.0, 0.0, 0.0),
                crate::body::Body::default(),
            ))
            .id();
        let bystander = app
            .world_mut()
            .spawn((
                // The bundle already carries an `NpcMemory`; spawning a second
                // one here would silently overwrite it.
                UtilityControlBundle::new(UtilityAgent::new(71, 0)),
                Transform::from_xyz(2.0, 0.0, 0.0),
            ))
            .id();
        let across_the_station = app
            .world_mut()
            .spawn((
                UtilityControlBundle::new(UtilityAgent::new(72, 0)),
                Transform::from_xyz(35.0, 0.0, 0.0),
            ))
            .id();

        app.world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Burn,
                crate::utility_ai::JobDomain::Cargo,
                patient,
                None,
                Vec3::ZERO,
                Normalized::new(0.6).unwrap(),
                0.0,
            )
            .expect("the ledger has room for one incident");
        app.update();

        assert!(
            app.world()
                .get::<NpcMemory>(bystander)
                .unwrap()
                .knows_about(patient, 0.0),
            "a crew member standing beside a collapse must remember it"
        );
        assert!(
            !app.world()
                .get::<NpcMemory>(across_the_station)
                .unwrap()
                .knows_about(patient, 0.0),
            "a collapse must not be known station-wide by fiat"
        );
    }

    #[test]
    fn confidence_decays_to_nothing_over_the_retention_window() {
        let fact = MemoryFact {
            kind: StimulusKind::Casualty,
            subject: None,
            actor: None,
            at: Vec3::ZERO,
            room_key: None,
            modality: Modality::Seen,
            source: None,
            confidence: 1.0,
            learned_at: 0.0,
        };
        let horizon = StimulusKind::Casualty.retention_seconds();

        assert!((fact.confidence_at(0.0) - 1.0).abs() < 1e-3);
        assert!(fact.confidence_at(horizon * 0.5) < 1.0);
        assert!(fact.confidence_at(horizon * 0.5) > 0.0);
        assert_eq!(fact.confidence_at(horizon), 0.0);
        assert_eq!(fact.confidence_at(horizon * 2.0), 0.0);
        // A memory never resurrects.
        assert_eq!(fact.confidence_at(f32::MAX), 0.0);
    }

    #[test]
    fn memory_is_bounded_and_merges_repeat_sightings() {
        let mut memory = NpcMemory::default();
        let subject = Entity::from_bits(7);
        let fact = |confidence: f32, at: f32| MemoryFact {
            kind: StimulusKind::Casualty,
            subject: Some(subject),
            actor: None,
            at: Vec3::ZERO,
            room_key: None,
            modality: Modality::Seen,
            source: None,
            confidence,
            learned_at: at,
        };

        // Seeing the same casualty repeatedly must refresh one memory, not
        // fill the buffer with duplicates.
        for tick in 0..50 {
            memory.remember(fact(1.0, tick as f32));
        }
        assert_eq!(memory.len(), 1);

        // Distinct subjects accumulate, but stay bounded.
        for id in 100..200u64 {
            memory.remember(MemoryFact {
                subject: Some(Entity::from_bits(id)),
                ..fact(0.5, 0.0)
            });
        }
        assert!(
            memory.len() <= MAX_FACTS,
            "memory grew past its bound: {}",
            memory.len()
        );
    }

    /// Direct sight must supersede hearsay about the same event, so a witness
    /// who later sees it for themselves stops relying on a rumour.
    #[test]
    fn seeing_something_supersedes_having_been_told_about_it() {
        let mut memory = NpcMemory::default();
        let subject = Entity::from_bits(11);
        let teller = Entity::from_bits(12);

        memory.remember(MemoryFact {
            kind: StimulusKind::Casualty,
            subject: Some(subject),
            actor: None,
            at: Vec3::ZERO,
            room_key: None,
            modality: Modality::Told,
            source: Some(teller),
            confidence: 0.4,
            learned_at: 0.0,
        });
        memory.remember(MemoryFact {
            kind: StimulusKind::Casualty,
            subject: Some(subject),
            actor: None,
            at: Vec3::new(1.0, 0.0, 0.0),
            room_key: None,
            modality: Modality::Seen,
            source: None,
            confidence: 0.9,
            learned_at: 5.0,
        });

        assert_eq!(memory.len(), 1, "the same event must not be held twice");
        let held = memory.best(StimulusKind::Casualty, 5.0).unwrap();
        assert_eq!(held.modality, Modality::Seen);
        assert_eq!(held.source, None);
    }

    /// The gate's three cases, stated directly. An un-modelled agent is not
    /// blind — gating it would silently switch off every department that has
    /// not opted into perception yet.
    #[test]
    fn only_a_modelled_agent_that_lacks_the_fact_is_refused() {
        let subject = Entity::from_raw_u32(9).unwrap();
        let mut knows = NpcMemory::default();
        knows.remember(MemoryFact {
            kind: StimulusKind::Casualty,
            subject: Some(subject),
            actor: None,
            at: Vec3::ZERO,
            room_key: None,
            modality: Modality::Seen,
            source: None,
            confidence: 1.0,
            learned_at: 0.0,
        });
        let blank = NpcMemory::default();

        assert!(
            may_respond_to(Some(&knows), Some(subject), 0.0),
            "a witness must be able to respond"
        );
        assert!(
            !may_respond_to(Some(&blank), Some(subject), 0.0),
            "a modelled agent that saw nothing must not respond"
        );
        assert!(
            may_respond_to(Some(&blank), None, 0.0),
            "station work naming no subject is never gated"
        );
        assert!(
            may_respond_to(None, Some(subject), 0.0),
            "an agent with no memory component is un-modelled, not blind"
        );
        assert!(
            !may_respond_to(
                Some(&knows),
                Some(subject),
                StimulusKind::Casualty.retention_seconds() + 1.0
            ),
            "a fully decayed memory must stop authorising a response"
        );
    }
}
