//! What the crew say out loud, in the room they are standing in.
//!
//! Until this, everything an NPC "said" reached the player through
//! [`crate::radio`] — a station-wide feed whose delay is deliberate and whose
//! whole design is that it arrives *later*, while you are elbow-deep in
//! something else — or as an [`Order::plea`](crate::orders::Order::plea) string
//! in the corner of the HUD. Nothing was ever said where it happened.
//!
//! That gap is worst exactly where the game most recently invested. `saboteur`
//! was rebuilt so an ignored tech physically *walks* to your beaker, and its
//! module doc is explicit about why: "you can watch them coming, and you can
//! stop them", where the old version's radio-line-only tell "made the tell a
//! notification rather than a thing that happened in the room". But nothing
//! gave the player a reason to look up, so the walk was real and invisible.
//!
//! # The shape
//!
//! One primitive — [`Speech`], a line a body is currently saying — and a
//! handful of narrow systems that decide when to insert one. Presentation is a
//! screen-space bubble anchored over the speaker's head, within [`EARSHOT`]
//! and only where a wall is not in the way — so a line is a thing that
//! happened *where you are*, rather than another card in the corner.
//!
//! This is the counterpart to the radio, not a replacement: the immediate
//! reaction belongs to the person in front of you, the delayed echo belongs to
//! the station.
//!
//! # Two things that would break co-op, written down
//!
//! **[`Speech`] is inserted once and removed once — never ticked.** The
//! countdown lives in [`SpeechTimer`], which is server-only and unreplicated.
//! A remaining-seconds field on the replicated component would re-send the
//! whole thing, string included, every frame of every line. That is precisely
//! the trap `Order.waited` and `AgitationRun` were both caught in, and fixing
//! it afterwards cost a pass of `bypass_change_detection` plumbing.
//!
//! **[`DWELL_SECONDS`] is a constant here, not authored in the script.** The
//! script is loaded through [`crate::threat::ScriptPlugin`], which promotes
//! authority-side only, so a client never sees it. Anything both ends must
//! agree on — and how long a line stays up is exactly that — has to be
//! derivable without it. Authored values in `station.speech.ron` are all
//! decisions only the authority makes.

use std::collections::HashMap;

use bevy::prelude::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use bevy_replicon::prelude::FromClient;

use crate::audio::{PlaySfx, Sfx};
use crate::crew::{Ambient, CrewMember, Errand};
use crate::interaction::{authority_segment_blocked, InteractRequested};
use crate::lab::{Solid, WalkableAreas};
use crate::net::is_authority;
use crate::orders::{Order, OrderResolved, Outcome};
use crate::player::{Chemist, PlayerCamera};
use crate::showdown::Pursuit;
use crate::threat;
use crate::AppState;

/// How long a line stays up, clamping the length-scaled figure below.
///
/// Constants rather than authored values, and deliberately so — see the module
/// doc. Both ends compute the same number from the same text.
///
/// The ceiling is a guard against a pathologically long authored line, not
/// something ordinary content reaches: at [`SECONDS_PER_CHAR`] a line would
/// have to run past seventy characters to hit it, and
/// `a_line_stays_up_long_enough_to_read_and_not_longer` pins both ends.
const DWELL_SECONDS: (f32, f32) = (2.6, 6.5);
/// A flat grant before length is counted at all.
///
/// Without it a two-word greeting is gone before a player looking the other
/// way has turned around — and "Don't mind me." is the single most common
/// thing anyone says. Noticing a line and reading it are two costs, and only
/// the second scales with how long it is.
const SPEECH_BASE_SECONDS: f32 = 2.0;
/// Reading pace on top of that base. A three-word greeting should not hold the
/// screen as long as a full sentence does.
const SECONDS_PER_CHAR: f32 = 0.06;

/// Roughly how far above a body's origin its head sits.
///
/// Crew stand with their origin at [`crate::crew::BODY_OFFSET`] (0.93) and the
/// chemist's eyes are at [`crate::player::EYE_HEIGHT`] (1.7), so this puts the
/// anchor a little over the head of someone the same height as the player.
const HEAD_HEIGHT: f32 = 0.78;

/// How far a line carries, in metres. The one distance in this module: it
/// decides both whether someone bothers speaking and whether their words are
/// drawn.
///
/// **Not a room test, and that is the important part.** The obvious rule —
/// only speak to, and only draw for, someone in the same room — gets the
/// single most common case in the game wrong: the chemist works in the Mixing
/// Hall, and the counter every customer walks up to is in the Lobby next door,
/// plainly visible through the doorway between them. A same-room rule would
/// have left that arrival silent and, worse, would have hidden the words of
/// someone the player can see standing right there.
///
/// Line of sight is a question `interaction::authority_segment_blocked`
/// already answers properly against the lab's own walls, so the bubble asks
/// *that* rather than approximating it with room identity. Rooms are still
/// what makes an arrival an arrival — see [`RoomMemory`] — but they have
/// nothing to say about what you can see.
///
/// Sized against the lab, not the station: the Mixing Hall is 15 m across and
/// the Lobby hangs off it, so this covers the working suite and stops well
/// short of the departments crew wander between all shift.
const EARSHOT: f32 = 14.0;

/// How many bubbles may be on screen at once, nearest first.
///
/// A queue at the counter is three or four people deep, and every one of them
/// greeting you at once would wall the view with text at exactly the moment
/// the player most needs to see the counter.
const MAX_BUBBLES: usize = 3;

/// Fade in and out, in seconds. Matches the ramp `ui::animate_radio_dispatch`
/// uses so the two channels feel like one game.
const FADE_IN: f32 = 0.15;
const FADE_OUT: f32 = 0.45;

pub struct SpeechPlugin;

impl Plugin for SpeechPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<SpeechScript>::new(
            "data/station.speech.ron",
            "speech.ron",
        ))
        .init_resource::<RoomMemory>()
        .init_resource::<Chatter>()
        .init_resource::<PendingRemarks>()
        .add_systems(OnEnter(AppState::Playing), reset_chatter)
        .add_systems(
            Update,
            (
                // Deciding what gets said is simulation, so it happens once,
                // on the authority, and reaches clients as a replicated
                // component — the same split `CrewRoute` has always had.
                // `remember_rooms` is part of that: rooms are server scratch
                // here, and no client reads them. See `RoomMemory`.
                (
                    remember_rooms,
                    notice_arrivals,
                    notice_errands,
                    notice_resolutions,
                    // Answering a question outranks every unprompted bark:
                    // last writer wins on one body, and a greeting landing on
                    // top of the answer you just asked for is the one ordering
                    // the player would actually notice.
                    start_exchanges,
                    deliver_remarks,
                    handle_talk,
                    // Last, so a line said this frame gets its full dwell
                    // rather than being aged by the tick that preceded it.
                    expire_speech,
                )
                    .chain()
                    .after(threat::PromoteScripts)
                    .run_if(is_authority),
                // Presentation, on both ends, built from `Added<Speech>` —
                // the co-op rule. A bubble drawn only on the host is the
                // exact silent failure every co-op bug in this project has
                // had. Nothing in this group touches authority-only state,
                // which is what makes that true by construction rather than
                // by inspection.
                (
                    announce_speech,
                    spawn_bubbles,
                    place_bubbles,
                    despawn_bubbles,
                )
                    .chain(),
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

// ---------------------------------------------------------------------------
// The primitive
// ---------------------------------------------------------------------------

/// One line, said out loud, by a body in a room.
///
/// Replicated, and carrying only what a peer has to *draw*: the words and how
/// to colour them. How long it lasts is derived identically on both ends by
/// [`speech_seconds`]; when it ends is [`SpeechTimer`]'s business, and only
/// the authority's.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speech {
    pub text: String,
    pub tone: SpeechTone,
}

/// How a line is coloured.
///
/// Mirrors [`crate::radio::RadioTone`] and adds the one a room needs that a
/// feed does not: `Wary`, for someone who is not pleased with you but has not
/// said anything worth calling negative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeechTone {
    #[default]
    Neutral,
    Friendly,
    Wary,
    Urgent,
}

impl SpeechTone {
    fn color(self) -> Color {
        match self {
            SpeechTone::Neutral => Color::srgb(0.88, 0.90, 0.94),
            SpeechTone::Friendly => Color::srgb(0.70, 0.93, 0.72),
            SpeechTone::Wary => Color::srgb(0.96, 0.84, 0.62),
            SpeechTone::Urgent => Color::srgb(0.96, 0.72, 0.66),
        }
    }
}

/// Server-side countdown on a line. Not replicated: see the module doc.
#[derive(Component)]
struct SpeechTimer(Timer);

/// How long until this body will say anything unprompted again.
///
/// Server-side, and kept on the body rather than in a table keyed by name
/// because it is per-*visit* state — a crew member who leaves and is later
/// respawned for another order is starting a new visit and should be free to
/// greet you again.
#[derive(Component)]
struct SpeechCooldown(f32);

/// How long a line stays up, from its own length.
///
/// Shared by the authority (which removes [`Speech`] when it expires) and
/// every client (which fades the bubble out over the same window), so the two
/// agree without a second value crossing the wire.
pub fn speech_seconds(text: &str) -> f32 {
    (SPEECH_BASE_SECONDS + text.chars().count() as f32 * SECONDS_PER_CHAR)
        .clamp(DWELL_SECONDS.0, DWELL_SECONDS.1)
}

/// Says one line, replacing whatever this body was saying before.
///
/// The one write path, so nothing anywhere else has to remember to attach the
/// timer — the same reason [`crate::crew::send_on_errand`] owns its
/// `CrewRoute` removal and [`crate::radio::PendingBroadcasts::push_delayed`]
/// owns its timer.
///
/// Newest wins on a body that is already talking, which is the rule
/// `ui::show_toasts` already applies to two interrupting cards: someone whose
/// situation changed mid-sentence should say the new thing, not finish the
/// stale one.
pub fn say(commands: &mut Commands, speaker: Entity, text: impl Into<String>, tone: SpeechTone) {
    let text = text.into();
    let seconds = speech_seconds(&text);
    commands.entity(speaker).insert((
        Speech { text, tone },
        SpeechTimer(Timer::from_seconds(seconds, TimerMode::Once)),
    ));
}

/// Takes a finished line off the body. The removal is what replicates the end
/// of it, which is why nothing here has to tell clients anything.
fn expire_speech(
    mut commands: Commands,
    time: Res<Time>,
    mut talking: Query<(Entity, &mut SpeechTimer)>,
) {
    for (entity, mut timer) in &mut talking {
        if timer.0.tick(time.delta()).just_finished() {
            commands
                .entity(entity)
                .remove::<Speech>()
                .remove::<SpeechTimer>();
        }
    }
}

// ---------------------------------------------------------------------------
// Rooms
// ---------------------------------------------------------------------------

/// Which room each crew member was last *definitely* standing in.
///
/// Rooms answer exactly one question in this module — has this body just
/// *arrived* somewhere — and nothing about what the player can see, which is
/// line of sight's job (see [`EARSHOT`]). So this is server scratch, in the
/// same category as `CrewRoute`: written and read only by the authority,
/// never replicated, and no client ever needs it.
///
/// That is worth stating rather than leaving implicit, because the natural
/// alternative is a trap. Had the bubble asked "same room?", this would have
/// had to run on every peer and cover chemists too — and a chemist query is
/// the one place a co-op bug hides in plain sight, since [`Chemist`] is
/// authority-only and a guest's own body never carries it. Keeping rooms
/// entirely server-side removes that whole class of failure instead of
/// guarding against it.
///
/// [`WalkableAreas::room_at`] returns `None` in a doorway by design — a
/// threshold belongs to neither of the rooms it joins. `docs/npc-ai.md` warns
/// that any rule reading rooms has to decide explicitly what a doorway means,
/// "because treating it as blind makes walking through a door a reliable way
/// to vanish"; here it would make one walk through a door read as two
/// arrivals. So a body holds its last known room until it is definitely
/// somewhere else.
#[derive(Resource, Default)]
pub struct RoomMemory(HashMap<Entity, String>);

impl RoomMemory {
    fn room_of(&self, entity: Entity) -> Option<&str> {
        self.0.get(&entity).map(String::as_str)
    }
}

/// Keeps [`RoomMemory`] current for everyone whose room anything asks about,
/// and forgets bodies that have gone.
///
/// The prune is not housekeeping for its own sake: crew are despawned when
/// they walk out of the station, several times a shift, all session. Without
/// it this map is a slow leak keyed by an entity id that will eventually be
/// reused.
/// Keeps [`RoomMemory`] current, and forgets bodies that have gone.
///
/// Crew only. Chemists are deliberately absent: nothing asks which room the
/// player is in any more, and adding them would be the first step back toward
/// the co-op trap [`RoomMemory`]'s own doc describes.
fn remember_rooms(
    areas: Option<Res<WalkableAreas>>,
    mut memory: ResMut<RoomMemory>,
    bodies: Query<(Entity, &Transform), With<CrewMember>>,
) {
    let Some(areas) = areas else {
        return;
    };
    let mut seen = Vec::with_capacity(memory.0.len());
    for (entity, transform) in &bodies {
        seen.push(entity);
        if let Some(room) = areas.room_at(transform.translation) {
            // Only a definite answer overwrites. A `None` in a doorway leaves
            // the previous room in place — that is the whole point.
            match memory.0.get(&entity) {
                Some(known) if known == room => {}
                _ => {
                    memory.0.insert(entity, room.to_string());
                }
            }
        }
    }
    // Not housekeeping for its own sake: crew are despawned when they walk
    // back out of the station, several times a shift, all session. Without
    // this the map is a slow leak keyed by an entity id that will eventually
    // be reused — and a reused id inheriting a stranger's last room would
    // silently cost the new occupant their arrival.
    memory.0.retain(|entity, _| seen.contains(entity));
}

// ---------------------------------------------------------------------------
// The authored pools
// ---------------------------------------------------------------------------

/// `assets/data/station.speech.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct SpeechScript {
    /// How long after speaking before the same body will speak unprompted
    /// again. Authored because it is pacing, and pacing is content — unlike
    /// [`DWELL_SECONDS`], nothing on a client needs to know it.
    pub cooldown_seconds: (f32, f32),
    pub arrivals: Vec<ArrivalLineDef>,
    pub errands: Vec<SpeechLineDef>,
    pub resolutions: Vec<ResolutionLineDef>,
    pub collapses: Vec<SpeechLineDef>,
    /// What they tell you when you walk up and ask — see the Conversation
    /// section below.
    pub gossip: Vec<GossipDef>,
    /// What they say once they have told you everything they have.
    pub exhausted: Vec<SpeechLineDef>,
    /// Two residents talking to each other, for you to overhear.
    pub exchanges: Vec<ExchangeDef>,
    pub exchange_gap_seconds: (f32, f32),
}

/// A short two-hander between two residents standing together.
///
/// Same shape as `radio::RadioExchangeDef`, deliberately: this is that idea
/// moved into the room. `when` is evaluated against the *opener*, so an
/// exchange can carry a clue and still obey the knowledge rule
/// [`Knows::known_by`] sets out — which is what makes eavesdropping worth
/// doing rather than merely atmospheric.
#[derive(Clone, Debug, Deserialize)]
pub struct ExchangeDef {
    /// Restricts who may open it. The replier is whoever they are standing
    /// with, which is the whole point of it being overheard rather than staged.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub when: Vec<Knows>,
    pub lines: Vec<SpeechLineDef>,
}

/// The shape every pool shares: optionally restricted to one department, and
/// substituted with `{name}` / `{role}` before it is spoken.
///
/// `role` mirrors [`crate::radio::RadioLineDef`]'s own field exactly, for the
/// same reason and with the same fallback rule: departments keep their own
/// voice where lines are written for them, and a general pool always exists so
/// a new role can never be struck mute.
#[derive(Clone, Debug, Deserialize)]
pub struct SpeechLineDef {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub tone: SpeechTone,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ArrivalLineDef {
    pub situation: Situation,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub tone: SpeechTone,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResolutionLineDef {
    pub outcome: Outcome,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub tone: SpeechTone,
    pub text: String,
}

/// Why this person is in your room.
///
/// Read off which components they are carrying, which is how every other
/// decision about an NPC in this codebase is made — see `docs/npc-ai.md`:
/// "behaviour is decided entirely by which marker components happen to be
/// attached".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Situation {
    /// On an [`Errand`] — crossing the room to do something with their hands.
    /// The saboteur heading for your glassware is this one.
    Errand,
    /// Carrying an [`Order`]: they came to collect something.
    Waiting,
    /// An [`Ambient`] resident, just moving through.
    Passing,
    /// A [`Pursuit`]. Short, and not friendly.
    Hostile,
}

/// This module's authored script, once loaded.
type Script = threat::Authored<SpeechScript>;

/// Picks a line, preferring one written for `role`.
///
/// Lifted wholesale from [`crate::radio::pick_line`]'s rule — role-specific
/// wins 60% of the time where one exists, with a general fallback — rather
/// than invented afresh, so the two channels weight a department's own voice
/// identically.
fn pick<'a, T>(
    lines: impl Iterator<Item = &'a T>,
    role_of: impl Fn(&T) -> Option<&str>,
    role: &str,
    rng: &mut impl Rng,
) -> Option<&'a T>
where
    T: 'a,
{
    let matching: Vec<&T> = lines.collect();
    let role_specific: Vec<&T> = matching
        .iter()
        .copied()
        .filter(|line| role_of(line) == Some(role))
        .collect();
    let general: Vec<&T> = matching
        .iter()
        .copied()
        .filter(|line| role_of(line).is_none())
        .collect();

    if !role_specific.is_empty() && rng.random_bool(0.6) {
        role_specific.choose(rng).copied()
    } else if !general.is_empty() {
        general.choose(rng).copied()
    } else {
        matching.first().copied()
    }
}

fn fill(text: &str, member: &CrewMember) -> String {
    text.replace("{name}", &member.name)
        .replace("{role}", &member.role)
}

// ---------------------------------------------------------------------------
// Barks
// ---------------------------------------------------------------------------

/// Someone walked into a room the chemist is in, and says so.
///
/// The signal this whole module exists for. Note what is deliberately *not*
/// here: no check of whether the visit is legitimate. An
/// [`IllicitOrder`](crate::orders::IllicitOrder) visitor draws from the same
/// pool as anyone else, because that marker's own doc comment is emphatic
/// that nothing on screen may ever mark a visit as suspicious — and a speech
/// system is exactly how that guarantee gets broken by accident. The tell an
/// attentive player can learn is in what the *situation* makes them say, the
/// same way `smuggler::loiter_smuggler` is a behavioural tell rather than a
/// label.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn notice_arrivals(
    mut commands: Commands,
    time: Res<Time>,
    script: Option<Res<Script>>,
    memory: Res<RoomMemory>,
    mut last_seen: Local<HashMap<Entity, String>>,
    chemists: Query<&Transform, (With<Chemist>, Without<CrewMember>)>,
    mut crew: Query<(
        Entity,
        &CrewMember,
        &Transform,
        Option<&mut SpeechCooldown>,
        Has<Errand>,
        Has<Pursuit>,
        Has<Ambient>,
        Has<Order>,
    )>,
) {
    let Some(script) = script else {
        return;
    };
    let dt = time.delta_secs();
    let listeners: Vec<Vec3> = chemists.iter().map(|at| at.translation).collect();
    let mut rng = rand::rng();

    for (entity, member, at, cooldown, errand, hostile, ambient, waiting) in &mut crew {
        // The *room* is what makes this an arrival rather than "is nearby":
        // a body already standing in the room the chemist walked into has not
        // arrived anywhere.
        let Some(room) = memory.room_of(entity) else {
            continue;
        };
        let arrived = last_seen
            .get(&entity)
            .is_none_or(|previous| previous != room);
        // Recorded whether or not they end up speaking, so a body that was on
        // cooldown when it walked in does not fire the moment the cooldown
        // lapses while it stands there.
        if arrived {
            last_seen.insert(entity, room.to_string());
        }

        if let Some(mut cooldown) = cooldown {
            cooldown.0 -= dt;
            if cooldown.0 > 0.0 {
                continue;
            }
        }
        let heard = listeners
            .iter()
            .any(|listener| listener.distance_squared(at.translation) <= EARSHOT * EARSHOT);
        if !arrived || !heard {
            continue;
        }

        // Order matters: an errand and a pursuit both replace `CrewRoute`, so
        // a body can be on one while still carrying the `Order` that sent it.
        // The thing they are doing right now is what they talk about.
        let situation = if hostile {
            Situation::Hostile
        } else if errand {
            Situation::Errand
        } else if waiting {
            Situation::Waiting
        } else if ambient {
            Situation::Passing
        } else {
            continue;
        };

        let Some(line) = pick(
            script
                .arrivals
                .iter()
                .filter(|line| line.situation == situation),
            |line| line.role.as_deref(),
            &member.role,
            &mut rng,
        ) else {
            continue;
        };
        say(&mut commands, entity, fill(&line.text, member), line.tone);
        commands.entity(entity).insert(SpeechCooldown(
            rng.random_range(script.cooldown_seconds.0..=script.cooldown_seconds.1),
        ));
    }

    last_seen.retain(|entity, _| crew.contains(*entity));
}

/// Someone set off across the room to do something.
///
/// Watches [`Errand`] itself rather than hooking `saboteur`, which is the one
/// consumer today. `Errand`'s own doc names `security`'s sweep as the next
/// intended user, so every future errand gets its line for free — and this is
/// the line that turns the saboteur's walk into something the player has the
/// length of the walk to answer.
fn notice_errands(
    mut commands: Commands,
    script: Option<Res<Script>>,
    starting: Query<(Entity, &CrewMember), Added<Errand>>,
) {
    let Some(script) = script else {
        return;
    };
    let mut rng = rand::rng();
    for (entity, member) in &starting {
        let Some(line) = pick(
            script.errands.iter(),
            |line| line.role.as_deref(),
            &member.role,
            &mut rng,
        ) else {
            continue;
        };
        say(&mut commands, entity, fill(&line.text, member), line.tone);
    }
}

/// What they say about how it went, before they turn around.
///
/// The radio's own report is untouched and still delayed — "a report that
/// lands the instant you hand a beaker over is a score popup" is still true
/// *of the radio*. This is the other half of that thought.
///
/// Matched by name rather than entity because [`OrderResolved`] carries no
/// entity, and adding one would touch every construction site of a message
/// five modules already read. Name is sound here for the reason `addiction`
/// gives for keying its whole table that way: the roster is the cast, every
/// recurring antagonist identity is deliberately kept *off* it so an ordinary
/// order can never double-book them, and `recall_resident_for_order` exists
/// precisely so one name is never two live bodies.
fn notice_resolutions(
    mut commands: Commands,
    script: Option<Res<Script>>,
    mut resolved: MessageReader<OrderResolved>,
    crew: Query<(Entity, &CrewMember)>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let mut rng = rand::rng();
    for report in resolved.read() {
        let Some((entity, member)) = crew.iter().find(|(_, member)| member.name == report.name)
        else {
            continue;
        };
        let Some(line) = pick(
            script
                .resolutions
                .iter()
                .filter(|line| line.outcome == report.outcome),
            |line| line.role.as_deref(),
            &member.role,
            &mut rng,
        ) else {
            continue;
        };
        say(&mut commands, entity, fill(&line.text, member), line.tone);
    }
}

/// One line for a body going down, for [`crate::crew::handle_crew_collapse`]
/// to speak.
///
/// Exposed rather than driven from a system here because that module already
/// holds the `Changed<Body>` query that notices a collapse, already decides
/// what it means, and already writes the radio's half of it. A second query
/// watching the same thing would be two systems that could disagree about
/// when someone fell over.
pub(crate) fn collapse_line(
    script: Option<&SpeechScript>,
    member: &CrewMember,
) -> Option<(String, SpeechTone)> {
    let script = script?;
    let mut rng = rand::rng();
    let line = pick(
        script.collapses.iter(),
        |line| line.role.as_deref(),
        &member.role,
        &mut rng,
    )?;
    Some((fill(&line.text, member), line.tone))
}

/// A soft tick when a line lands, so a chemist facing a machine panel still
/// knows to look up. The bubble alone cannot do that, and looking up is the
/// entire point of the module.
///
/// Reuses an already-credited one-shot rather than introducing an asset, the
/// same way the radio channel-ident palette does (see `CREDITS.md`). A
/// dedicated sample would be better and is worth doing before launch.
fn announce_speech(mut spoken: MessageWriter<PlaySfx>, started: Query<(), Added<Speech>>) {
    // One cue however many people spoke this frame — two lines landing
    // together are still one thing to look up at.
    if !started.is_empty() {
        spoken.write(PlaySfx(Sfx::Speak));
    }
}

// ---------------------------------------------------------------------------
// The bubble
// ---------------------------------------------------------------------------

/// A line being drawn over its speaker's head.
///
/// Spawned at the **root** of the UI tree on purpose: `capture::hide_hud`
/// hides root-level UI with `(With<Node>, Without<ChildOf>)`, so F9 screenshot
/// mode excludes bubbles with no per-panel marker and no change there.
#[derive(Component)]
struct SpeechBubble {
    speaker: Entity,
    elapsed: f32,
    lifetime: f32,
}

#[derive(Component)]
struct BubbleText;

fn spawn_bubbles(mut commands: Commands, started: Query<(Entity, &Speech), Added<Speech>>) {
    for (speaker, speech) in &started {
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                // Placed properly by `place_bubbles` on the same frame;
                // starting hidden means a bubble is never seen for one frame
                // in the top-left corner before its first projection.
                max_width: px(300),
                padding: UiRect::axes(px(10), px(6)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            Visibility::Hidden,
            BackgroundColor(Color::srgba(0.05, 0.06, 0.08, 0.88)),
            GlobalZIndex(30),
            SpeechBubble {
                speaker,
                elapsed: 0.0,
                lifetime: speech_seconds(&speech.text),
            },
            crate::until_we_leave_the_lab(),
            children![(
                Text::new(speech.text.clone()),
                TextFont::from_font_size(14.0),
                TextColor(speech.tone.color()),
                BubbleText,
            )],
        ));
    }
}

/// Projects every live bubble onto the screen, and decides which of them the
/// local chemist can actually see.
///
/// Three gates, cheapest first:
///
/// 1. **Same room.** Uses [`RoomMemory`], so a doorway does not blank it.
/// 2. **Not through a wall.** Reuses
///    [`authority_segment_blocked`](crate::interaction::authority_segment_blocked),
///    which is the segment/AABB test the lab already describes its walls for.
/// 3. **In front of the camera.** `world_to_viewport` errs behind it.
///
/// Then at most [`MAX_BUBBLES`] survive, nearest first.
#[allow(clippy::too_many_arguments)]
fn place_bubbles(
    time: Res<Time>,
    camera: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    speakers: Query<&Transform, With<Speech>>,
    solids: Query<(&Transform, &Solid)>,
    mut bubbles: Query<(&mut SpeechBubble, &mut Node, &mut Visibility, &Children)>,
    mut texts: Query<&mut TextColor, With<BubbleText>>,
) {
    let Ok((camera, camera_transform)) = camera.single() else {
        return;
    };
    // The camera, not the body: this is a question about what the player can
    // see, and their eyes are where the camera is.
    let eye = camera_transform.translation();

    // Everything that could be drawn, with the distance that decides which
    // ones win when more than `MAX_BUBBLES` want the screen.
    let mut candidates: Vec<(Entity, f32)> = Vec::new();
    for (bubble, _, _, _) in &bubbles {
        let Ok(speaker) = speakers.get(bubble.speaker) else {
            continue;
        };
        let head = speaker.translation + Vec3::Y * HEAD_HEIGHT;
        let range = eye.distance_squared(head);
        if range > EARSHOT * EARSHOT {
            continue;
        }
        // The only thing that decides whether you can read a line: is there a
        // wall in the way. Cheaper checks have already run.
        let blocked = solids.iter().any(|(transform, solid)| {
            authority_segment_blocked(eye, head, transform.translation, solid.half_extents)
        });
        if blocked {
            continue;
        }
        candidates.push((bubble.speaker, range));
    }
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
    candidates.truncate(MAX_BUBBLES);

    for (mut bubble, mut node, mut visibility, children) in &mut bubbles {
        bubble.elapsed += time.delta_secs();
        let shown = candidates
            .iter()
            .any(|(entity, _)| *entity == bubble.speaker);
        let projected = shown
            .then(|| speakers.get(bubble.speaker).ok())
            .flatten()
            .and_then(|speaker| {
                camera
                    .world_to_viewport(
                        camera_transform,
                        speaker.translation + Vec3::Y * HEAD_HEIGHT,
                    )
                    .ok()
            });

        let Some(at) = projected else {
            if *visibility != Visibility::Hidden {
                *visibility = Visibility::Hidden;
            }
            continue;
        };
        if *visibility != Visibility::Visible {
            *visibility = Visibility::Visible;
        }
        // `left`/`top` place the node's own corner, so the bubble is nudged
        // left of the anchor and up above the head rather than hanging off
        // the speaker's right shoulder. Not centred exactly — that would need
        // the node's measured width, a frame late.
        node.left = px(at.x - 90.0);
        node.top = px(at.y - 46.0);

        let alpha = (bubble.elapsed / FADE_IN).clamp(0.0, 1.0)
            * ((bubble.lifetime - bubble.elapsed) / FADE_OUT).clamp(0.0, 1.0);
        for child in children.iter() {
            if let Ok(mut color) = texts.get_mut(child) {
                let base = color.0.with_alpha(1.0);
                color.0 = base.with_alpha(alpha);
            }
        }
    }
}

/// Takes a bubble away when its line ends — and when its speaker does.
///
/// The liveness sweep is not defensive padding. Crew are despawned the moment
/// they walk back out of the station, several times a shift, and a despawn
/// takes the `Speech` component with it without ever producing a removal this
/// system would otherwise see. A bubble whose speaker is gone would sit on
/// screen for the rest of the session, which is the same class of leak
/// `showdown::a_finished_showdown_never_leaves_anything_behind` exists to
/// catch.
fn despawn_bubbles(
    mut commands: Commands,
    speaking: Query<(), With<Speech>>,
    bubbles: Query<(Entity, &SpeechBubble)>,
) {
    for (entity, bubble) in &bubbles {
        if !speaking.contains(bubble.speaker) {
            commands.entity(entity).despawn();
        }
    }
}

// ---------------------------------------------------------------------------
// Conversation
// ---------------------------------------------------------------------------
//
// What someone tells you when you walk up and ask.
//
// The point is not flavour, though it is that too. The game tracks a great
// deal about its own state that the player is never shown and, by deliberate
// design, mostly never should be: `Shift::npc_standing` holds a *separate*
// hidden opinion for every named crew member, and `SecuritySuspicion`,
// `UnderworldStanding`, `Instability`, `Campaign::plot` and the addict table
// are all invisible accumulators. The standing board shows one number per
// department and nothing else.
//
// That state is what makes a station worth walking around. So an NPC will tell
// you about it — in their own words, never as a number, and only ever the part
// of it a person in their job would plausibly have noticed. Security has heard
// people asking questions. Cargo notices glassware going missing. Medical sees
// who keeps coming in shaky.
//
// Which maps, deliberately, one-to-one onto the five department minor threads:
// each department gossips about its own antagonist, so the clue you get is
// about the trouble that department is actually the victim of.

/// How close to the ceiling a hidden meter has to climb before the people who
/// would notice start saying so.
///
/// Fractions of each meter's own `_MAX` rather than absolute numbers, so
/// retuning a ceiling cannot silently strand a whole tier of lines above
/// anything the meter can now reach — the failure
/// `arc::every_authored_threshold_sits_under_the_ceiling_of_the_meter_that_feeds_it`
/// exists to catch on the authored side.
const STIRRING: i32 = 4;
const LOUD: i32 = 2;

/// Standing at or above which someone is pleased with you, and at or below
/// which they are not.
///
/// Between zero and `estrangement::ESTRANGED_BELOW`, so there is a real band
/// of "cooling off" to say something about before the relationship becomes the
/// separate, mechanical fact that module owns.
const WARM: i32 = 6;
const COOL: i32 = -5;

/// The shortest a line may have been up before the next press interrupts it.
///
/// Not a cooldown so much as letting them finish the word. Without it, holding
/// the key down machine-guns both the bubble and its cue; with a full cooldown
/// the conversation feels like it is buffering.
const MIN_BEFORE_NEXT_REMARK: f32 = 0.9;

/// One thing a crew member might know.
///
/// Conditions, not facts: the authored line says which of these have to hold
/// for it to be sayable, and [`StationMood::holds`] answers each against the
/// live world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Knows {
    /// Their own opinion of you. Anyone can hold their own feelings.
    LikesYou,
    ResentsYou,
    /// Past `estrangement::ESTRANGED_BELOW` — they have stopped dealing with
    /// you, and `estrangement` has already told Security about it.
    Estranged,
    /// How their whole department feels, which they would hear in the break
    /// room whether or not it is about them.
    DepartmentPleased,
    /// People have been asking questions about the lab.
    SuspicionStirring,
    /// Security is close to acting on it.
    SuspicionHigh,
    /// There is a market for what you are making.
    UnderworldActive,
    /// Someone keeps turning up in a bad way.
    CrewGettingHooked,
    /// A casualty is being treated right now.
    CrisisActive,
    /// The station is fraying.
    StationFraying,
    /// It is worse than that.
    StationBreaking,
    /// Glassware and stock have stopped coming back.
    StockGoingMissing,
    /// Something is wrong with the crew and nobody can name it.
    PlotStirring,
    /// The station has narrowed it to a kind of threat.
    AntagSuspected,
    /// It has a name, and the name is on the standing board — so anyone may
    /// say this one.
    AntagNamed,
    /// This speaker personally has a habit, and is dry.
    Withdrawing,
    /// The lab has been shut long enough that the departments have started
    /// souring over it — see `shift::impatience`.
    LabHasBeenShut,
}

impl Knows {
    /// Which departments could plausibly hold this, or `None` for anyone.
    ///
    /// **The integrity rule of the whole feature, and the reason it is a
    /// method rather than a convention.** A line that hands the player a fact
    /// its speaker has no way of knowing is not a clue, it is a readout with a
    /// portrait attached — and the hidden meters are only interesting for as
    /// long as reaching them costs the player a walk to the right person.
    ///
    /// `every_authored_line_is_only_spoken_by_someone_who_could_know_it` walks
    /// the authored pool against this, so content cannot drift from it.
    ///
    /// Consulted only by that test, which is the right place for it: a bad
    /// line should fail the build loudly rather than be silently filtered at
    /// runtime, where it would look like an NPC mysteriously having nothing to
    /// say. Same shape as `arc`'s own authored-threshold guard. The
    /// `cfg_attr` follows `crew::ErrandGoal::Point`'s precedent — plain
    /// `cargo check` does not compile tests, so without it this reads as dead.
    #[cfg_attr(not(test), allow(dead_code))]
    fn known_by(self) -> Option<&'static [&'static str]> {
        match self {
            // Their own relationship with you, and their own body. Universal.
            Knows::LikesYou
            | Knows::ResentsYou
            | Knows::Estranged
            | Knows::DepartmentPleased
            | Knows::Withdrawing => None,
            // Everyone can see the sign, and everyone has been waiting on it.
            Knows::LabHasBeenShut => None,
            // On the standing board already — no longer a secret to keep.
            Knows::AntagNamed => None,
            Knows::SuspicionStirring | Knows::SuspicionHigh | Knows::UnderworldActive => {
                Some(&["Security"])
            }
            Knows::CrewGettingHooked | Knows::CrisisActive => Some(&["Medical"]),
            Knows::StationFraying | Knows::StationBreaking => Some(&["Engineering"]),
            Knows::StockGoingMissing => Some(&["Cargo"]),
            Knows::PlotStirring | Knows::AntagSuspected => Some(&["Service"]),
        }
    }
}

/// One thing someone might say when asked.
#[derive(Clone, Debug, Deserialize)]
pub struct GossipDef {
    #[serde(default)]
    pub role: Option<String>,
    /// Everything here must hold. An empty list is small talk, always
    /// available — which is what keeps anyone from ever being mute.
    #[serde(default)]
    pub when: Vec<Knows>,
    /// Higher wins. A raid coming outranks a joke about the coffee.
    #[serde(default)]
    pub weight: i32,
    #[serde(default)]
    pub tone: SpeechTone,
    pub text: String,
}

/// Everything a speaker could draw on, gathered once per question asked.
///
/// A [`SystemParam`] rather than a dozen parameters on the handler, following
/// `ui::PanelViews`. Every resource is optional: a headless test app, and the
/// first frames of a real session, legitimately have none of them, and a
/// missing meter should read as "nothing to report" rather than panic.
#[derive(bevy::ecs::system::SystemParam)]
pub struct StationMood<'w, 's> {
    shift: Option<Res<'w, crate::orders::Shift>>,
    estranged: Option<Res<'w, crate::estrangement::Estranged>>,
    suspicion: Option<Res<'w, crate::antagonist::SecuritySuspicion>>,
    underworld: Option<Res<'w, crate::antagonist::UnderworldStanding>>,
    instability: Option<Res<'w, crate::instability::Instability>>,
    campaign: Option<Res<'w, crate::arc::Campaign>>,
    addictions: Option<Res<'w, crate::addiction::Addictions>>,
    addiction_script: Option<Res<'w, threat::Authored<crate::addiction::AddictionScript>>>,
    crises: Query<'w, 's, (), With<crate::orders::CrisisOrder>>,
}

impl StationMood<'_, '_> {
    fn holds(&self, what: Knows, member: &CrewMember) -> bool {
        let standing = self
            .shift
            .as_ref()
            .map_or(0, |shift| shift.npc_standing(&member.name));
        let department = self
            .shift
            .as_ref()
            .zip(crate::orders::Department::from_role(&member.role))
            .map_or(0, |(shift, department)| shift.standing(department));
        let suspicion = self.suspicion.as_ref().map_or(0, |meter| meter.level());
        let underworld = self.underworld.as_ref().map_or(0, |meter| meter.level());

        match what {
            Knows::LikesYou => standing >= WARM,
            Knows::ResentsYou => standing <= COOL,
            Knows::Estranged => self
                .estranged
                .as_ref()
                .is_some_and(|estranged| estranged.0.contains(&member.name)),
            Knows::DepartmentPleased => department >= WARM,
            Knows::SuspicionStirring => suspicion >= crate::antagonist::SUSPICION_MAX / STIRRING,
            Knows::SuspicionHigh => suspicion >= crate::antagonist::SUSPICION_MAX / LOUD,
            Knows::UnderworldActive => underworld >= crate::antagonist::UNDERWORLD_MAX / STIRRING,
            // Anyone hooked at all, not this speaker — Medical sees the ward,
            // not their own arm. `Withdrawing` below is the personal one.
            Knows::CrewGettingHooked => self.hooked_crew() > 0,
            Knows::CrisisActive => !self.crises.is_empty(),
            Knows::StationFraying => self.tier() >= crate::instability::StabilityBand::Strained,
            Knows::StationBreaking => self.tier() >= crate::instability::StabilityBand::Critical,
            // Cargo's own minor is the smuggler, whose thefts are what
            // `UnderworldStanding` rising actually looks like from the dock.
            Knows::StockGoingMissing => underworld > 0,
            Knows::PlotStirring => self
                .campaign
                .as_ref()
                .is_some_and(|campaign| campaign.plot >= crate::arc::PLOT_MAX / STIRRING),
            Knows::AntagSuspected => self.reveal() >= crate::arc::Reveal::Suspected,
            Knows::AntagNamed => self.reveal() >= crate::arc::Reveal::Named,
            Knows::Withdrawing => self.is_dry(&member.name),
            // Reads the same replicated field the HUD banner draws from, so
            // there is one answer to "has this gone on too long" rather than
            // two that could drift. `shift::impatience` zeroes it on
            // reopening, which is what makes this stop being said.
            Knows::LabHasBeenShut => self
                .shift
                .as_ref()
                .is_some_and(|shift| shift.closure_pressure > 0),
        }
    }

    fn tier(&self) -> crate::instability::StabilityBand {
        self.instability
            .as_ref()
            .map_or(Default::default(), |meter| meter.band)
    }

    fn reveal(&self) -> crate::arc::Reveal {
        self.campaign
            .as_ref()
            .map_or(Default::default(), |campaign| campaign.reveal)
    }

    fn hooked_crew(&self) -> usize {
        match (self.addictions.as_ref(), self.addiction_script.as_ref()) {
            (Some(addictions), Some(script)) => addictions.hooked(&script.0).len(),
            _ => 0,
        }
    }

    fn is_dry(&self, name: &str) -> bool {
        let (Some(addictions), Some(script)) =
            (self.addictions.as_ref(), self.addiction_script.as_ref())
        else {
            return false;
        };
        addictions
            .hooked(&script.0)
            .into_iter()
            .any(|(who, habit)| who == name && habit.dry_for > 0.0)
    }
}

/// Which authored remarks this person has already given you.
///
/// Server-side, by index into the authored pool. Deliberately **never
/// cleared**: an NPC repeating themselves is the signal that nothing has
/// changed since you last asked, and a new condition coming true adds a line
/// they have not said yet all on its own. That falls out of "pick the best
/// thing not yet said" without any refresh timer, any invalidation, or any way
/// for the two to disagree.
#[derive(Component, Default)]
pub struct Remarks {
    spoken: Vec<usize>,
}

#[derive(Component, Default)]
struct SocialRemarks {
    personal_stage: u8,
    evidence_stage: u8,
}

fn personal_relationship_line(
    member: &CrewMember,
    social: &crate::social::SocialState,
) -> Option<(String, SpeechTone)> {
    let relationship = social.relationships.get(&member.name)?;
    if relationship.last_outcome == crate::social::FavorOutcome::Unresolved {
        return None;
    }
    let profile = social.profile(&member.name)?;
    let line = match (relationship.last_outcome, profile.temperament) {
        (crate::social::FavorOutcome::Helped, crate::social::Temperament::Warm) => {
            "You showed up when I needed someone. That matters."
        }
        (crate::social::FavorOutcome::Helped, crate::social::Temperament::Blunt) => {
            "You did the job. I trust that more than promises."
        }
        (crate::social::FavorOutcome::Helped, crate::social::Temperament::Cautious) => {
            "You handled my request carefully. I noticed."
        }
        (crate::social::FavorOutcome::Helped, crate::social::Temperament::Exacting) => {
            "The handoff met every requirement. Good work."
        }
        (crate::social::FavorOutcome::Deceived, _) => {
            "I checked what you handed me. The facts did not match the story."
        }
        (crate::social::FavorOutcome::Refused, _) => {
            "You left my request hanging. I had to remember that."
        }
        (crate::social::FavorOutcome::Compromised, crate::social::Temperament::Warm) => {
            "I know you meant to help. The result still put someone at risk."
        }
        (crate::social::FavorOutcome::Compromised, crate::social::Temperament::Blunt) => {
            "Close enough is how people get hurt."
        }
        (crate::social::FavorOutcome::Compromised, crate::social::Temperament::Cautious) => {
            "I cannot rely on a handoff with that many loose ends."
        }
        (crate::social::FavorOutcome::Compromised, crate::social::Temperament::Exacting) => {
            "The sample failed the conditions I gave you."
        }
        _ => return None,
    };
    Some((line.into(), SpeechTone::Wary))
}

fn evidence_suspicion_line(
    member: &CrewMember,
    social: &crate::social::SocialState,
) -> Option<(String, SpeechTone)> {
    let selected = social.resident_antagonist?;
    if social.antagonist_resolution == crate::social::AntagonistResolution::Turned
        && member.name == selected.resident()
    {
        let text = match selected {
            crate::social::ResidentAntagonist::OkonkwoQuack =>
                "Quiet warning: somebody is shopping for another off-chart dose. Check Medical's real log.",
            crate::social::ResidentAntagonist::SatoSmuggler =>
                "Quiet warning: the next clean manifest is the suspicious one. Cargo is laundering a route.",
            crate::social::ResidentAntagonist::ReyesBentGuard =>
                "Quiet warning: an inspection is being timed around the evidence locker. Do not leave it open.",
        };
        return Some((text.into(), SpeechTone::Wary));
    }
    if social.evidence_progress == 0
        || matches!(
            social.antagonist_resolution,
            crate::social::AntagonistResolution::Reported
        )
    {
        return None;
    }
    if member.name == selected.resident() {
        let profile = social.profile(&member.name)?;
        let text = match profile.temperament {
            crate::social::Temperament::Warm =>
                "The paperwork looks strange because I was helping someone who had nowhere else to go.",
            crate::social::Temperament::Blunt =>
                "A discrepancy is not a crime. Either act on it or get out of my way.",
            crate::social::Temperament::Cautious =>
                "Keep the seals intact and follow the chain. Guessing will only contaminate it.",
            crate::social::Temperament::Exacting =>
                "Check the dose, label, time, and manifest. Precision will explain more than suspicion.",
        };
        return Some((text.into(), SpeechTone::Wary));
    }
    if member.name == crate::social::BEX && social.evidence_progress >= 2 {
        return Some((
            "One inconsistency is noise. A consequence and a matching item make a case. Bring me the physical link.".into(),
            SpeechTone::Wary,
        ));
    }
    None
}

/// Answers a chemist who walked up and asked.
///
/// Triggered by an ordinary [`InteractRequested`] at a [`CrewMember`] from
/// someone **holding nothing** — and that condition is not arbitrary, it is
/// the one genuinely unclaimed seam in the whole interaction fan-out. Every
/// other reader that acts on a crew member — `orders::handle_delivery`,
/// `rogue_security::handle_rogue_delivery`, `cult::handle_incident_delivery`,
/// `showdown::handle_breach_delivery`,
/// `antagonist::handle_illicit_offer_pickup` — begins by finding a container
/// held by the sender and gives up when there is none. So an empty hand can
/// never double-fire with any of them, which is why this needs no ordering
/// against them at all. `talking_never_steals_a_press_that_would_have_been_a_delivery`
/// pins that.
///
/// The other half of the rule is the player's: a full hand means business, an
/// empty one means conversation. That is already how `[F]` disambiguates
/// itself, and the prompt says which you are about to get.
#[allow(clippy::too_many_arguments)]
fn handle_talk(
    mut commands: Commands,
    script: Option<Res<Script>>,
    mood: StationMood,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<&crate::containers::HeldBy>,
    crew: Query<(
        &CrewMember,
        Option<&Remarks>,
        Option<&SpeechTimer>,
        Option<&crate::social::PersonalFavor>,
        Option<&SocialRemarks>,
    )>,
    social: Option<Res<crate::social::SocialState>>,
) {
    let Some(script) = script else {
        requests.clear();
        return;
    };
    for request in requests.read() {
        let Some(player) = crate::machines::chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        // A full hand is somebody else's press. See the doc above.
        if held.iter().any(|holder| holder.0 == player) {
            continue;
        }
        let Ok((member, remarks, talking, favor, social_remarks)) = crew.get(request.target) else {
            continue;
        };
        // Let them finish the word before starting the next one.
        if talking.is_some_and(|timer| timer.0.elapsed_secs() < MIN_BEFORE_NEXT_REMARK) {
            continue;
        }

        if let Some(favor) = favor {
            say(
                &mut commands,
                request.target,
                favor.summary.clone(),
                SpeechTone::Wary,
            );
            continue;
        }
        if let Some(social) = social.as_deref() {
            let spoken_personal = social_remarks.map_or(0, |remarks| remarks.personal_stage);
            let chain_stage = social
                .relationships
                .get(&member.name)
                .map_or(0, |relationship| relationship.chain_stage);
            if chain_stage > spoken_personal {
                if let Some(line) = personal_relationship_line(member, social) {
                    say(&mut commands, request.target, line.0, line.1);
                    commands.entity(request.target).insert(SocialRemarks {
                        personal_stage: chain_stage,
                        evidence_stage: social_remarks.map_or(0, |remarks| remarks.evidence_stage),
                    });
                    continue;
                }
            }
            let spoken_evidence = social_remarks.map_or(0, |remarks| remarks.evidence_stage);
            if social.evidence_progress > spoken_evidence {
                if let Some(line) = evidence_suspicion_line(member, social) {
                    say(&mut commands, request.target, line.0, line.1);
                    commands.entity(request.target).insert(SocialRemarks {
                        personal_stage: chain_stage,
                        evidence_stage: social.evidence_progress,
                    });
                    continue;
                }
            }
        }

        let spoken = remarks
            .map(|remarks| remarks.spoken.as_slice())
            .unwrap_or(&[]);
        let (line, index) = next_remark(&script, &mood, member, spoken);
        say(&mut commands, request.target, fill(&line.0, member), line.1);
        if let Some(index) = index {
            let mut updated = spoken.to_vec();
            updated.push(index);
            commands
                .entity(request.target)
                .insert(Remarks { spoken: updated });
        }
    }
}

/// The heaviest thing in `gossip` that passes `eligible` and has not been said.
///
/// Split out of [`next_remark`] and made pure specifically so the priority
/// ladder can be tested against a pool where authored order and weight
/// *disagree*. Testing it only through the real script proved nothing: the
/// authored file happens to list its conditional lines before its small talk,
/// so replacing this ordering with plain authored order still produced the
/// right answer, and a guard that passes with the fix removed is not a guard.
fn best_unspoken<'a>(
    gossip: &'a [GossipDef],
    spoken: &[usize],
    eligible: impl Fn(&GossipDef) -> bool,
) -> Option<(usize, &'a GossipDef)> {
    gossip
        .iter()
        .enumerate()
        .filter(|(index, line)| !spoken.contains(index) && eligible(line))
        // Heaviest first; ties broken by authored order, so a pool of equals
        // reads the way it was written rather than however the filter
        // happened to produce it.
        .max_by(|a, b| a.1.weight.cmp(&b.1.weight).then(b.0.cmp(&a.0)))
}

/// The best thing this person has left to say, and which authored entry it was.
///
/// `None` for the index means they had nothing new and fell back to the
/// exhausted pool — which must not be recorded, or the fallback would itself
/// be used up.
///
/// Pure, so the whole priority ladder is testable without a world: eligibility,
/// the knowledge restriction, weighting, and running dry.
fn next_remark(
    script: &SpeechScript,
    mood: &StationMood,
    member: &CrewMember,
    spoken: &[usize],
) -> ((String, SpeechTone), Option<usize>) {
    let best = best_unspoken(&script.gossip, spoken, |line| {
        line.role.as_deref().is_none_or(|role| role == member.role)
            && line.when.iter().all(|what| mood.holds(*what, member))
    });

    match best {
        Some((index, line)) => ((line.text.clone(), line.tone), Some(index)),
        None => {
            let mut rng = rand::rng();
            let line = pick(
                script.exhausted.iter(),
                |line| line.role.as_deref(),
                &member.role,
                &mut rng,
            );
            (
                line.map_or_else(
                    || ("That's all I've got.".to_string(), SpeechTone::Neutral),
                    |line| (line.text.clone(), line.tone),
                ),
                None,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Overheard exchanges
// ---------------------------------------------------------------------------

/// How close two residents have to be standing to be talking to each other.
const CHAT_RANGE: f32 = 2.6;
/// Gap between two people trading lines. Long enough to read the first.
const EXCHANGE_BEAT_SECONDS: (f32, f32) = (2.2, 3.4);

/// Server-side scheduler for overheard exchanges, shaped like
/// `radio::AmbientRadio` — including its exhaust-the-bag draw, so the same
/// two-hander does not come round twice in a row.
#[derive(Resource)]
struct Chatter {
    timer: Timer,
    bag: Vec<usize>,
}

impl Default for Chatter {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(45.0, TimerMode::Once),
            bag: Vec::new(),
        }
    }
}

fn reset_chatter(mut chatter: ResMut<Chatter>) {
    *chatter = Chatter::default();
}

/// Lines waiting on their beat. The second half of a two-hander, held until
/// the first has been heard — `radio::PendingBroadcasts` with a speaker
/// attached.
#[derive(Resource, Default)]
struct PendingRemarks(Vec<(Timer, Entity, String, SpeechTone)>);

/// Puts two idle residents standing near each other into conversation.
///
/// Suppressed by exactly what suppresses `radio::tick_ambient_radio`, and for
/// the same reason: two people gossiping about the coffee while a casualty is
/// bleeding on the counter is not atmosphere, it is the game not noticing what
/// it is doing.
///
/// Requires a chemist within [`EARSHOT`] before it will spend an exchange at
/// all. There is no point burning one on an empty corridor, and the bag would
/// drain all shift on conversations nobody was there for.
#[allow(clippy::too_many_arguments)]
fn start_exchanges(
    time: Res<Time>,
    script: Option<Res<Script>>,
    mood: StationMood,
    orders: Query<&Order>,
    hazards: Query<(), With<crate::hazards::ActiveHazard>>,
    showdown: Option<Res<crate::showdown::Showdown>>,
    chemists: Query<&Transform, (With<Chemist>, Without<CrewMember>)>,
    residents: ResidentQuery,
    mut chatter: ResMut<Chatter>,
    mut pending: ResMut<PendingRemarks>,
    mut commands: Commands,
) {
    let Some(script) = script else {
        return;
    };
    // Deliberately *not* the whole of `radio::tick_ambient_radio`'s suppressor
    // list: that one also stops while the sign is down, and this must not.
    // The sign being down means chemistry is closed, not that the station has
    // emptied — and it is exactly when the player has time to walk the halls
    // and listen. Going quiet then would silence the feature at the only
    // moment there is room to use it. Every other suppressor is kept: those
    // are all "something is actually wrong", and two people discussing the
    // shuttle over a casualty is the game not noticing what it is doing.
    let quiet = orders.iter().all(|order| order.remaining() >= 30.0)
        && mood.crises.is_empty()
        && hazards.is_empty()
        && showdown.is_none();
    if !quiet || !chatter.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    let Some(pair) = idle_pair(&residents, &chemists, &mut rng) else {
        // Nobody to talk to, or nobody around to overhear. Try again shortly
        // rather than burning the full gap — the lab empties and refills
        // constantly, and a missed roll should not mean a quiet minute.
        chatter.timer = Timer::from_seconds(rng.random_range(8.0..=14.0), TimerMode::Once);
        return;
    };

    let (Ok(opener), Ok(replier)) = (residents.get(pair.0), residents.get(pair.1)) else {
        return;
    };
    let (first, second) = ((pair.0, opener.1), (pair.1, replier.1));

    if script.exchanges.is_empty() {
        return;
    }
    if chatter.bag.is_empty() {
        chatter.bag.extend(0..script.exchanges.len());
        chatter.bag.shuffle(&mut rng);
    }
    // Draw the first exchange in the bag whose conditions the opener could
    // actually hold. Draining rather than scanning keeps the no-repeat
    // guarantee: a rejected exchange is put back, an accepted one is spent.
    let mut rejected = Vec::new();
    let chosen = loop {
        let Some(index) = chatter.bag.pop() else {
            break None;
        };
        let exchange = &script.exchanges[index];
        if exchange
            .role
            .as_deref()
            .is_none_or(|role| role == first.1.role)
            && exchange.when.iter().all(|what| mood.holds(*what, first.1))
            && exchange.lines.len() >= 2
        {
            break Some(exchange);
        }
        rejected.push(index);
    };
    chatter.bag.extend(rejected);

    if let Some(exchange) = chosen {
        let mut delay = 0.0;
        for (turn, line) in exchange.lines.iter().enumerate() {
            // Alternating, so a three-line exchange is A-B-A: two people
            // talking, not a queue taking turns at a microphone.
            let (speaker, member) = if turn % 2 == 0 { first } else { second };
            let text = fill(&line.text, member);
            if delay == 0.0 {
                say(&mut commands, speaker, text, line.tone);
            } else {
                pending.0.push((
                    Timer::from_seconds(delay, TimerMode::Once),
                    speaker,
                    text,
                    line.tone,
                ));
            }
            delay += rng.random_range(EXCHANGE_BEAT_SECONDS.0..=EXCHANGE_BEAT_SECONDS.1);
        }
    }

    let gap = script.exchange_gap_seconds;
    chatter.timer = Timer::from_seconds(rng.random_range(gap.0..=gap.1), TimerMode::Once);
}

type ResidentQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static CrewMember,
        &'static Transform,
        &'static crate::crew::CrewRoute,
    ),
    With<Ambient>,
>;

/// Two residents standing still together, with a chemist close enough to hear
/// them.
///
/// Hands back entities rather than `&CrewMember` deliberately: borrowing out of
/// the query would tie the pair to the query's own lifetime and stop the caller
/// touching it again, and the caller has to — an exchange alternates between
/// the two speakers and needs each one's name every turn.
fn idle_pair(
    residents: &ResidentQuery,
    chemists: &Query<&Transform, (With<Chemist>, Without<CrewMember>)>,
    rng: &mut impl Rng,
) -> Option<(Entity, Entity)> {
    let listeners: Vec<Vec3> = chemists.iter().map(|at| at.translation).collect();
    if listeners.is_empty() {
        return None;
    }
    let standing: Vec<(Entity, Vec3)> = residents
        .iter()
        .filter(|(_, _, _, route)| !route.is_moving())
        .map(|(entity, _, at, _)| (entity, at.translation))
        .filter(|(_, at)| {
            listeners
                .iter()
                .any(|listener| listener.distance_squared(*at) <= EARSHOT * EARSHOT)
        })
        .collect();

    let mut pairs = Vec::new();
    for (index, (entity, at)) in standing.iter().enumerate() {
        for (other, other_at) in standing.iter().skip(index + 1) {
            if at.distance_squared(*other_at) <= CHAT_RANGE * CHAT_RANGE {
                pairs.push((*entity, *other));
            }
        }
    }
    pairs.choose(rng).copied()
}

/// Lands the second half of a two-hander when its beat comes round.
///
/// Drops a line whose speaker has walked off or been despawned rather than
/// reviving them: a reply nobody is there to give is not a reply.
fn deliver_remarks(
    mut commands: Commands,
    time: Res<Time>,
    mut pending: ResMut<PendingRemarks>,
    alive: Query<(), With<CrewMember>>,
) {
    let mut due = Vec::new();
    pending.0.retain_mut(|(timer, speaker, text, tone)| {
        if timer.tick(time.delta()).just_finished() {
            due.push((*speaker, text.clone(), *tone));
            false
        } else {
            true
        }
    });
    for (speaker, text, tone) in due {
        if alive.contains(speaker) {
            say(&mut commands, speaker, text, tone);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> SpeechScript {
        ron::from_str(include_str!("../../assets/data/station.speech.ron")).unwrap()
    }

    const DEPARTMENTS: [&str; 5] = ["Medical", "Security", "Engineering", "Cargo", "Service"];

    #[test]
    fn every_situation_has_a_role_agnostic_line() {
        // A situation with only role-specific lines is silence for anyone
        // whose department nobody wrote for — and silence exactly when the
        // player needed the signal.
        let script = script();
        for situation in [
            Situation::Errand,
            Situation::Waiting,
            Situation::Passing,
            Situation::Hostile,
        ] {
            let general = script
                .arrivals
                .iter()
                .filter(|line| line.situation == situation && line.role.is_none())
                .count();
            assert!(
                general >= 2,
                "{situation:?} needs two role-agnostic fallbacks so an \
                 unwritten department stays varied"
            );
        }
    }

    #[test]
    fn every_outcome_has_a_line_to_say_about_it() {
        let script = script();
        for outcome in [
            Outcome::Success,
            Outcome::Short,
            Outcome::Impure,
            Outcome::Overdose,
            Outcome::Wrong,
            Outcome::Expired,
        ] {
            let general = script
                .resolutions
                .iter()
                .filter(|line| line.outcome == outcome && line.role.is_none())
                .count();
            assert!(general >= 2, "{outcome:?} needs two role-agnostic lines");
        }
    }

    #[test]
    fn every_department_has_an_arrival_line_of_its_own() {
        let script = script();
        for role in DEPARTMENTS {
            assert!(
                script
                    .arrivals
                    .iter()
                    .any(|line| line.role.as_deref() == Some(role)),
                "{role} has no voice of its own on arrival"
            );
        }
    }

    #[test]
    fn nothing_is_authored_blank() {
        let script = script();
        let every_text = script
            .arrivals
            .iter()
            .map(|line| &line.text)
            .chain(script.errands.iter().map(|line| &line.text))
            .chain(script.resolutions.iter().map(|line| &line.text))
            .chain(script.collapses.iter().map(|line| &line.text));
        assert!(every_text.into_iter().all(|text| !text.trim().is_empty()));
    }

    /// An app with the real authored script and one chemist standing at the
    /// origin, in `room`.
    fn arrival_app(room: &str) -> (App, Entity) {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(threat::Authored(script()))
            .init_resource::<RoomMemory>()
            .add_systems(Update, notice_arrivals);

        let chemist = app
            .world_mut()
            .spawn((
                Chemist {
                    client: bevy_replicon::prelude::ClientId::Server,
                },
                Transform::from_xyz(0.0, crate::crew::BODY_OFFSET, 0.0),
            ))
            .id();
        app.world_mut()
            .resource_mut::<RoomMemory>()
            .0
            .insert(chemist, room.to_string());
        (app, chemist)
    }

    /// Someone appearing in `room`, `metres` from the chemist.
    fn walks_in(
        app: &mut App,
        room: &str,
        metres: f32,
        name: &str,
        role: &str,
        extra: impl Bundle,
    ) -> Entity {
        let crew = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: name.to_string(),
                    role: role.to_string(),
                },
                Transform::from_xyz(metres, crate::crew::BODY_OFFSET, 0.0),
                extra,
            ))
            .id();
        app.world_mut()
            .resource_mut::<RoomMemory>()
            .0
            .insert(crew, room.to_string());
        crew
    }

    fn an_order() -> Order {
        Order {
            reagent: chem_sim::ReagentId(0),
            specific: false,
            minimum_purity: 0.0,
            amount: chem_sim::Units::whole(5),
            plea: "Test order".to_string(),
            patience: 60.0,
            waited: 0.0,
        }
    }

    #[test]
    fn an_illicit_visitor_draws_from_the_same_arrival_pool_as_a_legitimate_one() {
        // `IllicitOrder`'s own doc comment is emphatic: nothing on screen may
        // ever mark a visit as suspicious. A speech system is exactly how that
        // guarantee gets broken by accident, so this asserts the property
        // where it actually lives — two visitors, identical but for the
        // marker, both saying something out of the same pool.
        let (mut app, _) = arrival_app("Mixing Hall");
        let honest = walks_in(
            &mut app,
            "Mixing Hall",
            2.0,
            "Nurse Okonkwo",
            "Medical",
            an_order(),
        );
        let illicit = walks_in(
            &mut app,
            "Mixing Hall",
            3.0,
            "Tech Boyle",
            "Engineering",
            an_order(),
        );
        app.world_mut()
            .entity_mut(illicit)
            .insert(crate::orders::IllicitOrder);

        app.update();

        let authored = script();
        let waiting: Vec<&str> = authored
            .arrivals
            .iter()
            .filter(|line| line.situation == Situation::Waiting)
            .map(|line| line.text.as_str())
            .collect();
        for (who, label) in [
            (honest, "a legitimate visitor"),
            (illicit, "an illicit one"),
        ] {
            let said = app
                .world()
                .get::<Speech>(who)
                .unwrap_or_else(|| panic!("{label} said nothing on arrival"));
            assert!(
                waiting.contains(&said.text.as_str()),
                "{label} said {:?}, which is not in the Waiting pool",
                said.text
            );
        }
    }

    #[test]
    fn a_line_stays_up_long_enough_to_read_and_not_longer() {
        // The floor: the shortest thing anyone can say still gets long enough
        // to be noticed, which is a separate cost from reading it.
        assert_eq!(speech_seconds("Hi."), DWELL_SECONDS.0);
        // The ceiling: a pathologically long authored line cannot camp the
        // screen. Nothing in `station.speech.ron` reaches it, by design.
        assert_eq!(speech_seconds(&"a".repeat(400)), DWELL_SECONDS.1);
        // In between, longer means longer.
        let short = speech_seconds("Don't mind me.");
        let long = speech_seconds("Just going to check your glassware while I'm here.");
        assert!(short > DWELL_SECONDS.0 && long < DWELL_SECONDS.1);
        assert!(long > short);
    }

    #[test]
    fn every_authored_line_is_short_enough_to_read_while_someone_walks_past() {
        // The ceiling above is a guard, not a target. A line that hits it is a
        // line the player is reading instead of watching the room — which for
        // an arrival bark defeats the point.
        let script = script();
        for text in script
            .arrivals
            .iter()
            .map(|line| &line.text)
            .chain(script.errands.iter().map(|line| &line.text))
            .chain(script.resolutions.iter().map(|line| &line.text))
            .chain(script.collapses.iter().map(|line| &line.text))
        {
            assert!(
                speech_seconds(text) < DWELL_SECONDS.1,
                "{text:?} is long enough to hit the dwell ceiling — say it shorter"
            );
        }
    }

    #[test]
    fn nobody_greets_a_chemist_too_far_away_to_hear_them() {
        // The bark is a signal that someone came *to you*. Firing it station-
        // wide would mean every resident wandering between departments all
        // shift is speaking to nobody, which is both noise and a lie.
        let (mut app, _) = arrival_app("Mixing Hall");
        let elsewhere = walks_in(
            &mut app,
            "Cargo Bay",
            EARSHOT + 5.0,
            "Miner Sato",
            "Cargo",
            Ambient::new(5.0),
        );

        app.update();

        assert!(
            app.world().get::<Speech>(elsewhere).is_none(),
            "someone across the station greeted a chemist who cannot hear them"
        );
    }

    #[test]
    fn a_customer_at_the_counter_is_heard_from_the_next_room() {
        // The case a strict same-room rule got wrong, and the reason `EARSHOT`
        // is a distance: the chemist works in the Mixing Hall and every
        // customer in the game walks up to a counter in the Lobby next door.
        // Silent there would have left the single most common arrival unsignalled.
        let (mut app, _) = arrival_app("Mixing Hall");
        let customer = walks_in(
            &mut app,
            "Lobby",
            EARSHOT - 3.0,
            "Dr. Vance",
            "Medical",
            an_order(),
        );

        app.update();

        assert!(
            app.world().get::<Speech>(customer).is_some(),
            "a customer one room over went unheard"
        );
    }

    #[test]
    fn walking_back_and_forth_through_a_doorway_is_not_a_conversation() {
        // A resident whose route clips the lab twice in ten seconds must not
        // greet you twice. `SpeechCooldown` is what stops it, and this is the
        // behaviour that would make the whole feature obnoxious without it.
        let (mut app, _) = arrival_app("Mixing Hall");
        let resident = walks_in(
            &mut app,
            "Mixing Hall",
            2.0,
            "Botanist Ivy",
            "Service",
            Ambient::new(5.0),
        );

        app.update();
        assert!(
            app.world().get::<Speech>(resident).is_some(),
            "the first arrival should be greeted"
        );

        // Out into the corridor and straight back in.
        app.world_mut().entity_mut(resident).remove::<Speech>();
        for room in ["Lobby", "Mixing Hall"] {
            app.world_mut()
                .resource_mut::<RoomMemory>()
                .0
                .insert(resident, room.to_string());
            app.update();
        }

        assert!(
            app.world().get::<Speech>(resident).is_none(),
            "they greeted the same chemist twice inside the cooldown"
        );
    }

    #[test]
    fn both_ends_agree_on_how_long_a_line_lasts() {
        // The authority's `SpeechTimer` and every client's bubble lifetime are
        // both this one function of the text, which is what lets `Speech`
        // carry no clock across the wire. If this ever needed the script, a
        // client — which never loads it — would fade at a different time from
        // the host's removal.
        let text = "Don't mind me.";
        let mut app = App::new();
        app.add_systems(Update, move |mut commands: Commands| {
            let speaker = commands.spawn_empty().id();
            say(&mut commands, speaker, text, SpeechTone::Neutral);
        });
        app.update();
        let timer = app
            .world_mut()
            .query::<&SpeechTimer>()
            .single(app.world())
            .expect("a line was said");
        assert_eq!(timer.0.duration().as_secs_f32(), speech_seconds(text));
    }

    #[test]
    fn a_bubble_never_outlives_the_body_that_said_it() {
        // The leak this guards is real and routine: crew despawn when they
        // walk out, which takes `Speech` with them without ever producing a
        // removal event.
        let mut app = App::new();
        app.add_systems(Update, despawn_bubbles);

        let speaker = app
            .world_mut()
            .spawn(Speech {
                text: "Don't mind me.".to_string(),
                tone: SpeechTone::Neutral,
            })
            .id();
        let bubble = app
            .world_mut()
            .spawn(SpeechBubble {
                speaker,
                elapsed: 0.0,
                lifetime: 3.0,
            })
            .id();

        app.update();
        assert!(
            app.world().get_entity(bubble).is_ok(),
            "a live speaker keeps its bubble"
        );

        app.world_mut().entity_mut(speaker).despawn();
        app.update();
        assert!(
            app.world().get_entity(bubble).is_err(),
            "the bubble outlived the body that said it"
        );
    }

    #[test]
    fn a_finished_line_is_taken_off_the_body() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .add_systems(Update, expire_speech);
        let speaker = app
            .world_mut()
            .spawn((
                Speech {
                    text: "Hi.".to_string(),
                    tone: SpeechTone::Neutral,
                },
                SpeechTimer(Timer::from_seconds(0.5, TimerMode::Once)),
            ))
            .id();

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(600));
        app.update();

        assert!(
            app.world().get::<Speech>(speaker).is_none(),
            "the line should have been removed, which is what replicates its end"
        );
        assert!(app.world().get::<SpeechTimer>(speaker).is_none());
    }

    #[test]
    fn drawing_a_bubble_never_needs_anything_only_the_authority_has() {
        // This module's one real co-op trap, closed by construction rather
        // than guarded. `Chemist` and `RoomMemory` are both authority-only, so
        // a presentation system that reached for either would work perfectly
        // on the host and silently misbehave for whoever joined — which is
        // what every co-op bug in this project has looked like, none of them
        // with an error message.
        //
        // Asserted against the signatures of the four systems that run on both
        // ends, because the failure this catches is a *future* edit adding one
        // of those parameters, not a wrong answer today.
        let source = include_str!("mod.rs");
        for system in [
            "fn announce_speech(",
            "fn spawn_bubbles(",
            "fn place_bubbles(",
            "fn despawn_bubbles(",
        ] {
            let start = source.find(system).expect("system still exists");
            let signature = &source[start..start + source[start..].find(") {").unwrap()];
            for authority_only in ["RoomMemory", "Chemist", "SpeechTimer", "SpeechCooldown"] {
                assert!(
                    !signature.contains(authority_only),
                    "{system} reads {authority_only}, which a guest never has — \
                     the bubble would be drawn correctly on the host only"
                );
            }
        }
    }

    #[test]
    fn a_crew_member_is_placed_in_the_room_they_are_standing_in() {
        let mut app = App::new();
        app.insert_resource(WalkableAreas::from_floor_plan())
            .init_resource::<RoomMemory>()
            .add_systems(Update, remember_rooms);

        let mut inside = crate::lab::ROOMS[crate::lab::LOBBY].center();
        inside.y = crate::crew::BODY_OFFSET;
        let visitor = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".to_string(),
                    role: "Medical".to_string(),
                },
                Transform::from_translation(inside),
            ))
            .id();

        app.update();

        assert_eq!(
            app.world().resource::<RoomMemory>().room_of(visitor),
            Some("Lobby")
        );
    }

    // -----------------------------------------------------------------------
    // Conversation
    // -----------------------------------------------------------------------

    #[test]
    fn every_authored_line_is_only_spoken_by_someone_who_could_know_it() {
        // The integrity rule of the whole feature. A line that hands the
        // player a fact its speaker has no way of knowing is not a clue, it is
        // a readout with a portrait attached — and the hidden meters are only
        // worth having for as long as reaching them costs a walk to the right
        // person. `Knows::known_by` is the single source of truth; this is
        // what stops authored content drifting away from it.
        let script = script();
        let mut checked = 0;
        for (index, line) in script.gossip.iter().enumerate() {
            for what in &line.when {
                let Some(who) = what.known_by() else {
                    continue;
                };
                checked += 1;
                let role = line.role.as_deref().unwrap_or("<anyone>");
                assert!(
                    line.role.as_deref().is_some_and(|role| who.contains(&role)),
                    "gossip[{index}] is spoken by {role} but needs {what:?}, \
                     which only {who:?} could know: {:?}",
                    line.text
                );
            }
        }
        for (index, exchange) in script.exchanges.iter().enumerate() {
            for what in &exchange.when {
                let Some(who) = what.known_by() else {
                    continue;
                };
                checked += 1;
                let role = exchange.role.as_deref().unwrap_or("<anyone>");
                assert!(
                    exchange
                        .role
                        .as_deref()
                        .is_some_and(|role| who.contains(&role)),
                    "exchanges[{index}] is opened by {role} but needs {what:?}, \
                     which only {who:?} could know"
                );
            }
        }
        assert!(
            checked >= 10,
            "only {checked} restricted conditions are authored at all — this \
             test would pass on an empty pool, which is not the same as passing"
        );
    }

    #[test]
    fn every_department_leaks_the_thread_it_is_the_victim_of() {
        // The design in one assertion: each department gossips about its own
        // minor antagonist, so walking to the right person tells you about the
        // trouble they are actually placed to notice. A department with no
        // conditional line of its own is one the player has no reason to visit.
        let script = script();
        for role in DEPARTMENTS {
            assert!(
                script.gossip.iter().any(|line| {
                    line.role.as_deref() == Some(role)
                        && line.when.iter().any(|what| what.known_by().is_some())
                }),
                "{role} has nothing of its own to leak, so there is no reason \
                 to walk over and ask them anything"
            );
        }
    }

    /// An app that can be asked a question, with the real authored script.
    fn talking_app() -> (App, Entity) {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(threat::Authored(script()))
            .init_resource::<crate::orders::Shift>()
            .add_message::<FromClient<InteractRequested>>()
            .add_systems(Update, handle_talk);
        let player = app
            .world_mut()
            .spawn(Chemist {
                client: bevy_replicon::prelude::ClientId::Server,
            })
            .id();
        (app, player)
    }

    fn someone(app: &mut App, name: &str, role: &str) -> Entity {
        app.world_mut()
            .spawn(CrewMember {
                name: name.to_string(),
                role: role.to_string(),
            })
            .id()
    }

    /// Presses use on `target`, and returns what they said.
    fn ask(app: &mut App, target: Entity) -> String {
        app.world_mut().write_message(FromClient {
            client_id: bevy_replicon::prelude::ClientId::Server,
            message: InteractRequested { target },
        });
        app.update();
        let said = app
            .world()
            .get::<Speech>(target)
            .map(|speech| speech.text.clone())
            .unwrap_or_default();
        // Clear the line so the next press is not refused for interrupting it,
        // which is the real `MIN_BEFORE_NEXT_REMARK` behaviour and not what
        // these tests are about.
        app.world_mut()
            .entity_mut(target)
            .remove::<Speech>()
            .remove::<SpeechTimer>();
        said
    }

    #[test]
    fn talking_never_steals_a_press_that_would_have_been_a_delivery() {
        // Why this needs no ordering against `orders::handle_delivery` at all.
        // Every reader of `InteractRequested` that acts on a crew member —
        // delivery, the rogue officer's shakedown, a cult anchor, the siege
        // breach, an illicit offer — begins by finding a container held by the
        // sender and gives up when there is none. An empty hand is therefore
        // the one genuinely unclaimed press in the fan-out, and a full hand
        // must stay somebody else's.
        let (mut app, player) = talking_app();
        let member = someone(&mut app, "Dr. Vance", "Medical");
        app.world_mut().spawn((
            crate::containers::Container::new(crate::containers::ContainerKind::Beaker),
            crate::containers::HeldBy(player),
        ));

        app.world_mut().write_message(FromClient {
            client_id: bevy_replicon::prelude::ClientId::Server,
            message: InteractRequested { target: member },
        });
        app.update();

        assert!(
            app.world().get::<Speech>(member).is_none(),
            "a chemist holding a beaker got a conversation instead of a handover"
        );
    }

    #[test]
    fn nobody_is_ever_mute() {
        // With no meters, no standing and nothing happening — the state a
        // fresh save is actually in — every department still has something to
        // say. A silent NPC reads as broken, not as reticent.
        for role in DEPARTMENTS {
            let (mut app, _) = talking_app();
            let member = someone(&mut app, "Somebody", role);
            assert!(
                !ask(&mut app, member).is_empty(),
                "{role} had nothing at all to say on a fresh save"
            );
        }
    }

    #[test]
    fn asking_again_gets_you_something_new() {
        let (mut app, _) = talking_app();
        let member = someone(&mut app, "Chef Dubois", "Service");

        let first = ask(&mut app, member);
        let second = ask(&mut app, member);

        assert_ne!(
            first, second,
            "asking twice repeated the same line, so there is no reason to ask twice"
        );
    }

    #[test]
    fn someone_who_has_told_you_everything_says_so_and_keeps_saying_so() {
        // Running dry must not itself be recorded as something they said —
        // otherwise the fallback is used up too and the next press is silence.
        let (mut app, _) = talking_app();
        let member = someone(&mut app, "Miner Sato", "Cargo");
        let authored = script();
        let exhausted: Vec<&str> = authored
            .exhausted
            .iter()
            .map(|line| line.text.as_str())
            .collect();

        let mut last = String::new();
        for _ in 0..40 {
            last = ask(&mut app, member);
        }
        assert!(
            exhausted.contains(&last.as_str()),
            "after forty questions they said {last:?} rather than admitting they are out"
        );
        assert!(
            exhausted.contains(&ask(&mut app, member).as_str()),
            "running dry was recorded as a remark, so the fallback ran out too"
        );
    }

    #[test]
    fn a_meter_climbing_gives_the_department_that_watches_it_something_new() {
        // The feature, end to end: a hidden accumulator moves, and the person
        // who would notice has something to tell you that they did not before.
        let quiet = {
            let (mut app, _) = talking_app();
            let officer = someone(&mut app, "Officer Reyes", "Security");
            ask(&mut app, officer)
        };

        let (mut app, _) = talking_app();
        app.insert_resource(crate::antagonist::SecuritySuspicion::default());
        app.world_mut()
            .resource_mut::<crate::antagonist::SecuritySuspicion>()
            .restore(crate::antagonist::SUSPICION_MAX);
        let officer = someone(&mut app, "Officer Reyes", "Security");
        let alarmed = ask(&mut app, officer);

        assert_ne!(
            quiet, alarmed,
            "suspicion at the ceiling and Security still opens with small talk"
        );
        let authored = script();
        let expected: Vec<&str> = authored
            .gossip
            .iter()
            .filter(|line| line.when.contains(&Knows::SuspicionHigh))
            .map(|line| line.text.as_str())
            .collect();
        assert!(
            expected.contains(&alarmed.as_str()),
            "Security led with {alarmed:?} rather than the thing they are most \
             worried about"
        );
    }

    fn gossip(weight: i32, text: &str) -> GossipDef {
        GossipDef {
            role: None,
            when: Vec::new(),
            weight,
            tone: SpeechTone::Neutral,
            text: text.to_string(),
        }
    }

    #[test]
    fn the_heaviest_thing_they_have_to_say_comes_first() {
        // Deliberately authored so that weight order and list order DISAGREE.
        // The end-to-end `a_grudge_outranks_small_talk` below cannot prove
        // this on its own: the real script lists its conditional lines before
        // its small talk, so plain authored order gives the same answer and
        // the guard passes with the ordering removed. This is the version that
        // actually bites.
        let pool = [
            gossip(5, "small talk"),
            gossip(90, "a raid is coming"),
            gossip(40, "a grudge"),
        ];

        let (index, line) = best_unspoken(&pool, &[], |_| true).expect("something to say");
        assert_eq!(
            line.text, "a raid is coming",
            "the heaviest line did not win"
        );
        assert_eq!(index, 1);

        // Having said it, they move down the ladder rather than repeating.
        let (_, next) = best_unspoken(&pool, &[1], |_| true).expect("something else to say");
        assert_eq!(next.text, "a grudge");

        // Ties keep authored order, so a pool of equals reads as written.
        let equals = [gossip(5, "first"), gossip(5, "second")];
        assert_eq!(
            best_unspoken(&equals, &[], |_| true).unwrap().1.text,
            "first"
        );

        // Nothing eligible is `None`, not a panic and not a silent first entry.
        assert!(best_unspoken(&pool, &[], |_| false).is_none());
        assert!(best_unspoken(&pool, &[0, 1, 2], |_| true).is_none());
    }

    #[test]
    fn leaving_the_sign_down_gives_everyone_something_to_say_about_it() {
        // The two halves of the closure penalty agree because they read the
        // same replicated field: `shift::impatience` writes
        // `Shift::closure_pressure`, the HUD banner draws it, and this asks
        // about it. One answer to "has this gone on too long", not two that
        // could drift apart.
        let (mut app, _) = talking_app();
        app.world_mut()
            .resource_mut::<crate::orders::Shift>()
            .closure_pressure = 4;
        let member = someone(&mut app, "Miner Sato", "Cargo");

        let said = ask(&mut app, member);
        let authored = script();
        let about_the_sign: Vec<&str> = authored
            .gossip
            .iter()
            .filter(|line| line.when.contains(&Knows::LabHasBeenShut))
            .map(|line| line.text.as_str())
            .collect();
        assert!(
            about_the_sign.contains(&said.as_str()),
            "the lab had been shut for four minutes and they opened with {said:?}"
        );
    }

    #[test]
    fn reopening_stops_anyone_bringing_it_up() {
        let (mut app, _) = talking_app();
        let member = someone(&mut app, "Miner Sato", "Cargo");
        let authored = script();
        let about_the_sign: Vec<&str> = authored
            .gossip
            .iter()
            .filter(|line| line.when.contains(&Knows::LabHasBeenShut))
            .map(|line| line.text.as_str())
            .collect();

        // `closure_pressure` is zero: the lab is open, or was reopened.
        for _ in 0..40 {
            let said = ask(&mut app, member);
            assert!(
                !about_the_sign.contains(&said.as_str()),
                "an open lab was still being told off for being shut: {said:?}"
            );
        }
    }

    #[test]
    fn a_grudge_outranks_small_talk() {
        // Weighting is the whole priority ladder. Someone who has given up on
        // you should not open with a joke about the shuttle.
        let (mut app, _) = talking_app();
        app.world_mut()
            .resource_mut::<crate::orders::Shift>()
            .adjust_npc("Miner Sato", -40);
        let member = someone(&mut app, "Miner Sato", "Cargo");

        let said = ask(&mut app, member);
        let authored = script();
        let resentful: Vec<&str> = authored
            .gossip
            .iter()
            .filter(|line| line.when.contains(&Knows::ResentsYou))
            .map(|line| line.text.as_str())
            .collect();
        assert!(
            resentful.contains(&said.as_str()),
            "someone at the standing floor opened with {said:?}"
        );
    }

    #[test]
    fn a_line_written_for_one_department_is_never_said_by_another() {
        let (mut app, _) = talking_app();
        let member = someone(&mut app, "Tech Lindqvist", "Engineering");
        let authored = script();
        let others: Vec<&str> = authored
            .gossip
            .iter()
            .filter(|line| {
                line.role
                    .as_deref()
                    .is_some_and(|role| role != "Engineering")
            })
            .map(|line| line.text.as_str())
            .collect();

        for _ in 0..40 {
            let said = ask(&mut app, member);
            assert!(
                !others.contains(&said.as_str()),
                "Engineering said {said:?}, which was written for someone else"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Overheard exchanges
    // -----------------------------------------------------------------------

    #[test]
    fn every_exchange_is_a_conversation_and_not_a_monologue() {
        let script = script();
        assert!(script.exchanges.len() >= 8);
        for (index, exchange) in script.exchanges.iter().enumerate() {
            assert!(
                exchange.lines.len() >= 2,
                "exchanges[{index}] has nobody to answer it"
            );
            assert!(exchange
                .lines
                .iter()
                .all(|line| !line.text.trim().is_empty()));
            for line in &exchange.lines {
                assert!(
                    speech_seconds(&line.text) < DWELL_SECONDS.1,
                    "{:?} is too long to overhear in passing",
                    line.text
                );
            }
        }
    }

    /// An app that can run an exchange, with two residents standing together
    /// and a chemist close enough to overhear them.
    fn exchange_app() -> (App, Entity, Entity) {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(threat::Authored(script()))
            .insert_resource(crate::orders::Shift {
                accepting_orders: true,
                ..default()
            })
            .init_resource::<Chatter>()
            .init_resource::<PendingRemarks>()
            .add_systems(Update, start_exchanges);

        app.world_mut().spawn((
            Chemist {
                client: bevy_replicon::prelude::ClientId::Server,
            },
            Transform::from_xyz(0.0, crate::crew::BODY_OFFSET, 0.0),
        ));
        let first = standing_resident(&mut app, "Botanist Ivy", "Service", 2.0);
        let second = standing_resident(&mut app, "Chef Dubois", "Service", 3.0);
        (app, first, second)
    }

    fn standing_resident(app: &mut App, name: &str, role: &str, x: f32) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.to_string(),
                    role: role.to_string(),
                },
                Transform::from_xyz(x, crate::crew::BODY_OFFSET, 0.0),
                crate::crew::CrewRoute::standing(),
                Ambient::new(5.0),
            ))
            .id()
    }

    /// Runs the scheduler until it fires, or gives up.
    fn wait_for_chatter(app: &mut App) -> bool {
        for _ in 0..60 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(10.0));
            app.update();
            if app.world_mut().query::<&Speech>().iter(app.world()).count() > 0 {
                return true;
            }
        }
        false
    }

    #[test]
    fn two_residents_standing_together_are_overheard() {
        let (mut app, _, _) = exchange_app();
        assert!(
            wait_for_chatter(&mut app),
            "two residents stood next to a chemist for ten minutes and said nothing"
        );
    }

    #[test]
    fn the_station_keeps_talking_while_the_sign_is_down() {
        // The one place this deliberately parts company with
        // `radio::tick_ambient_radio`, which does stop while the sign is down.
        // Closing the counter means chemistry is shut, not that the station
        // has emptied — and it is precisely when the player has time to walk
        // the halls and listen. Silencing it then would turn the feature off
        // at the only moment there is room to use it.
        let (mut app, _, _) = exchange_app();
        app.world_mut()
            .resource_mut::<crate::orders::Shift>()
            .accepting_orders = false;

        assert!(
            wait_for_chatter(&mut app),
            "the station went silent the moment the counter closed"
        );
    }

    #[test]
    fn nobody_gossips_while_someone_is_bleeding_on_the_counter() {
        // Suppressed by exactly what suppresses `radio::tick_ambient_radio`.
        // Two people chatting about the shuttle over a casualty is the game
        // not noticing what it is doing.
        let (mut app, _, _) = exchange_app();
        app.world_mut().spawn(crate::orders::CrisisOrder);

        assert!(
            !wait_for_chatter(&mut app),
            "the station gossiped through a crisis"
        );
    }

    #[test]
    fn a_reply_is_dropped_when_the_person_who_owed_it_has_gone() {
        // Residents walk off and visitors despawn between the two halves of a
        // two-hander. A reply from a body that no longer exists would be a
        // bubble with nobody under it.
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .init_resource::<PendingRemarks>()
            .add_systems(Update, deliver_remarks);

        let gone = app.world_mut().spawn_empty().id();
        let here = app
            .world_mut()
            .spawn(CrewMember {
                name: "Chef Dubois".to_string(),
                role: "Service".to_string(),
            })
            .id();
        for who in [gone, here] {
            app.world_mut().resource_mut::<PendingRemarks>().0.push((
                Timer::from_seconds(0.1, TimerMode::Once),
                who,
                "I'd noticed.".to_string(),
                SpeechTone::Neutral,
            ));
        }

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.2));
        app.update();

        assert!(
            app.world().get::<Speech>(here).is_some(),
            "the person still standing there never gave their reply"
        );
        assert!(
            app.world().get::<Speech>(gone).is_none(),
            "a body with no `CrewMember` was made to answer"
        );
    }

    #[test]
    fn a_body_in_a_doorway_keeps_the_room_it_came_from() {
        // `room_at` returns `None` on a threshold by design. Treating that as
        // "nowhere" would blink a bubble off every time either party walked
        // through a door — the exact trap `docs/npc-ai.md` warns about.
        let mut memory = RoomMemory::default();
        let body = Entity::from_raw_u32(1).unwrap();
        memory.0.insert(body, "Mixing Hall".to_string());
        assert_eq!(memory.room_of(body), Some("Mixing Hall"));
    }
}
