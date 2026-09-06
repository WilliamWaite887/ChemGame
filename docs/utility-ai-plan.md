# ChemGame Utility AI Master Plan

Status: Living implementation plan

Last updated: 2026-09-04

Current phase: P3 five of seven departments complete (Cargo, Medical, Botany,
Engineering, Service). Integration wave closed green; Security, Bridge, and the
P5 opportunity buffer are the next packets.

Primary reference: [An introduction to Utility AI](https://shaggydev.com/2023/04/19/utility-ai/)

## Purpose

This document is the source of truth for replacing ChemGame's ambient NPC
wander logic and disconnected scripted behaviors with one station-wide utility
AI. It is written so several agents can implement separate work packets without
creating competing concepts of an NPC, a job, an emergency, or an action.

The target is not an NPC that merely looks occupied. Every resident should
choose useful work, personal, social, emergency, and antagonist actions from
the actual station state. Those actions must change the world in ways that
other NPCs and the player can observe and respond to.

The reference scenario is deliberately systemic:

1. Miner Sato chooses a Cargo task and works a freight station.
2. A bounded workplace risk produces a real burn on Sato's existing `Body`.
3. Medical notices or is told about the casualty.
4. A free doctor may collect Sato, reserve a bed, transport him to Medical,
   and place him in that bed.
5. If Medical cannot resolve the case directly, another Cargo worker or a
   Medical worker can create a treatment request and walk to Chemistry.
6. The delivered medicine is applied to the linked patient, rather than
   resolving an abstract order whose casualty never existed.
7. Sato recovers, leaves the bed, remembers the event, and later returns to
   useful work.

The Service reference scenario is equally systemic:

1. Service prepares and exposes food that NPCs can choose to eat together.
2. A covert antagonist evaluates whether contaminating that food advances a
   private goal and whether the opportunity is safe enough.
3. The antagonist must travel to the food and perform the action.
4. Consumers ingest the food's real chemical contents.
5. Symptoms, witnesses, Medical response, Security suspicion, relationships,
   and later decisions change because of what actually happened.

These are acceptance scenarios for shared architecture, not bespoke scripts.

The player can also intervene before a department asks for anything:

1. Real job backlog, unavailable workers, injuries, faults, or missing supplies
   make a department visibly fall behind.
2. The player notices qualitative room, speech, board, or work-state cues.
3. The player physically carries a chemical or drug to that department and
   submits it as voluntary aid rather than fulfilling an order.
4. Department NPCs assess, collect, store, administer, use, reject, or report
   the batch through utility actions.
5. The actual solution, dose, purity, label, and method determine whether work
   recovers or the intervention causes impairment, addiction, injury, poisoning,
   suspicion, damaged trust, or another incident.

## Scope

### Included

- One utility decision framework for every resident NPC.
- Real job loops for Medical, Security, Engineering, Cargo, Service, Botany,
  and Bridge.
- Botany and Bridge as first-class job and relationship departments.
- Exactly two unique core characters in each department who directly talk to
  and build relationships with the player.
- A larger, authored support crew in every department. Supporting NPCs use the
  same bodies, needs, utility decisions, jobs, incidents, and social spaces as
  the named cast rather than existing as decoration.
- Chemistry-facing requests created by real NPC needs and station incidents.
- Linked deliveries retain their requester, carrier, beneficiary, source, and
  use destination. Accepting a container never implies self-application.
- Proactive player aid for a lagging department, with actual chemical benefits
  and consequences rather than an abstract productivity bonus.
- Personal needs, work priorities, social behavior, emergencies, and covert
  antagonist behavior in the same scoring framework.
- Physical action execution through navigation, reservations, work duration,
  outcomes, and interruption.
- Real injuries, chemistry, food contamination, treatment, recovery, and
  consequences.
- Perception and memory sufficient to stop NPCs from acting on information they
  could not know.
- Authority-safe multiplayer state and deterministic headless tests.
- Debug traces and tuning tools that do not expose hidden numbers to players.

### Not included

- NPCs autonomously taking over the player's Chemistry machines.
- A second health, chemistry, navigation, relationship, order, or antagonist
  simulation beside the existing systems.
- Perfect life simulation, unrestricted free-form dialogue, or an economic
  simulation unrelated to player-facing station play.
- Persisting frame-level action state, path waypoints, or reservations across a
  save. Those are reconstructed from persistent world facts.
- Giving clients replicated secret traits, knowledge, allegiance, target
  scores, or antagonist intent.
- A generic "good chemical" token that raises a department meter without the
  batch entering a real body, process, stock, or job.

## Existing contracts that must survive

The implementation begins from the current source, not from a blank design.

- `CrewMember` remains NPC identity.
- `Body` and `Bloodstream` remain the only physical health and chemistry truth.
- `CrewRoute`, `Errand`, `NavGraph`, `Trail`, and `WalkableAreas` remain the
  movement foundation during migration.
- `ErrandResolved` remains the proven arrival seam for an actor moving to a
  point or entity.
- `Shift::npc_standing`, department standing, Social history, and existing
  speech remain the relationship and communication foundation.
- The ordinary order pipeline remains the player-facing way to ask Chemistry
  for medicine. AI-created requests add a linked case source rather than a
  parallel request UI.
- `Campaign`, `AntagId`, existing antagonist modules, and plot/instability
  mutators remain campaign truth.
- The authority owns decisions and simulation. Clients receive only public
  state and construct presentation locally.
- A station resident must never be replaced by a duplicate visitor with the
  same name.
- Navigation containment, leaving routes, stall recovery, and safe off-floor
  exit behavior must remain intact.

`CrewPhase::Waiting` must not gain another meaning. The current code already
uses it as a proxy for several different kinds of arrival. Utility execution
gets an explicit lifecycle instead.

## Replacement and controller ownership

This is a replacement refactor, not a second AI layered over the first one.
During staged migration, old and new decision code may exist in the repository,
but they must be entity-disjoint and must never issue competing intent or
movement for the same NPC.

### Single-controller invariant

Every NPC has exactly one authority-side control owner at a time:

```text
LegacyAmbient
UtilityAction
OrderVisit
ScriptedErrand
Pursuit
MedicalTransport
Incapacitated
```

The final names may change, but the ownership rule may not. Control transitions
are explicit, centralized, and atomic. Starting a new controller either performs
a legal handoff from the current owner or fails without changing the NPC.

Examples:

- A utility worker recalled for an order releases its action reservations,
  completes cleanup, and hands control to `OrderVisit` before routing begins.
- A pursuit interrupts an interruptible utility action, removes its travel
  state, and claims control before the pursuer moves.
- Incapacitation immediately suspends or cancels the current action and prevents
  every movement controller from advancing the body.
- Medical transport gives locomotion to the transporter while the patient is
  attached as transported state. The patient's route and utility executor do
  not also move the patient.
- Finishing an existing scripted errand returns control through one adapter,
  which restores utility selection or the correct visitor flow.

### Staged migration rule

In the Cargo pilot:

- Cargo pilot workers have `UtilityAgent` and are queried only by utility
  selection.
- The existing `ambient_behaviour` query must explicitly exclude
  `UtilityAgent`.
- Utility decision queries must explicitly require `UtilityAgent`.
- Supporting identity markers may still be shared for order filtering,
  rendering, health, and speech. Shared identity is not shared control.
- Activation inserts the utility marker before either behavior schedule can
  run, with an explicit command flush where Bevy deferred commands require it.
- Deactivation completes cleanup and restores exactly one legacy controller.

Before the wider department migration, split resident identity from legacy
ambient decision state. `Ambient` currently does both jobs because
`NotResident = Without<Ambient>` while `ambient_behaviour` also reads its dwell
timer. Move residency filtering to the existing `StationResident` concept or a
renamed public equivalent, move the legacy dwell value into a legacy-only
component, then remove that component as the last department migrates.

### Locomotion ownership

- `CrewRoute`, `Errand`, pursuit state, and transport-follow state are mutually
  exclusive locomotion modes on one NPC.
- All insertion and removal goes through control-handoff helpers.
- An action selection system never writes `Transform`.
- One ordered navigation stage contains every allowed transform writer.
- Use an explicit deferred-command flush between control handoff and navigation
  so a route removed this frame cannot also move before an inserted errand.
- Presentation rotation must run after locomotion and may only turn stationary
  actors.
- Debug builds assert that no NPC has incompatible locomotion components.

### Required anti-conflict tests

- A utility agent is never selected by `ambient_behaviour`.
- A legacy ambient resident is never selected by utility scoring.
- Activating utility control before an update produces no legacy destination.
- Starting utility travel leaves no `CrewRoute` on the actor before navigation.
- Recalling a utility resident for an order cancels the action once, releases
  reservations once, and produces only the order route.
- A pursuit or scripted errand cannot coexist with utility travel.
- A transported patient has no independently advancing route or action.
- An incapacitated body never changes position through any NPC controller.
- Each update has at most one system authorized to mutate an NPC's translation.
- After the final migration, no production system registers legacy ambient
  decision-making.

## Design principles

1. Utility chooses what; an action state machine performs how.
2. Score facts, not stories. A Cargo burn, unavailable doctor, free bed, hungry
   diner, poisoned meal, and witnessed act are world facts shared by systems.
3. Use normalized inputs in the range 0 to 1.
4. A hard precondition scores zero. No executor should begin an impossible
   action and hope to repair it later.
5. Prefer multiplicative considerations so one zero vetoes an action.
6. Use buckets for true priority classes, not to disguise poor tuning.
7. Add bounded variety only among near-best candidates. Critical needs choose
   more deterministically than leisure.
8. Actions commit long enough to read as intentional, but emergencies may
   interrupt interruptible work.
9. Every meaningful object and destination is reservable.
10. Consequences must be visible through bodies, objects, speech, movement,
    relationships, orders, or station state.
11. NPC knowledge comes from assignments, direct perception, reports, or
    memory. Avoid omniscient global queries in scoring.
12. Secret state stays on the authority and never leaks through component
    presence or obvious action labels.
13. Data files tune weights, curves, timings, and content. Rust owns invariants,
    access control, dispatch, and side effects.
14. The system must remain understandable under a debugger. Every selection
    can explain its winning score and rejected alternatives.
15. Player-facing behaviors outrank simulation detail that nobody can observe.
16. One NPC has one decision owner and one locomotion owner at a time.

## Terminology

- Agent: A resident `CrewMember` with `UtilityAgent`.
- Need: Slowly changing personal pressure such as hunger or social need.
- Fact: A normalized piece of current context available to an agent.
- Consideration: A curve that turns one fact into a score from 0 to 1.
- Candidate: An action kind plus concrete targets and consideration scores.
- Bucket: A true priority class selected before comparing actions inside it.
- Intent: The selected action and its target set.
- Executor: The state machine that travels, performs, and resolves an intent.
- Job ticket: A real unit of station work that can be claimed and completed.
- Incident: A consequential event that may create casualties, hazards, reports,
  job tickets, memories, or Chemistry requests.
- Stimulus: Something an agent could see, hear, or be told.
- Memory fact: A stimulus the agent retained with confidence and age.
- Public activity: The minimal replicated description needed for animation,
  prompts, and visible feedback.
- Private motive: Authority-only allegiance, goal, knowledge, or score input.

## Department coverage

The completed relationship and utility models both have seven departments:
Medical, Security, Engineering, Cargo, Service, Botany, and Bridge. Each has a
complete job loop and exactly two core player-facing characters. Chemistry
remains the player profession and a station dependency.

The existing `Department` relationship enum expands with Botany and Bridge.
`CrewMember.role` continues to drive appearance and authored identity, while
`NpcJobProfile` supplies cross-training and specific capabilities. Botany is no
longer hidden inside Service, and Bridge is no longer a support-only room.

### Medical

Routine jobs:

- Inspect beds and monitors.
- Restock treatment points.
- Perform rounds on admitted patients.
- Triage reported or visible injuries.
- Escort a mobile patient to Medical.
- Transport an incapacitated patient.
- Reserve and prepare a bed.
- Stabilize, monitor, discharge, or request missing treatment.

Consequential outcomes:

- A linked treatment request reaches Chemistry when Medical lacks an effective
  medicine.
- A patient occupies a real bed until discharge, transfer, recovery, or death.
- Delayed care worsens urgency and changes later utility scores.
- Repeated preventable workplace cases affect individual and department
  relationships.

### Security

Planned support cast:

- Patrol Officer Dlamini handles patrol, scene security, and physical response.
- Dispatcher Novak handles reports, watch coverage, and response routing.
- They support existing core characters Officer Reyes and Warden Bex without
  joining the core standing average or random Chemistry customer roster.

Routine jobs:

- Patrol authored routes and high-value rooms.
- Staff dispatch and checkpoints.
- Inspect reported hazards, tampering, theft, or assault.
- Interview witnesses and remember reports.
- Guard a casualty, dangerous object, or secured scene.
- Escort a hostile or incapacitated suspect when an appropriate system exists.
- Check suspicious food or containers without magically knowing their contents.

Consequential outcomes:

- Witnessed covert actions create evidence and suspicion, not instant perfect
  identification.
- Unanswered incidents become security job tickets and later relationship or
  campaign pressure.
- Existing sweep, raid, bent-guard, and rogue-security behaviors migrate onto
  shared actions without changing their authored outcomes.

### Engineering

Planned cast for the first Engineering adapter:

- Tech Lindqvist remains the hands-on diagnostic core character.
- Chief Engineer Morrow is the second core character, responsible for triage,
  risk policy, and player-facing department priorities.
- Mechanic Torres and Systems Tech Adeyemi are support residents with distinct
  repair and systems-check qualifications.

Routine jobs:

- Inspect utility panels and station machinery.
- Perform maintenance at duty stations.
- Respond to machine faults, leaks, fires, and damaged infrastructure.
- Isolate unsafe equipment.
- Repair or reset a fault after obtaining required supplies.
- Assist with physically dangerous station work.

Consequential outcomes:

- Work can create bounded burns, brute injuries, spills, smoke, or equipment
  downtime when risk conditions are present.
- Ignored faults raise instability or create follow-up incidents.
- Completed repairs clear real fault state and reduce downstream risk.

### Cargo

Routine jobs:

- Read the manifest board.
- Dispatch, weigh, sort, receive, shelve, and move freight.
- Operate the requisitions desk.
- Deliver restock items to other departments.
- Clear blocked staging areas.
- Help an injured coworker or report the case.

Consequential outcomes:

- Freight work updates actual task and stock state.
- Hazards can cause real burns or brute injuries with visible lead-up and
  authored safeguards.
- A coworker can become the requester for a linked casualty when Medical is
  busy or the patient is still mobile.

Implemented pilot cast:

- Miner Sato is Cargo's hands-on core worker. His existing standing, dialogue,
  requisitions, and player history remain intact while utility AI adds physical
  freight work.
- Quartermaster Rhee is Cargo's second core character. She is precise,
  procedure-minded, and focused on manifests, requisitions, and whether an aid
  proposal can be accounted for. Her unique dialogue and favors remain a P3
  content pass, but her stable core identity and Social relationship entry are
  live.
- Loader Bell and Clerk Nwosu are stable support workers. They share the full
  physical job, health, navigation, and consequence simulation while staying
  outside the random Chemistry customer and core relationship rosters.

### Service

Service contains the kitchen, bar, lounge, and shared dining area.

Planned core cast:

- Chef Dubois owns kitchen craft, ingredient quality, meal safety, and the
  consequences of what leaves the pass.
- Steward Amari owns front-of-house hosting, room appeal, drinks, mediation,
  and noticing social trouble. Amari is the stable second Service core identity
  and inherits Dubois's department standing on legacy saves.

Routine jobs:

- Prepare a meal from real ingredients.
- Plate, expose, serve, clear, and discard food.
- Stock the bar and kitchen.
- Clean spills and shared tables.
- Host, eat, drink, sit, socialize, comfort, gossip, argue, or leave.

Consequential outcomes:

- Food has servings, provenance, and a chemical solution that consumers ingest.
- A contaminated batch affects every consumer of that batch according to dose.
- Food quality, safety, crowding, mood, and relationships change whether the
  Service room attracts or repels residents.
- Service becomes the preferred social hub because it offers useful affordances,
  not because all NPCs receive a hardcoded instruction to stand there.

### Botany

Botany is a first-class job and relationship department.

Routine jobs:

- Inspect plots and plant health.
- Sow, water, tend, and harvest authored crops.
- Process safe and hazardous produce.
- Deliver ingredients to Service.
- Deliver chemical feedstock or specimens to the appropriate department.
- Clean or quarantine a contaminated plot.

Consequential outcomes:

- Harvests become real ingredients, meal inputs, or chemical sources.
- Neglected plots lose yield, become unsafe, or create a cleanup ticket.
- Toxic produce carries real chemical contents and provenance.
- A suspicious substitution or contaminated harvest can propagate into Service
  food, Medical cases, Security work, and antagonist opportunity.
- Botany outcomes affect its two core characters and Botany standing.

### Bridge

Planned cast migration:

- Helmsman Odera becomes the navigation and station-readiness core character.
- Yeoman Sissel becomes the communications, reporting, and briefing core
  character.
- Ensign Park, Ensign Alvarez, Operator Fenn, and Operator Ruiz remain full
  support residents and cover the remaining watches and consoles.

Routine jobs:

- Staff command, navigation, communications, sensor, and watch consoles.
- Review department condition and dispatch cross-department work.
- Conduct shift briefings and receive incident reports.
- Coordinate emergency priorities without directly performing every response.
- Maintain a continuous watch while allowing breaks and social needs.

Consequential outcomes:

- Ignored reports, missing watch coverage, or impaired officers delay station
  awareness and response.
- Good coordination can raise the urgency or visibility of a real job ticket
  without completing it by fiat.
- The two core Bridge characters give the player a direct command relationship
  and qualitative station overview.
- Four additional Bridge support workers keep consoles and watches staffed.

### Chemistry boundary

- Chemistry publishes requests and receives deliveries, incidents, patients,
  and reports. NPCs do not use player machines autonomously unless a later
  design explicitly adds AI chemists.

## Population model

The current station has eight core residents and six hardcoded Bridge support
residents. The utility rollout expands this into an authored two-tier cast of
30 stable residents.

Core residents:

- Exactly two belong to every department.
- Keep unique names, personalities, relationship standing, authored dialogue,
  job perspectives, favors, order eligibility, and campaign relevance.
- Directly talk to the player about their department, current work, remembered
  incidents, voluntary aid, coworkers, and personal relationship.
- Use the same utility kernel as everyone else.

Support residents:

- Have stable authored names, role, job domain, capabilities, personality
  defaults, and home assignment.
- Have real `Body`, `Bloodstream`, needs, memory, speech, jobs, social actions,
  injuries, treatment, and antagonist consequences.
- Affect department standing and station outcomes, but do not automatically
  gain core personal-standing arcs or ordinary random order eligibility.
- May become the exact requester for a linked incident involving themselves or
  a coworker.
- Are never spawned as anonymous visual-only copies.

Initial population target:

| Department | Core residents | Support residents | Initial total |
|---|---:|---:|---:|
| Medical | Dr. Vance, Nurse Okonkwo | Paramedic Hale, Orderly Imani | 4 |
| Security | Officer Reyes, Warden Bex | Patrol Officer Dlamini, Dispatcher Novak | 4 |
| Engineering | Tech Lindqvist, Chief Engineer Morrow | Mechanic Torres, Systems Tech Adeyemi | 4 |
| Cargo | Miner Sato, Quartermaster Rhee | Loader Bell, Clerk Nwosu | 4 |
| Service | Chef Dubois, Steward Amari | Cook Navarro, Attendant Mensah | 4 |
| Botany | Botanist Ivy, Agronomist Vale | Grower Chen, Technician Mbatha | 4 |
| Bridge | Helmsman Odera, Yeoman Sissel | Ensign Park, Ensign Alvarez, Operator Fenn, Operator Ruiz | 6 |
| Total | 14 | 16 | 30 |

Security is not in the newly listed room set because it already satisfies the
two-character rule. It remains a full department with its existing two core
characters and supporting crew.

The two core characters in a department must be complementary, not
interchangeable portraits. Each pair needs:

- Distinct job responsibilities and capability emphasis.
- Distinct personality weights that produce different utility choices.
- Separate personal standing and remembered player history.
- Department-specific conversation about current backlog, incidents, supplies,
  and voluntary chemical aid.
- At least one unique favor, concern, or perspective that does not duplicate
  the partner's role.
- Dialogue for neutral, trusted, strained, injured, chemically affected, and
  emergency states.
- The ability to perform normal work, socialize, suffer consequences, seek
  treatment, and respond to the other core character.

Helmsman Odera and Yeoman Sissel are promoted from the current six Bridge
residents and deeply authored rather than replaced. The other four remain
support workers. Their role split keeps physical navigation readiness distinct
from communications, reporting, and briefings.

The exact support names belong in data and must be reviewed as a cast. Runtime
random names are prohibited because save identity, witness memory, speech,
debug traces, and deterministic tests require stable identity.

Replace the hardcoded Bridge-only `crew::fluff::FLUFF_CREW` with validated core
and support roster data. Keep random Chemistry customer eligibility explicit,
since `station.crew.ron` currently doubles as that roster. Validation must
reject duplicate names, a department without exactly two core characters,
unknown home departments, missing job profiles, and a support NPC accidentally
becoming a random order customer.

The Cargo micro-pilot begins with Miner Sato, Cargo's new second core character,
and two authored Cargo support workers. This is important test coverage, not
just added density: four workers can compete for jobs, reserve different
stations, notice a coworker's injury, choose between helping and continuing
work, discuss department condition with the player, and create a real treatment
request.

### Direct player interaction contract

Every core character supports the same minimum direct interaction vocabulary:

- Talk with empty hands through the existing Social and speech interaction
  seam.
- Ask how their department is doing and receive qualitative, knowledge-limited
  information.
- Discuss a current job, casualty, fault, shortage, suspicious event, or recent
  outcome they plausibly know about.
- Offer voluntary chemical aid through an explicit interaction that cannot also
  deliver an order or inject the NPC.
- React later to whether that aid helped, failed, was unsafe, or was deceptive.
- Build separate personal standing while contributing to department standing.
- Ask for or grant authored favors where their personal role permits it.
- Remember meaningful interactions in the shared co-op transcript and
  authority-owned NPC memory.

Supporting characters can still speak, report, accept assignments, and react
socially, but they use smaller shared content pools and do not replace either
of the department's two core relationship anchors.

### Relationship and save migration

Adding Botany and Bridge to the relationship model requires a deliberate save
migration:

- Extend `Department::ALL`, labels, descriptions, member lists, Social directory
  sections, standing views, snapshots, sync messages, and exhaustive matches.
- Move Botanist Ivy's membership from Service to Botany while preserving her
  existing personal standing.
- Seed Agronomist Vale from Ivy's standing on legacy saves so
  the new department average does not silently dilute the existing relationship.
- Keep Chef Dubois in Service and seed Steward Amari from his standing on
  legacy saves for the same reason.
- Add the two promoted Bridge core characters at neutral standing on legacy
  saves because no prior Bridge relationship exists.
- Keep support workers outside the personal-standing average. Their outcomes
  adjust the department through the two core members, preserving the existing
  rule that department standing is the average of its authored member pair.
- Preserve the complete shared Social transcript and never partition it by the
  new department sections.

## Architecture

### Proposed module layout

```text
src/utility_ai/
  mod.rs                 plugin, schedules, public types
  curves.rs              normalized response curves
  context.rs             clearing-house facts and candidate queries
  decision.rs            buckets, scoring, hysteresis, selection
  action.rs              intent lifecycle and interruption
  reservation.rs         actor, object, workstation, bed, and social slots
  needs.rs               hunger, fatigue, social, morale, duty pressure
  status.rs              public department and crew status projection
  aid.rs                 voluntary player aid custody, assessment, and use
  perception.rs          stimuli, sensing, reports, memory
  incidents.rs           incident creation and consequence routing
  jobs/
    mod.rs               job tickets and shared job execution
    medical.rs
    security.rs
    engineering.rs
    cargo.rs
    service.rs
    botany.rs
  population.rs          core/support roster and job-profile assignment
  social.rs              pair and group interactions
  antagonist.rs          private goals and covert candidates
  debug.rs               traces, overlays, and deterministic diagnostics
```

Authoring surfaces:

```text
assets/data/station.utility_ai.ron
assets/data/station.jobs.ron
assets/data/station.social_actions.ron
assets/data/station.personalities.ron
assets/data/station.support_crew.ron
assets/data/station.department_aid.ron
```

The implementation may split these files further when their schemas stabilize.
Stable IDs must be strings or enums, never indices into authored arrays.

### Scheduling

Introduce explicit system sets:

```text
Observe
  -> MaintainNeeds
  -> BuildContext
  -> Score
  -> Select
  -> BeginAction
  -> Navigate
  -> Perform
  -> Resolve
  -> Publish
```

Requirements:

- Decision evaluation is authority-only.
- Needs, hazards, metabolism, and incident facts update before scoring.
- Selection never writes transforms.
- Navigation is the only utility-AI stage that moves an actor.
- Resolution is the only stage that applies job completion side effects.
- Speech, animation, and client presentation consume public activity after
  resolution or state changes.
- Agents evaluate on staggered intervals, initially 0.35 to 0.75 seconds, with
  immediate dirty wakes for emergency stimuli and invalidated targets.

### Core components and resources

Names are provisional until P1 compiles, but their responsibilities are fixed.

`UtilityAgent`

- Marks a resident as participating in utility decisions.
- Holds a deterministic decision phase offset and personality profile ID.
- Authority-only unless a public field is proven necessary.

`NpcJobProfile`

- Stores a primary `JobDomain` and capability set independently from
  `CrewMember.role`.
- Initial domains are Medical, Security, Engineering, Cargo, Service, Botany,
  and Bridge duty.
- Maps Botanist Ivy to Botany while the relationship migration preserves her
  personal standing, dialogue, and save compatibility.
- Allows later Command crew or cross-training without changing utility kernel
  types.

`NarrativeTier`

- Distinguishes Core and Support content eligibility without changing physical
  simulation or utility quality.
- Core gates unique relationship arcs, bespoke dialogue, campaign roles, and
  ordinary random order selection.
- Support NPCs still work, socialize, remember, suffer consequences, request
  incident treatment, and affect their department.
- Must never be used as a reason to skip health, navigation, or emergency
  behavior for a visible NPC.

`NpcNeeds`

- Hunger, fatigue, social need, morale, and duty pressure, each normalized.
- Does not duplicate damage, toxin, sedation, or other `Body`/`Bloodstream`
  facts.
- Long-term values may persist; short action cooldowns do not.

`NpcTraits`

- Duty, empathy, sociability, neatness, courage, caution, aggression, and
  appetite.
- Authored by stable crew name with role defaults.
- Clamped to documented ranges and hidden from clients.

`NpcMemory`

- Facts learned from direct perception, reports, assignments, and outcomes.
- Each entry has subject, kind, location, confidence, learned time, and expiry.
- Memory can be mistaken or stale only through an explicit authored rule.

`CurrentAction`

- Authority-only selected intent.
- Stores action ID, bucket, targets, lifecycle phase, elapsed time, minimum
  commitment, timeout, and interruption policy.
- Never stores route waypoints or private data needed only by the target system.

`NpcActivity`

- Replicated public presentation state such as Traveling, Working, Helping,
  Eating, Socializing, Resting, Treating, or Down.
- Must not distinguish innocent food handling from covert contamination.
- Must not contain scores, allegiance, exact knowledge, or hidden target IDs
  that a client could inspect to reveal the antagonist.

`ReservationBook`

- Authority resource keyed by stable target slot.
- Supports exclusive, capacity-limited, and pair reservations.
- Releases on completion, interruption, entity removal, timeout, disconnect,
  shift transition, and target invalidation.

`JobBoard`

- Authority resource or ECS ticket set containing available station work.
- Tickets include stable ID, domain, location/target, urgency, required
  capability, created time, deadline, risk, and completion state.
- A ticket is the fact that work exists. It is not the actor's action state.

`UtilityOpportunityBuffer`

- Is a per-frame authority-only candidate-provider seam for personal, social,
  reporting, and covert actions that are not department jobs.
- Providers publish an exact agent, stable action and target key, normalized
  utility inputs, execution target, reservation contract, timing, and
  interruption policy during `BuildContext`.
- The buffer clears before each context build and is consumed by the same
  selector that considers `JobBoard`; it is not a persistent request queue and
  never moves an actor or applies an outcome.
- Target-specific appeal, safety, affinity, and opportunity may affect a
  candidate's normalized base value, while shared `NpcNeeds` facts supply
  hunger, fatigue, and social pressure. Zero feasibility vetoes the candidate.
- Selection copies the execution contract into `CurrentAction`; the owning
  adapter observes the ordinary `UtilityActionResolved` result and commits the
  consequence. Reservations and interruption cleanup remain centralized.
- Eating, resting, socializing, reporting, and covert actions must use this
  seam rather than masquerading as `PerformJob` or creating another decision
  controller.

`DepartmentWorkState`

- Authority-side derived workload, staffing, shortage, fault, incident, and
  throughput facts for each department.
- Feeds job utility, voluntary-aid assessment, Bridge coordination, and the
  public Crew menu projection.
- Exact normalized pressure and hidden causes do not replicate.

`PublicDepartmentStatus` and `PublicCrewStatus`

- Safe qualitative projections for the Crew menu and other presentation.
- Contain only facts the crew has made public, the team observed, or the world
  visibly demonstrates.
- Never contain utility scores, private need values, secret goals, unreported
  witness memories, exact unknown chemicals, or antagonist identity.
- Update on meaningful changes or a bounded low-frequency cadence, not every
  frame.

`IncidentLedger`

- Records active consequential situations such as injury, fire, tampering,
  poisoning, theft, contamination, or equipment failure.
- Publishes stimuli and creates job tickets without directly choosing who acts.

`UtilityTuning`

- Loaded from RON.
- Contains decision intervals, curves, weights, bucket policy, hysteresis,
  cooldowns, and risk bounds.
- Validation rejects missing IDs, non-finite numbers, invalid ranges, and
  references to unknown actions or facts.

### Action identity

Use a stable Rust enum or validated string-backed newtype for dispatch. Initial
action families:

```text
Survive
SeekMedicalHelp
RespondToCasualty
TransportPatient
UseMedicalBed
TreatPatient
RequestTreatment
ReturnTreatmentToCase
AssessDepartmentAid
CollectDepartmentAid
AdministerDepartmentAid
UseProcessAid
RejectDepartmentAid
ReportSuspiciousAid
PerformJob
Investigate
Repair
Patrol
HandleFreight
PrepareFood
ServeFood
EatFood
Clean
Socialize
Comfort
ReportIncident
Rest
IdleObserve
Sabotage
PoisonFood
ConcealEvidence
```

Concrete department jobs remain ticket data beneath `PerformJob` where their
execution is genuinely shared. Use a specialized action only when it has a
distinct executor, target contract, interruption policy, or consequence.

## Utility scoring model

### Context clearing house

`context.rs` presents normalized facts through typed accessors. It may query
several ECS sources internally, but scorers do not reach into unrelated
resources themselves.

Initial fact families:

- Self: health, pain, toxin load, mobility, hunger, fatigue, social need,
  morale, current danger, current commitment.
- Work: duty pressure, ticket urgency, qualification, distance, target
  availability, expected value, risk, deadline.
- Medical: casualty severity, mobility, time untreated, responder availability,
  bed availability, treatment availability.
- Social: affinity, familiarity, target availability, privacy, crowd quality,
  room appeal, recent repetition.
- Safety: perceived hazard, witnessed aggression, escape access, security
  presence, exposure likelihood.
- Covert: private goal value, opportunity, witness risk, evidence risk,
  plausible access, expected campaign effect.

All distance inputs use path distance when available. Straight-line distance
must not make a target across a wall look adjacent.

### Buckets

Buckets are evaluated in priority order only when their entry condition is met:

1. Terminal: incapacitated, transported, restrained, or otherwise unable to
   choose. The executor or body state owns behavior.
2. Immediate survival: flee active danger, seek urgent aid, or perform a
   lifesaving self action.
3. Emergency response: casualty, fire, violence, or critical station fault for
   a qualified responder.
4. Obligated work: claimed or urgent departmental job.
5. Goal-directed covert work: only for an agent with a private active goal.
6. Routine work and errands.
7. Personal and social needs.
8. Low-cost ambient actions and observation.

A bucket is eligible only if at least one candidate has a non-zero score. A
lower bucket remains available when a higher one has no feasible action.

### Considerations and curves

Each candidate starts with a base utility and multiplies independent normalized
considerations:

```text
raw_score = base_utility * product(consideration_score)
weighted_score = raw_score * category_weight * trait_weight
```

Every score is finite and clamped to a documented range. A precondition returns
zero. Initial curve types:

- Linear and inverse linear.
- Power and inverse power.
- Logistic threshold.
- Piecewise authored points.
- Boolean gate.
- Window, useful for an ideal middle range.

Do not compare actions with radically different numbers of decorative
considerations. A consideration exists only when it can veto or materially
reshape the choice. If multiplication bias becomes measurable, add one tested
compensation rule globally rather than hand-correcting individual actions.

Example, doctor responding to a casualty:

```text
base 0.85
* qualification gate
* casualty severity curve
* inverse path-cost curve
* bed or stabilization-spot gate
* responder duplication penalty
* duty trait weight
```

Example, covert actor poisoning exposed food:

```text
base 0.55
* private-goal relevance gate
* useful toxin possession gate
* exposed food servings curve
* inverse witness-risk curve
* inverse security-attention curve
* target reservation gate
* antagonist commitment weight
```

### Selection stability and variety

- Keep the current action while it remains valid and within a hysteresis band
  of the new winner.
- Every action has minimum commitment, reevaluation interval, hard timeout, and
  interruption class.
- Immediate survival can interrupt anything interruptible.
- Emergency response can interrupt leisure and routine work.
- Routine work does not interrupt another routine action before minimum
  commitment unless its target becomes invalid.
- Repeating the same leisure or social action receives a cooldown penalty.
- After scoring, candidates within an authored delta of the best may be chosen
  through deterministic weighted randomness.
- Randomness shrinks toward zero as urgency approaches one.
- The authority seeds selection from campaign/shift identity, agent identity,
  and decision ordinal so tests and replays are reproducible.

## Action state machine

```text
Unassigned
  -> Reserved
  -> Traveling
  -> Performing
  -> Resolving
  -> Cooldown
  -> Unassigned

Any active phase
  -> Interrupted
  -> Cleanup
  -> Unassigned

Reserved or Traveling or Performing
  -> Failed
  -> Cleanup
  -> Unassigned
```

Required action hooks:

- `preconditions`: Pure feasibility and candidate generation.
- `reserve`: Atomically claims every required slot.
- `begin`: Creates public activity and movement intent.
- `on_arrive`: Starts work, pairing, carrying, sitting, eating, or treatment.
- `tick`: Advances bounded performance time and monitors invalidation.
- `resolve`: Applies each side effect exactly once.
- `interrupt`: Applies any partial outcome explicitly.
- `cleanup`: Releases reservations and transient markers on every exit path.

Movement adapter:

- Fixed-point and entity-target travel initially use `send_on_errand` and
  `ErrandResolved`.
- An action-specific marker maps `ErrandResolved.walker` back to the current
  intent.
- An action that needs ordinary route behavior after completion restores a
  fresh route through one utility-owned helper.
- `CrewRoute` and `Errand` never coexist on the same moving actor.
- Long term, shared movement may be extracted beneath both, but utility AI does
  not need that refactor to begin.

## Reservations and claims

Reservation keys must support:

- Workstation slot.
- Job ticket.
- Movable object.
- Patient.
- Medical bed.
- Food preparation slot.
- Food serving slot.
- Seat.
- Social partner.
- Investigation target.
- Evidence or contraband item.

Rules:

- Candidate scoring can inspect capacity but cannot claim it.
- Selection reserves atomically before movement begins.
- Multi-target actions either reserve all required keys or reserve none.
- Emergency responders may supersede lower-priority reservations only through
  an explicit interruption policy.
- A patient may have one primary transporter and one treatment team reservation
  with authored capacity.
- Beds, seats, and workstations have stable map-authored slot IDs.
- Cleanup is idempotent and tested against despawn and target removal.

## Workstation and room authoring

Do not overload `crew_post` with every future semantic. Add a map-backed
utility affordance such as `utility_spot` with validated properties:

```text
id
kind
domain
capabilities
capacity
facing
work_seconds
risk_profile
linked_object
```

Examples:

```text
cargo.manifest.1
cargo.dispatch.1
cargo.weigh.1
cargo.sort.1
cargo.requisitions.1
medical.bed.1
medical.monitor.1
security.dispatch.1
engineering.utility_panel.1
service.kitchen_prep.1
service.food_counter.1
service.table.1
service.seat.1
botany.plot.1
bridge.watch_console.1
```

Existing decoration names identify useful visual locations, but a decoration is
not automatically an interactable workstation. The map loader validates unique
IDs, walkable positions, legal domains, capability names, and linked entities.

Rooms provide modifiers rather than commands. Service should become a strong
hangout because it contains food, seats, social partners, lower duty pressure,
and social activities. A poisoned, dirty, overcrowded, frightening, or hostile
Service room should lose that appeal naturally.

## Job tickets and work outcomes

### Job ticket lifecycle

```text
Available -> Reserved -> InProgress -> Completed
                    \-> Released
                    \-> Failed -> FollowUp or Closed
```

Ticket sources:

- Recurring departmental work generators with bounded queue sizes.
- A real station state change, such as low stock or dirty table.
- An incident, casualty, fault, theft report, or contaminated meal.
- A request from another department.
- A campaign or antagonist event.

Ticket completion must change at least one real fact. It may update stock,
clear a fault, move an item, prepare food, help a patient, generate a report,
change relationships, adjust instability, or create a follow-up ticket.

Pure animation loops may exist as `MaintainPost`, but they are the lowest-value
work action and cannot starve consequential tickets.

### Workplace risk

Risk is an outcome of completing or failing a hazardous task, not a random
damage timer attached to a room.

Inputs may include:

- Ticket risk profile.
- Current equipment fault or spill.
- Agent fatigue, impairment, and caution.
- Missing protective condition.
- Recent repeated exposure.
- Station instability.

Safeguards:

- No injury during the opening grace period.
- A per-agent and per-domain incident cooldown.
- No chain of repeated injuries from one unresolved ticket.
- Serious injury requires a visible or inspectable risky condition.
- Seeded deterministic rolls.
- Tunable upper bounds per shift.
- Debug controls to force the next outcome for tests.

### Station stability and department problem frequency

`StationStability` is the single authority-owned pressure signal for how often
ordinary department problems become real. Do not add a parallel AI difficulty
meter. Stability affects frequency and grace, while the triggering work,
equipment, supply, food, body, or relationship state still determines what can
happen.

Problem sources use the signal once:

- Routine Chemistry demand continues to use the existing
  `StabilityBand::order_gap_multiplier` and active-order bonus.
- Hazardous job outcomes use the shared `WorkplaceRiskPressure`, calculated
  from the ticket's authored base risk and the exact hidden stability value.
- Future Engineering faults, Botany crop problems, Service failures, Cargo
  mishaps, Medical supply failures, Security reports, and Bridge operational
  problems must submit candidates to one bounded department-problem director.
- Consequences derived from an already active incident, such as a Medical case
  or Security report, are not rolled again as new stability-driven problems.
- Antagonist actions are chosen from motive, opportunity, supplied resources,
  witnesses, and evidence risk. Low stability may make an opportunity more
  attractive, but it does not fabricate poison, illicit chemicals, or intent.

The initial shared workplace curve is continuous:

```text
health = clamp(stability / 100, 0, 1)
multiplier = lerp(0.35, 2.00, 1 - health)
routine chance = min(ticket base risk * multiplier, 0.50)
```

At full stability, an authored workplace risk occurs at 35 percent of its base
frequency. At zero stability, it occurs at twice its base frequency, with a
hard 50 percent chance cap per eligible completion. A zero-risk job remains
zero-risk at every stability value.

Grace is consumed by hazardous completions, not elapsed wall time:

| Stability band | Grace consumed per hazardous completion |
|---|---:|
| Stable | 1 |
| Strained | 1 |
| Unstable | 2 |
| Critical | 3 |
| Evacuating | 3 |

Department adapters must also observe an unresolved-incident cap, a cooldown,
and the global bounded incident ledger. During the Cargo pilot the opening
grace is six hazardous completions, the post-incident cooldown is eight
hazardous completions, and only one unresolved Cargo incident may exist. These
are tuning values, not permission to fork the formula in each department.

Forced debug outcomes bypass the grace and probability roll so tests remain
deterministic, but they do not bypass active-case or ledger caps. If a forced
outcome is blocked by a current case, it remains queued until that case is
resolved. High stability therefore makes routine operation meaningfully safer,
low stability increases pressure, and neither state can produce an unbounded
casualty cascade.

## Crew menu and player communication

The existing anytime Social directory becomes the Crew menu. Do not build a
second overlapping crew screen. Preserve its binding, department navigation,
shops, `PublicRelationship`, and complete shared `ConversationHistory`, then
extend that screen with utility-AI information.

The menu answers four questions:

1. Who works here?
2. What is this department dealing with right now?
3. What is each person publicly doing or recovering from?
4. Is there something the player can choose to help with?

It communicates every actionable or publicly known consequence, not every
private simulation value.

### Information architecture

Crew overview:

- One compact card per Medical, Security, Engineering, Cargo, Service, Botany,
  and Bridge.
- Qualitative `DepartmentCondition` and a short evidence-based reason such as
  "freight queue growing," "two beds occupied," or "plots need treatment."
- Core pair portraits/names and counts for working, responding, resting,
  injured, unavailable, and support crew.
- One most-actionable public need, incident, shortage, or accepted aid state.
- Department standing and relationship tone without utility numbers.

Department page:

- The department's room purpose and current condition.
- Two equal, prominent core-character cards.
- Current public work categories and backlog summary.
- Known casualties, faults, shortages, investigations, meal or harvest state,
  and cross-department dependencies.
- Voluntary-aid intake status: empty, awaiting assessment, accepted, in use,
  quarantined, rejected, or outcome observed.
- Qualitative guidance about what the department says it can use, based only on
  its current knowledge and authored expertise.
- Supporting crew in a compact roster with public activity and condition.
- Existing department shop, requisitions, and favors in the same department
  context instead of a parallel economy page.
- Recent public outcomes relevant to the department.

Core-character page:

- Name, title, department, job specialty, and stable portrait/model identity.
- Public relationship tier, personal history, favor state, and authored role.
- Current or last reported activity, such as on duty, responding, in Medical,
  on break, at the Chemistry window, or unavailable.
- Last publicly known room, with stale wording when the information is old.
- Known injury, impairment, or recovery state only when visible, reported, or
  voluntarily disclosed.
- Current concern or department perspective.
- The complete uncapped shared-save conversation transcript for that person.
- A local `Track department` or `Locate room` action that provides navigation
  help without issuing remote orders or moving an NPC.

Support-character rows:

- Stable name, job title, public activity, and known condition.
- Shared dialogue/history only when the player has actually interacted with or
  heard that worker.
- No fake personal-standing meter or bespoke favor slot unless the character is
  later promoted to core.

### Public-status vocabulary

Department condition:

```text
OnSchedule
Busy
Strained
Backlogged
Emergency
```

Crew activity examples:

```text
Working
TravelingForWork
Responding
WaitingForAccess
OnBreak
Socializing
Eating
SeekingTreatment
TreatingPatient
RecoveringInMedical
RequestingChemistry
AssessingAid
Unavailable
LastSeen
```

These are presentation summaries, not controllers. The menu cannot set an
NPC's action by writing `NpcActivity` or public status.

### Knowledge and spoiler boundary

- Exact action scores, needs, traits, target reservations, and private intent
  never appear.
- Secret antagonist identity, covert goals, evidence not yet discovered, and
  private witness memory never appear.
- Unknown chemical contents remain unknown. A label is displayed as a claim,
  not truth.
- A hidden poison may appear as "aid accepted" until symptoms, inspection, or
  a credible report changes public knowledge.
- Department condition reasons are selected only from facts the station has
  published or the team can already observe.
- Core NPCs may disagree or offer incomplete interpretations. The menu records
  source and wording instead of converting every report into omniscient truth.

### Diegetic and menu communication work together

- Rooms show the physical evidence first: queued work, occupied stations,
  injured bodies, dirty tables, failing plants, or unattended consoles.
- Speech bubbles and conversations explain local interpretation.
- Radio reports communicate urgent station-wide changes.
- The Crew menu preserves the latest public summary and full direct-dialogue
  history so the player does not need to memorize a fleeting line.
- The menu must not become a live surveillance map. Location is current only
  when public or observed, otherwise it is last-known and may be stale.

### Co-op behavior

- Department status, public crew status, incidents, and conversation transcript
  are shared-save facts and identical for all players.
- All dialogue by all players remains in the complete transcript.
- Menu selection, scrolling, tracked department, and local navigation pin are
  per-player presentation state.
- A late join receives the current public snapshot and full transcript without
  receiving private AI state.

### UI implementation boundary

- Evolve `SocialView` and `spawn_social_directory` into the Crew menu instead
  of registering a second interaction mode.
- Preserve the configurable Social/Crew binding during migration so player
  settings do not break. The displayed label may become `Crew` before an input
  schema rename is considered.
- Replace the fixed eight-name social assumptions with the authored 14-person
  core roster.
- Keep the flat, compact hierarchy already used by the shared UI primitives.
- Core pairs must remain readable at ordinary resolution without making the
  support roster or department condition require several nested screens.
- Controller and keyboard navigation must reach every department, core
  character, transcript, shop, aid summary, and back action.
- Menu systems read safe snapshots and never query authority-only utility state
  directly on clients.

### Crew menu tests

- Every department appears exactly once and has exactly two core characters.
- Botany and Bridge have independent standing, pages, and core pairs.
- A support worker appears under exactly one home department and never gains a
  fake core relationship card.
- A changing real backlog changes the qualitative department condition and its
  displayed reason.
- A public activity change updates the correct character without exposing the
  private action ID.
- Unknown contents display only the claimed label and custody state.
- A covert actor and an innocent worker performing the same public handling
  activity produce indistinguishable menu status.
- The complete conversation transcript survives department switching, save,
  load, co-op sync, and late join without partitioning or a line cap.
- A guest can render the complete menu from replicated public state alone.
- The menu cannot mutate AI intent, reservations, job tickets, or locomotion.
- Keyboard and controller traversal tests cover the expanded seven-department
  directory.

## Department performance and voluntary player aid

This system lets the player respond to what they observe instead of waiting for
an authored order. It is deliberately integrated with utility jobs, chemistry,
health, trust, and incidents. It is not a second delivery economy.

### What "lagging behind" means

`DepartmentWorkState` derives pressure from real facts:

- Available, claimed, overdue, and failed job tickets.
- Number and capability of conscious available workers.
- Workers diverted to emergencies, treatment, social recovery, or antagonist
  actions.
- Unresolved faults, hazards, stock shortages, dirty stations, or missing
  process inputs.
- Recent throughput compared with bounded authored demand.
- Patient load for Medical, case load for Security, faults for Engineering,
  freight for Cargo, service demand for Service, and plot/harvest health for
  Botany.

The authority may retain normalized pressure for scoring, but the player sees
only a qualitative `DepartmentCondition`:

```text
OnSchedule
Busy
Strained
Backlogged
Emergency
```

Visible evidence must support the label. Workers queue at stations, freight or
dishes accumulate, beds fill, faults remain active, plants wilt, NPCs mention
the pressure, and the station board summarizes the condition without exposing
utility scores. A status cannot say `Backlogged` when there is no world fact the
player can inspect.

### Player interaction

The first implementation uses one authored `department_aid_intake` per job
domain. With a container held, the prompt is `Offer contents to <department>`.
The player must physically visit the department.

On a valid offer:

- The authority verifies sender, reach, interaction mode, held ownership,
  container existence, and remaining contents.
- The container is removed from the hand once and placed visibly in the intake
  or transferred to department custody.
- The batch receives stable provenance recording the contributing player,
  claimed label, actual `Solution`, time, department, and intake location.
- No order is fulfilled, no delivery research is awarded, and no success is
  assumed at handoff time.
- A utility job ticket tells qualified workers there is aid to assess.
- Same-frame reservation prevents two NPCs or two interaction handlers from
  consuming the same batch.

Directly offering a batch to a named NPC may be added after the intake path is
proven. It must use an explicit `Offer aid` interaction mode so a held-container
press cannot simultaneously deliver an order, inject a body, start dialogue,
and donate the same container.

Normal department aid and an illicit deal are distinct transaction types. A
batch placed in `department_aid_intake` cannot silently satisfy an illicit
request or enter private illicit custody. Player-only illicit stock transfers
only through an accepted, embodied deal handoff to the requesting NPC. If an
NPC later steals a department batch, that is a separate witnessed or
discoverable physical action with its own custody transition.

### Aid lifecycle

```text
Offered
  -> AwaitingAssessment
  -> Accepted -> Stored -> ReservedForUse -> Used -> OutcomeObserved
  -> Rejected -> Returned or Disposed
  -> Quarantined -> Inspected -> Accepted, Disposed, or Reported
```

Authority-side state includes:

- Source entity and player identity available to existing co-op attribution.
- Destination `JobDomain` and intake slot.
- Claimed label and actual contents.
- Remaining amount and container entity.
- Assessment confidence and known hazards.
- Intended personnel, treatment, or process use.
- User, patient, workstation, or job ticket consuming it.
- Outcome and whether standing, suspicion, or campaign consequences were
  already applied.

### NPC assessment and utility actions

New candidates include:

```text
AssessDepartmentAid
CollectDepartmentAid
StoreDepartmentAid
AdministerDepartmentAid
UseProcessAid
RejectDepartmentAid
ReturnDepartmentAid
DisposeDepartmentAid
QuarantineDepartmentAid
ReportSuspiciousAid
```

Assessment uses only plausible knowledge:

- Department expertise and worker capabilities.
- Visible container type and label.
- Existing analysis or known recipe information.
- Relationship and department standing as trust, not proof.
- Current department pressure and shortage.
- Known contraindications, dose limits, legal status, and recent bad outcomes.
- A credible report, prior symptom, or inspection result.

The claimed label affects what staff believe. The real solution affects what
happens when it is used. A trusted player can therefore convince a strained
department to accept something dangerous, but trust never changes the chemistry.

### Personnel aid

Personnel aid enters an NPC's existing body through an appropriate real route.
The AI never receives a direct productivity modifier from a reagent ID.

- Correct treatment reduces existing damage or status through ordinary
  metabolism and treatment effects.
- Stimulants may improve work pace or willingness for a limited period only
  through derived physical status. They can also increase unsafe intensity,
  inhibit rest, cause overdose, produce a crash, or feed existing addiction.
- Sedatives reduce or stop work and may create an evacuation case.
- Toxins, allergens, alcohol, and mixed impurities cause their ordinary body
  consequences.
- Dose, purity, route, current bloodstream, health, and repeated exposure all
  remain relevant.

One shared `work_capacity` calculation reads mobility, consciousness, motor
control, fatigue, relevant statuses, and job capability. One shared
`work_risk` calculation reads impairment, intensity, fatigue, equipment state,
and task risk. Department executors must not each invent a separate rule that
"stimulant means faster."

### Process aid

Some departments use chemicals as materials rather than drugs:

- Medical can stock a valid treatment for an active case.
- Engineering can consume a suitable cleaning, cooling, sealing, or reaction
  material for a compatible fault.
- Cargo can use safe cleaning or handling materials for a compatible freight
  ticket, while personnel drugs remain personnel aid.
- Service can accept a real ingredient, cleaner, or drink batch.
- Botany can use nutrients, treatments, water, or pest controls on a compatible
  plot.
- Security can quarantine, test, or use only explicitly authorized operational
  supplies. Contraband does not become legitimate because it was donated.

Process tickets declare required chemical properties, quantity, and method.
They do not simply compare one exact reagent name unless the authored process
genuinely requires it. Consumption removes real volume and applies reaction,
contamination, residue, or disposal consequences where appropriate.

### Department-specific opportunities and risks

Medical:

- Useful medicine can shorten an actual treatment backlog.
- Wrong medicine, overdose, impurity, or a dangerous interaction can worsen a
  patient and create an investigation.

Security:

- Legitimate supplies may support an active authorized task.
- Stimulant use can extend alertness but raise aggression, error risk,
  addiction, or later fatigue.
- Suspicious, mislabeled, or contraband aid is likely to be quarantined and may
  increase scrutiny.

Engineering:

- Correct process chemistry can clear a fault or protect a hazardous task.
- Stimulants may increase short-term throughput while increasing burn, spill,
  or machinery risk at high intensity.
- Flammable, corrosive, or incompatible material can create a real hazard.

Cargo:

- Useful handling or cleaning material can clear a compatible ticket.
- Personnel stimulants can help a strained shift temporarily, but impaired
  judgment or a crash can produce dropped freight, injury, or delay.
- The Cargo pilot is the first place this end-to-end loop is implemented.

Service:

- Ingredients, drinks, cleaners, and treatments enter real meal, bar, or
  cleanup work.
- Unsafe contents can contaminate a batch and affect several consumers.
- Staff may reject inappropriate drugs even when the room is busy.

Botany:

- Nutrient or treatment chemistry can recover an unhealthy plot or improve a
  real harvest.
- Herbicides, toxins, contaminated water, or excessive dose can damage the
  plot, contaminate produce, or create hazardous ingredients.

Bridge:

- Command support may accept only tightly scoped personnel or operational aid.
- Alertness drugs can affect watch performance but carry the same overdose,
  behavior, addiction, and crash risks as anywhere else.

### Consequence and trust rules

- Standing changes after an observed outcome, not merely because the container
  was accepted.
- Effective help can improve the involved worker's relationship and the
  department average through existing standing rules.
- A harmless but useless batch is returned, stored, or discarded with little or
  no reward.
- A careless harmful batch causes a relationship penalty proportionate to the
  consequence.
- A false label, repeated harmful offers, contraband, or evidence of intent can
  create Security suspicion and stronger penalties.
- The simulation records evidence and provenance but does not read the player's
  mind. Accident, recklessness, and deliberate poisoning are conclusions drawn
  from observable facts and patterns.
- A department that is desperate may accept more risk. This makes pressure
  mechanically meaningful without making every strained NPC irrational.
- An antagonist can later exploit, steal, replace, or contaminate an aid batch
  through the same embodied opportunity and perception rules.

### Multiplayer and save rules

- The authority validates the offer and owns assessment, selection, and use.
- Clients see the offered container, public custody/activity, department
  condition, and physical outcomes.
- Exact assessment confidence, hidden contents not otherwise knowable, and
  private suspicion remain authority-only.
- Late join receives current intake contents and public condition.
- Accepted but unused aid, actual contents, provenance, and any linked job or
  patient persist when losing them on reload would erase a player consequence.
- Reservations and current candidate scores rebuild after load.

### Acceptance scenarios

Helpful voluntary aid:

- Cargo is visibly Backlogged because freight exceeds conscious worker
  capacity.
- The player offers a suitable, safely dosed batch without an outstanding
  order.
- A qualified Cargo worker assesses and uses or administers it.
- Actual work capacity or a compatible ticket improves for a bounded duration.
- The batch volume is consumed, its user is identifiable, and a later crash or
  repeated exposure remains possible.

Harmful voluntary aid:

- The player offers a mislabeled sedative, toxin, overdose, or incompatible
  process chemical.
- Trust and pressure may make the department accept it, but neither changes its
  real contents.
- Resulting impairment, injury, contamination, or poisoning changes utility
  decisions and may create Medical or Security work.
- The same container cannot also satisfy an order or be used twice.

Rejected aid:

- An on-schedule department, cautious worker, obvious contraband label, unsafe
  dose, or irrelevant material can make acceptance utility zero.
- Rejection preserves or returns the container according to the authored intake
  rule and explains the decision qualitatively without exposing scores.

## Medical case flow

### Case creation

An injured or poisoned body creates or updates one `MedicalCase` keyed by the
patient entity. Severity derives from the existing body and bloodstream, not a
second hit-point field.

The case records:

- Patient and last known location.
- Mobility and consciousness.
- Damage/status summary appropriate for AI scoring.
- Required treatment categories.
- Reporter and known responders.
- Time first seen and time last updated.
- Reserved bed and assigned transporter, if any.
- Linked Chemistry request, if any.

### Response branches

Mobile, non-critical patient:

- Self-seek Medical, accept escort, or remain at work based on severity,
  personality, and duty pressure.

Immobile or critical patient:

- A qualified responder reserves patient and bed, travels to the patient,
  attaches a transport relationship, moves both safely to Medical, places the
  patient in the bed, then monitors or treats.

No free responder or no suitable medicine:

- A knowledgeable coworker or Medical worker creates one linked treatment
  ticket and becomes its requester.
- The existing order pipeline recalls that resident rather than spawning a
  duplicate.
- The requester returns to the casualty or treatment station with the physical
  delivered batch. The handoff at Chemistry does not dose the requester.
- Application at the case location updates the linked patient's real
  `Body`/`Bloodstream` according to the medicine, amount, purity, and route that
  actually arrived.

### Requester, carrier, beneficiary, and use destination

An order must not assume that the NPC speaking at the counter is the person who
will consume its contents. Linked requests explicitly distinguish:

- `requester`: the NPC who asked Chemistry for the batch.
- `carrier`: the NPC currently responsible for transporting it after handoff.
- `beneficiary`: the patient, machine, meal batch, plot, or other world target
  that caused the request.
- `source`: the `MedicalCase`, incident, job ticket, shortage, or private goal
  that created the request.
- `use_destination`: the entity or last-known location where application must
  happen.
- `application`: intended route, bounded dose, required capability, and any
  workstation or tool requirement.

Self-use is valid only when `beneficiary == requester` and the request was
created from that NPC's own condition. A doctor's role, presence at the counter,
or possession of the batch is never evidence that the doctor is the patient.

Linked fulfillment is a two-stage lifecycle:

```text
Requested
  -> AcceptedByChemistry
  -> PreparedAndHandedOver
  -> CarriedToUseDestination
  -> AssessedAtDestination
  -> AppliedToBeneficiary, Rejected, Quarantined, Lost, or Expired
  -> OutcomeObserved
```

The Chemistry handoff removes the request from the counter queue and records
the prepared batch result, but it does not resolve the underlying case or job.
The physical container and exact remaining `Solution` move into NPC custody.
The utility controller selects `ReturnTreatmentToCase` or the corresponding
department delivery action, reserves the beneficiary or workstation, and uses
the normal travel executor. On arrival, the action revalidates the target,
capability, route, dose, and contents before applying anything.

Clinical or process outcome is emitted separately from preparation outcome.
Standing, research, campaign effects, and incident resolution must each declare
which stage owns them so no consequence fires twice. At minimum, preparing a
correct batch can credit Chemistry work, while healing, worsening, refusing,
or losing the batch changes the linked case only after the destination action.

Implemented migration seam: `orders::complete_delivery` preserves the original
self-consumption behavior only when an order has no explicit `OrderUse` context.
A linked order transfers the exact physical container into the requester's
custody, removes it from player inventory or the delivery tray, attaches the
beneficiary, source, use destination, route, dose, and supplier context, and
routes the carrier away without changing the requester's bloodstream. The
arrival-side system follows a moved beneficiary, applies only the bounded dose,
leaves the unapplied solution in the same container, and emits a clinical
fulfillment result separate from Chemistry's preparation result. An invalid
beneficiary sends the carrier home with the batch and never falls back to
self-use. This branch is selected only by explicit context, never by department
or role.

If the beneficiary moved, the carrier follows current authorized knowledge or
asks for an updated report. If the beneficiary died, recovered, disappeared,
or no longer needs the material, the carrier returns, stores, quarantines, or
disposes of the real batch according to policy and utility. The system never
falls back to applying it to the carrier merely because the original target is
invalid.

### Beds and posture

- Beds are explicit capacity-one utility slots.
- `InMedicalBed { bed }` is public, while treatment details stay authoritative.
- A mobile patient walks to the bed; an immobile patient is transported.
- The bed executor positions and orients the patient through a presentation
  anchor without moving the authoritative root outside the navigation rules.
- Add a dedicated lying/recovery animation only if the current collapsed clip
  is visually unsuitable. Do not fake an occupied bed by despawning the body.
- Discharge releases the bed before selecting the next action.

## Service room, food, and social behavior

### Food model

A `MealBatch` or equivalent station object contains:

- Stable batch ID and recipe ID.
- Servings remaining.
- Existing chemistry `Solution` or another direct adapter to it.
- Preparation quality and temperature.
- Prepared by and preparation time.
- Exposure state and location.
- Public appearance that does not reveal hidden contaminants.
- Provenance and evidence known only through perception or inspection.

Eating transfers a serving's chemical dose into the consumer's stomach through
the existing bloodstream path. Effects are therefore ordinary chemistry and
can trigger the same symptoms, incapacity, speech, Medical cases, and treatment
as any other exposure.

### Social affordances

Initial pair and group actions:

- Sit near others.
- Eat or drink together.
- Casual conversation.
- Gossip about a remembered incident.
- Comfort an injured or upset coworker.
- Ask for help or report a concern.
- Celebrate a resolved incident.
- Argue after a grievance.
- Avoid, leave, or report a threatening person.

Each action has:

- Participant gates and capacity.
- Relationship and mood considerations.
- A physical meeting location or seat reservation.
- A bounded duration.
- Speech pools appropriate to public knowledge.
- Need and relationship outcomes.
- Cooldowns that prevent one pair from looping forever.

### Food poisoning as a utility action

`PoisonFood` is not triggered merely because a selected campaign antagonist
exists. It requires all of the following:

- An embodied agent with an active private goal compatible with poisoning.
- Knowledge of or possession of an effective contaminant.
- An exposed meal with meaningful remaining servings.
- Physical access to the meal.
- A non-zero expected goal benefit.
- Acceptable witness and evidence risk according to personality and pressure.
- No conflicting reservation or already-achieved goal.

The actor travels, performs a visually ambiguous food-handling action, transfers
real reagent into the meal, and creates evidence/stimuli only for observers who
could perceive it. Consumers do not know the food is poisoned before symptoms
or a credible report unless they witnessed the act or inspected it.

## Perception, reports, and memory

### Stimuli

Initial stimulus kinds:

- Visible injury or collapse.
- Cry for help or spoken report.
- Fire, smoke, spill, or machine fault.
- Suspicious handling, assault, theft, or tampering.
- Food served, consumed, rejected, or linked to symptoms.
- Job assignment or department request.
- Social invitation, argument, comfort, or accusation.

Each stimulus has source, subject, location, modality, strength, created time,
and expiry.

### Sensing rules

- Sight uses distance, room/doorway logic, and the existing occlusion precedent.
- Hearing uses earshot and event loudness.
- Department radio or direct report can create knowledge without sight.
- Doorways keep last-known-room semantics where required.
- An NPC may always know its own assigned job and own physical state.
- Scoring consumes memory facts, not unrestricted world truth, except for
  invariant engine facts such as whether its reserved target still exists.

### Memory and evidence

- Confidence decays by fact kind.
- Reports preserve who said what.
- Witness memories can feed Security interviews.
- Evidence is a world object or incident record, not a global guilty flag.
- Contradictory reports remain distinct until an investigation resolves them.
- Player-visible speech stays qualitative. Numeric scores and hidden meters
  remain hidden.

## Antagonist integration

The utility framework supplies embodied opportunity selection. It does not
replace campaign truth or flatten every antagonist into identical sabotage.

Private authority-only state:

- Allegiance or covert role.
- Active private goals.
- Known tools, access, and opportunities.
- Risk tolerance and exposure pressure.
- Goal completion and evidence left behind.

Public state:

- The same identity, movement, animation, held objects, speech, and visible
  consequences available for ordinary workers.
- Ambiguous activities wherever possible.

Rules:

- Existing Cult inversion and authored thread outcomes remain valid.
- Department minor antagonists migrate one representative behavior at a time.
- Covert action utility is based on the current goal and station opportunity,
  not a generic desire to cause random harm.
- An antagonist can choose ordinary work or social behavior when no covert
  action is useful or safe.
- A witnessed action affects witness memory, Security work, social trust, and
  future action scores.
- Successful covert effects flow through existing campaign and instability
  mutators exactly once.
- No client receives a component whose presence identifies the secret actor.

## Illicit supply, player motivation, and choice

### Player-only special chemical rule

Special illicit chemicals do not exist in an NPC's hidden inventory by default.
The only way an illicit NPC obtains one is through a physical batch supplied by
a player.

Add an authored reagent or request classification such as
`PlayerOnlyIllicit`. The exact name is provisional, but the invariant is not:

- No NPC job, covert action, incident generator, shop, load hook, or campaign
  step may spawn a player-only illicit reagent for NPC use.
- An illicit request cannot be resolved from an abstract flag or meter. It must
  transfer actual solution volume from a player-owned container.
- The received container or transferred batch records source player,
  transaction, receiving NPC, actual solution, amount, purity, claimed label,
  time, and custody history.
- Saving and loading preserves unused illicit stock and provenance.
- Use, transfer, confiscation, destruction, dilution, or return changes the
  physical stock. No hidden copy remains.
- A covert action requiring that chemical has a zero utility score when no
  matching physical player-supplied stock remains.
- The same batch cannot support two incidents unless enough real volume remains
  for both authored doses.

Dangerous chemicals that arise naturally from Botany or ordinary station
processes remain available through those real sources. The player-only tag is
for the special illicit substances and exact plot materials whose supply choice
belongs to the player.

Audit the current illicit shop. `spawn_illicit_offer` currently creates authored
reagent containers for the player. A player-only illicit reagent may not appear
in that generated offer pool. Replace conflicting offers with equipment,
information, favors, legal rare inputs, non-player-only contraband, or a physical
batch whose prior provenance actually exists. Asset validation rejects a
player-only reagent in an NPC-generated offer.

### Physical illicit custody

The authority tracks a player-supplied illicit batch through ordinary container
and solution state plus private custody metadata:

```text
Requested
  -> OfferedByPlayer
  -> ReceivedByNpc
  -> Carried, Cached, Transferred, or Hidden
  -> ReservedForAction
  -> Consumed, Confiscated, Destroyed, Returned, or Recovered
```

The NPC may choose to hold, hide, move, transfer, return, destroy, or use the
batch according to goals and risk. Receipt does not guarantee immediate harm.
This gives the player time to notice behavior, recover evidence, warn others, or
change the situation before the final action.

Public presentation remains ambiguous. A visible container, handoff, cache, or
food-handling action can be observed, but private custody purpose and action
scores do not replicate. Security conclusions require evidence, witnesses, or
credible reports.

### Every deal needs a real upside

Giving an illicit NPC what they request must never be authored as only a delayed
punishment. Every illicit request declares at least one concrete
`PlayerBenefit`, and data validation rejects a request with no benefit.

Possible benefits:

- Immediate personal standing with the requesting core character.
- Underworld goodwill that unlocks the existing off-book offer economy.
- A guaranteed or selectable physical reward.
- Access to rare equipment, ingredients, containers, or non-player-only stock.
- A banked favor such as expedited freight, quiet access, an extra inspection,
  information, evidence, a warning, or help with a future incident.
- A department work shortcut or temporary service that changes a real job fact.
- A campaign opportunity or antagonist-mode advantage where appropriate.

Benefits must arrive clearly and reliably enough to feel like a trade. Risks can
be delayed, uncertain, or discoverable, but the positive side cannot be a vague
promise that always collapses into a larger guaranteed penalty.

Continue using `UnderworldStanding` as hidden progression and cost where it
fits. The Crew menu and dialogue describe it qualitatively, for example "they
owe us," "off-book access is opening," or "that contact trusts you." Do not
show the raw meter.

### Player decision options

An illicit approach should expose several meaningful responses through
conversation and physical action:

- Cooperate fully: supply the exact requested batch for the strongest reward
  and highest downstream opportunity.
- Negotiate: choose among authored rewards, ask for payment first, or trade a
  smaller amount when the relationship permits it.
- Counteroffer: provide a safer functional substitute when the requester can
  plausibly accept one, earning less benefit and reducing some risks.
- Delay: accept the conversation but supply nothing yet, allowing circumstances
  and utility opportunities to change.
- Refuse: preserve the stock and close or cool the opportunity without treating
  refusal as a universal failure.
- Deceive: mislabel, dilute, substitute, mark, or track the batch, with outcomes
  based on inspection, chemistry, and later discovery.
- Report or expose: take the learned request or physical evidence to Security,
  trading underworld access for lawful trust and investigation progress.
- Recover: steal back, confiscate, neutralize, or destroy a supplied batch before
  it is used when physical access and player actions permit it.

Not every character or request must support every option. Authored content lists
available choices and their known terms. The system must always support at least
cooperate, delay, refuse, and report once the player has enough evidence to make
reporting plausible.

### What the illicit NPC weighs

After receiving a batch, utility candidates may include:

```text
CacheIllicitStock
MoveIllicitStock
TransferIllicitStock
InspectIllicitStock
UseIllicitStockForGoal
ReturnIllicitStock
DestroyIllicitStock
ConcealIllicitEvidence
```

Use utility considers:

- Active private goal relevance.
- Real reagent, amount, dose, purity, and route.
- Opportunity value and number of affected targets.
- Current campaign pressure.
- Witness, evidence, and Security risk.
- Trust in the player and confidence in the claimed label.
- Whether the batch appears tampered with, diluted, tracked, or substituted.
- Whether a safer or more valuable future opportunity is likely.
- Personal traits and current physical state.

An NPC can therefore decide not to use a supplied chemical yet, discover a bad
substitution, take a reckless opportunity, or abandon the plan under pressure.
The player's delivery enables the option. It does not script the outcome.

### Crew menu and conversation boundary

- An illicit request never labels a character as illicit in the department
  overview.
- Terms the player actually heard appear in that core character's complete
  shared conversation transcript.
- Known promises, favors owed, completed rewards, and choices made may appear on
  the character page without revealing hidden underworld numbers.
- All players can review all dialogue from all players, including an illicit
  conversation, while secret allegiance and unobserved future intent remain
  authority-only.
- Public custody appears only when observed or reported. The Crew menu is not a
  tracker for hidden contraband.

### Consequences and balance

- Successful supply can raise underworld standing and grant its authored
  benefit.
- Exact harm happens only if and when the NPC uses the physical batch.
- Security suspicion comes from the existing illicit resolution and from later
  evidence or witnesses, not omniscient knowledge of private intent.
- Harmful use can create casualties, department backlog, relationship damage,
  crisis pressure, evidence, and campaign movement.
- A safer substitute may reduce goal utility or alter the outcome instead of
  being treated as the exact reagent.
- Reporting can improve lawful relationships while closing or damaging illicit
  opportunities.
- Repeated cooperation can produce stronger offers and stronger risks, giving
  the player an escalating but voluntary temptation curve.
- Rewards and harms use separate tuning fields so balancing one never silently
  erases the other.

### Required illicit supply tests

- A player-only illicit reagent has no NPC source in jobs, shops, incidents, or
  campaign initialization.
- An illicit NPC without player-supplied stock scores every matching chemical
  action at zero.
- Delivering a physical batch creates exactly one custody record with exact
  volume, contents, purity, label claim, and source.
- A normal department-aid donation cannot satisfy an illicit request, and one
  interaction cannot resolve both transaction types.
- Consuming part of a batch reduces later available dose.
- Confiscation, destruction, or recovery removes the corresponding action
  option.
- Save/load preserves unused stock and provenance without duplication.
- Current NPC-generated offer data cannot name a player-only illicit reagent.
- Every authored illicit request has at least one concrete player benefit.
- A successful deal grants the promised benefit even if no later covert action
  becomes worthwhile.
- Cooperate, delay, refuse, and report paths do not collapse into the same
  outcome.
- Safer substitution changes chemical and campaign consequences rather than
  secretly becoming the requested reagent.
- Covert action utility can choose to cache a supplied batch when witness risk
  is high and use it later when opportunity changes.
- Replicated state cannot reveal which NPC currently holds private illicit
  intent or where an unobserved cache is located.

## Multiplayer and replication

Authority-only:

- Needs and traits unless a presentation value is explicitly required.
- Candidate lists and score traces.
- Current private intent.
- Memory and knowledge.
- Reservation book and job-claim internals.
- Allegiance, private goals, witness risk, and antagonist opportunity.
- Random seeds and decision ordinals.

Replicated:

- Existing `CrewMember`, appearance, transform, `Body`, and `Bloodstream`.
- Minimal `NpcActivity` for presentation.
- Public workstation, meal, bed occupancy, fault, incident, and interaction
  state when clients need to render or interact with it.
- Speech through the existing replicated speech contract.

Network tests must prove:

- A guest sees the same actor, movement, posture, occupied bed, meal servings,
  symptoms, and public activity.
- A guest cannot inspect replicated state to identify a hidden antagonist,
  private score, witness memory, or exact motive.
- Late join reconstructs public station state without restarting actions or
  duplicating residents.

## Persistence

Persist only state that remains meaningful after reload:

- Stable personality overrides.
- Long-term personal needs if playtesting shows reload exploits matter.
- Existing body and bloodstream state where the save system already supports
  it, extended deliberately if required.
- Long-lived relationship and social memory already owned by Social/Shift.
- Active consequential incidents, admitted patients, linked treatment requests,
  meal contamination, and unresolved faults when losing them would erase player
  consequences.
- Campaign and antagonist goal progress through existing save contracts.

Do not persist:

- Current candidate scores.
- Path waypoints.
- Frame timers.
- Reservations.
- Pair locks.
- Decision cooldowns shorter than a normal save transition.

On load, rebuild job tickets, reservations, and action selection from persistent
world facts. Save migrations use defaults and retain existing flat field names
where current saves depend on them.

## Debugging and tuning

### Decision trace

In debug builds, retain a bounded trace per agent:

```text
decision ordinal
time
eligible bucket
candidate action and targets
raw fact values
curve outputs
weights
final score
selected or rejection reason
current-action hysteresis result
```

Provide filters by NPC, department, action, and incident. The trace is developer
diagnostics, never player UI.

### Required debug controls

- Pause utility selection while movement and metabolism continue.
- Force an agent reevaluation.
- Force the next workplace outcome.
- Spawn a specific job ticket or incident.
- Set a need to a normalized value.
- Show reservations and target capacity.
- Show current public activity and private action in separate debug views.
- Run a deterministic accelerated simulation without real-time waits.

### Performance targets

- Eight current residents should be negligible beside rendering and chemistry.
- Headless scale tests cover at least 8, the planned 30, and 64 agents.
- Decisions are staggered and event-woken.
- Context gathering avoids allocation in hot loops after schemas stabilize.
- Spatial queries reuse room/nav/perception indexes rather than scanning every
  entity for every consideration.
- Debug traces are bounded and compiled or gated out of release hot paths.

## Test strategy

### Pure tests

- Every curve endpoint, clamp, monotonicity expectation, and non-finite input.
- Multiplication and zero-gate behavior.
- Bucket eligibility and fallback.
- Hysteresis, minimum commitment, interruption, and timeout.
- Deterministic near-best selection.
- Reservation atomicity, capacity, cleanup, and stale-entity recovery.
- Data validation for every stable ID and cross-reference.

### ECS contract tests

- Selection does not move transforms.
- Only navigation moves an active traveling actor.
- Resolution applies a consequence once.
- Every action exit releases all reservations.
- `CrewRoute` and `Errand` never coexist on a utility traveler.
- Incapacity suppresses selection and stops movement.
- Residents remain unique when recalled for a treatment request.
- Utility agents do not count toward ordinary visitor queue caps.
- Client presentation requires no authority-only component.

### Scenario tests

Cargo burn and direct Medical response:

- A forced hazardous Cargo outcome damages the real Cargo worker.
- Exactly one Medical responder and one bed are reserved.
- The responder reaches the casualty.
- The casualty reaches and occupies the reserved bed.
- The patient remains in-world and receives treatment.
- Recovery releases the bed and both agents resume valid decisions.

Cargo burn and Chemistry request fallback:

- With responders unavailable or treatment missing, exactly one linked request
  is created.
- An existing resident becomes requester without a duplicate spawn.
- Chemistry handoff transfers the physical batch to the requester without
  changing the requester's body.
- The requester walks back to the patient or treatment station before use.
- Application affects the linked patient's body and consumes only the applied
  amount from the carried solution.
- If the patient moves, recovers, dies, or becomes unreachable, the batch is
  retained, returned, stored, quarantined, or disposed rather than used on the
  requester.
- Expiry leaves the case unresolved and raises appropriate urgency/consequence.

Service meal:

- Service creates a real batch with several servings.
- Hungry residents reserve seats/servings and eat.
- Needs change and ingredients reach stomach contents.
- Empty batches stop attracting consumers.

Service poisoning:

- A compatible covert actor with toxin, opportunity, and no witness can select
  poisoning.
- Removing any hard gate makes the action score zero.
- Poison transfers into the batch once.
- Consumers receive dose-based effects.
- A valid witness remembers and can report the act.
- An occluded or absent NPC gains no witness knowledge.
- Medical and Security tickets emerge from symptoms and reports.

Department job coverage:

- Every staffed job domain can generate, claim, perform, and resolve at least
  two routine jobs and one consequential response job.
- Support workers participate in task competition, aid, incidents, and social
  behavior rather than only filling work animations.
- No job domain's routine loop monopolizes all residents.
- Service room appeal changes with food, seating, safety, and crowd state.

### Falsification requirements

Every regression test added for a bug or missing contract must be run once with
the relevant implementation disabled and shown to fail. A test that passes with
and without the behavior is not evidence.

### Validation commands

Run one test filter per Cargo command.

```powershell
cargo fmt --all -- --check
cargo check --tests
cargo test --bin chemgame utility_ai
cargo test --bin chemgame crew
cargo test --workspace
cargo clippy --workspace --all-targets
git diff --check
```

Use an isolated `CARGO_TARGET_DIR` and constrained jobs if Windows reports cache
or OS-error-3 races. Record warnings as existing, introduced, or resolved. Do
not call a warning-producing run warning-free.

## Delivery phases

### P0: Architecture and living plan

Status: Complete, with this document remaining live

- [x] Inventory current NPC, navigation, health, order, social, campaign, map,
  and replication seams.
- [x] Define station-wide goals and department coverage.
- [x] Define scoring, action lifecycle, reservation, perception, incident, and
  multiplayer boundaries.
- [x] Review this plan against current save serialization and asset-loading
  conventions immediately before P1.
- [x] Convert approved provisional type names into a compiling skeleton.

Exit criteria:

- This document is accepted as the implementation source of truth.
- No open architecture question blocks the foundation types.

### P1: Utility kernel and migration seam

Status: Complete for migrated residents

- [x] Add `utility_ai` plugin and ordered system sets.
- [x] Add curves, typed facts, candidates, buckets, deterministic selection,
  hysteresis, and debug traces.
- [x] Add `CurrentAction`, `NpcActivity`, and `ReservationBook`.
- [x] Add one authority-side control owner and atomic compare-and-swap handoff
  foundation, including order recall interruption, reservation cleanup, and
  return-to-utility ownership restoration. Incapacity suspends and later
  restores the exact active controller without allowing locomotion to continue.
- [x] Make legacy ambient and utility selection queries explicitly disjoint.
- [x] Adapt fixed-point and entity travel to `Errand`/`ErrandResolved`, including
  caller-owned arrival radius for precise work posts.
- [x] Add one inert `IdleObserve` and one `MaintainPost` action.
- [x] Opt one headless test resident into utility AI while other residents keep current
  ambient behavior.
- [x] Prove co-op public/private component boundaries with tests.
- [x] Reopen action reservations and claimed job tickets when their worker is
  despawned.
- [x] Exclude migrated residents from legacy ambient and chemical-status intent
  changes while retaining legacy behavior for every unmigrated resident.

Exit criteria:

- One resident repeatedly chooses, travels, performs, resolves, and replans.
- Selection is deterministic under a fixed seed.
- All cleanup and exclusivity invariants pass.
- Anti-conflict tests prove one decision owner and one locomotion writer.
- Existing NPC behavior remains functional for agents not yet migrated.

### P2: Cargo micro-pilot

Status: In progress, headless routine and first incident slice implemented

- [x] Author Cargo's second core character plus two stable support workers,
  keeping support workers outside the random customer roster.
- [x] Add only the utility map spots and reservation slots required by Cargo.
- [x] Add the minimum general job ticket lifecycle used by the pilot.
- [x] Implement manifest review, dispatch, weighing, sorting, and requisitions
  as Cargo tickets and executors.
- [x] Give Cargo work real lightweight state such as pending freight, processed
  freight, or cleared requisitions.
- [x] Opt both Cargo core characters and the two Cargo support workers into
  utility AI while every other resident retains current ambient behavior.
- [ ] Run a manual station observation. The deterministic accelerated shift is
  complete and covers all four workers, five job types, travel, consequences,
  reservations, cleanup, and replanning.
- [x] Record every Cargo-specific assumption that must be generalized before
  another department uses the system.
- [x] Connect hazardous Cargo completions to the shared station-stability risk
  curve, opening grace, cooldown, unresolved-case cap, and forced test hook.

Pilot generalization notes:

- The shared kernel knows only `PerformJob`, `JobDomain`, capabilities, ticket
  targets, and reservation slots. Cargo job names and pipeline consequences
  remain in `cargo_pilot.rs`.
- P2 publishes at most one open ticket per Cargo workstation. Later department
  adapters may publish several ticket instances, but must retain stable ticket
  IDs and bounded generation.
- Cargo uses one shared `cargo.operations` capability during the pilot. P3 may
  split desk, handling, hazardous freight, and delivery qualifications without
  changing selection.
- Cargo work resets on entering a career session and receives bounded new
  freight every 90 seconds. Save persistence is deliberately deferred to P8.
- Map point targets are floor-authored and are lifted to the acting body's
  current height before navigation. Entity targets retain the target entity's
  live transform.

Exit criteria:

- Four Cargo workers choose among several jobs, travel safely, reserve separate
  targets, work for readable durations, change real Cargo state, clean up, and
  replan.
- Disabling utility selection returns them to a safe fallback without affecting
  the rest of the station.
- The pilot runs without target collisions, route/action conflicts, duplicate
  residents, or unbounded ticket growth.
- Cargo behavior uses shared facts, tickets, reservations, and action contracts
  instead of Cargo conditionals embedded in the kernel.

### P3: Generalize the pilot and add all routine job domains

Status: **All seven department adapters are landed.** Medical, Cargo, Botany,
Service, Engineering, Security, and Bridge each own their work state, publish
bounded tickets, and migrate their residents off the legacy ambient controller.
What remains in P3 is depth rather than coverage: Medical rounds/restock and
Security patrol beyond the current dispatch loop, plus the authored-tuning move
out of Rust.

- [x] Review the Cargo pilot and remove Cargo problem-frequency policy from
  shared code. Cargo retains only its authored work state and burn consequence.
- [x] Freeze `JobDomain`, `NpcJobProfile`, `NarrativeTier`, utility spot,
  capability, ticket, support roster, and executor registration contracts.
- [x] Split station-resident identity from legacy ambient decision state and
  migrate visitor filters before expanding utility control.
- [ ] Expand validated map affordances and bounded job generators.
- [x] Add one bounded `DepartmentProblemDirector` for non-job problem
  candidates, using `StationStability` once per candidate and never rerolling
  consequences from an already active case.
- [ ] Medical: rounds and restock.
- [x] Security: patrol and dispatch. `security.rs` staffs dispatch, the officer
  desk, and evidence audit, all capped below interview urgency — a department
  whose job is noticing should stay available. Every worker carries
  `security.interview`, which is what makes `interviews.rs` claimable at all.
- [x] Engineering: inspect and maintain. Bounded per-asset tickets, forced power
  faults, repair resolution, and stability-scaled severity are green.
- [x] Service: kitchen preparation, hosting, serving, and cleanup. Autonomous
  hunger-driven eating is deliberately excluded until the opportunity buffer
  exists; `consume_meal_serving` is the public seam it will call.
- [x] Botany: plot tending, harvesting, processing, and physical ingredient
  output. Service now consumes that physical output.
- [x] Bridge: qualified watch and briefing duties. `bridge.rs` staffs helm,
  comms, station monitor, and briefing. Helm and comms use *distinct*
  capabilities so Odera and Sissel are complementary rather than
  interchangeable; monitoring is the shared baseline so support can hold a
  console. Four authored spots reuse existing proven-walkable `duty` post
  coordinates, because the Bridge floor is four carved rectangles rather than
  one open room.
- [x] Add or promote exactly two core player-facing characters per department.
  Bridge was the last: Odera and Sissel moved from `crew::fluff` onto
  `station.crew.ron` and into `social::RESIDENT_NAMES` (`[&str; 14]`).
- [ ] Add two support residents per non-Bridge department, retain four Bridge
  support residents, and migrate one department at a time while preserving
  low-value wander/visit fallback actions.
- [x] Add Botany relationship membership and legacy save migration, including
  Ivy's move and seeded Vale/Amari standing.
- [x] Add Bridge relationship membership and neutral legacy standing when
  Odera and Sissel are promoted. Bridge is the *inverse* of the Morrow case and
  deliberately seeds nothing: no save has ever held Bridge standing, so neutral
  is the correct load result and copying standing in would invent a reputation
  the player never built. `a_promoted_bridge_starts_neutral_and_inherits_nothing`
  pins that against a future "fix".

Adapter-hardening decisions already integrated:

- `DepartmentProblemDirector` now owns one candidate-to-permit-to-commit flow
  for problem frequency. It samples station stability once, enforces opening
  grace, domain and actor cooldowns, unresolved and per-shift caps, and retains
  forced outcomes when an adapter aborts because its consequence could not be
  applied. Cargo currently uses grace 6, both cooldowns 8, unresolved cap 1,
  and shift cap 4.
- `StationResident` is durable identity, while `Ambient` is only one legacy
  behavior mode. Startup population adopts an existing same-name visit rather
  than creating a second body. Restock, illicit-offer, smuggler, crisis, order,
  and Medical paths either recall that body or defer while it is busy.
- Cargo and Medical residents are excluded from legacy favor scheduling while
  migrated. This is a temporary safety boundary, not removal of their social
  content. P5 must express those favors as utility `Socialize` or authored
  social-task actions before the old path is deleted.
- A collapse creates at most one Medical incident for the exact utility body.
  Legacy evacuation cannot despawn that patient, and Medical admission is the
  single cancellation point for incompatible visits, carried orders, social
  commitments, and older transport work.
- Linked deliveries are gated by body health and exact `OrderVisit` ownership.
  Losing that lease drops the real carried container and reports an unresolved
  application instead of silently consuming or self-applying it.
- Forced movement is ordered explicitly: damage is observed before utility
  navigation, physical impulses run after navigation, and Medical passenger
  attachment runs afterward. Pursuit, route, errand, and transport writers all
  require their matching owner.
- Emergency replacement is transactional. Selection rechecks the expected
  owner, action instance, job profile, health, and emergency ticket, then
  acquires the replacement ticket and target before releasing the old work.
  A failed proposal rolls back only its partial claims, so another worker never
  loses an action or reservation merely because it also considered the alert.
- A debug and test-only authority-side invariant checker validates settled
  controller, locomotion, action, suspension, and reciprocal Medical transport
  shapes after deferred commands apply. It reports structural state only and
  does not expose private utility or antagonist data.
- Medical combines simultaneous supported injury reports for one body into one
  active case, keeps the most urgent treatment kind and severity, and resolves
  every linked incident together. A second damage kind cannot leave a stale
  report that reopens after the patient recovers.
- Botany is now allowed to integrate against these frozen shared contracts.
  Final P3 acceptance still requires the Botany reuse proof and the remaining
  routine department adapters.

Exit criteria:

- Every staffed job domain visibly performs real routine work.
- Every completed job changes a real station fact or is explicitly classified
  as low-value post maintenance.
- Residents distribute across available work without target collisions.
- Botany work produces real inputs for Service or Chemistry-facing systems.
- Every department has two distinct core-character interaction contracts.
- The station has the 30-resident initial target with stable unique identities.

### P3B: Crew menu, department condition, and voluntary chemical aid

Status: **Complete.** The projection layer, the condition derivation, the aid
lifecycle, the handoff, the shared work-capacity reading, NPC assessment, and
the outcome payouts are all implemented and tested end to end.

- [x] Evolve the existing Social directory into the Crew menu without adding a
  competing interaction mode or losing current shops and transcripts. The
  department page now carries a condition strip; `SocialView`, the binding,
  shops and `ConversationHistory` are untouched, and no second mode exists.
- [x] Add safe `PublicDepartmentStatus` and `PublicCrewStatus` projections.
  Both live in `utility_ai::public`, are replicated components, and are the
  only thing the menu reads —
  `a_guest_can_render_the_menu_from_replicated_state_alone` builds a world
  holding neither `JobBoard` nor `IncidentLedger` and renders anyway.
- [x] Add overview, seven department pages, two core-character cards per page,
  compact support rosters, public work state, and recent outcomes. The overview
  is the department rail itself rather than an eighth screen: it already lists
  all seven departments, so each row now carries its condition beside its
  standing. The plan forbids a second competing screen, and a separate overview
  page would have been exactly that.
- [x] Add `DepartmentWorkState` and evidence-backed qualitative condition.
  Condition is a pure function of counts a player could take themselves, and
  `reason_is_backed_by_a_countable_world_fact` sweeps the whole input space to
  prove no label worse than `OnSchedule` is ever shown without countable
  evidence behind it.
- [x] Add one aid intake per department and validated held-container handoff.
  Seven `*.aid_intake` spots are authored in `lab.map` and covered by the
  existing `utility_spots_are_unique_walkable_and_routable` check.
- [x] Add aid custody, provenance, assessment, storage, rejection, quarantine,
  personnel use, process use, and observed outcome. The full lifecycle, its
  legal transitions, and the NPC assessment that drives it all live in
  `utility_ai::aid`. Every department's roster carries its own
  `<domain>.assess_aid` capability, so the published ticket is claimable by
  that department and nobody else.
- [x] Connect actual body chemistry to work capacity and work risk.
  `utility_ai::capacity` is the single shared reading. No department may
  special-case a reagent — the route is always reagent → real bloodstream →
  metabolized `StatusKind` → this module.
- [x] Connect successful and harmful outcomes to existing personal standing,
  department standing, incidents, addiction, and Security suspicion. An
  accepted batch earns department standing, a refusal costs a little, and a
  quarantine costs a lot *and* raises Security suspicion. `pay_out_aid_outcomes`
  reads batch state rather than a message — outcomes arrive by several routes
  and all of them end in a new state — so `claim_payout` is what makes it
  exactly once.
- [x] Add co-op, late-join, no-secret-leak, input conflict, and full-transcript
  tests. Co-op, late-join, secret-leak and input-conflict are covered; the
  transcript was already covered by the existing social tests and is
  unaffected, since the Crew menu extends that screen rather than replacing it.

Exit criteria:

- The player can identify why a department is falling behind without seeing a
  utility score.
- Every department page has two unique core characters and current actionable
  public information.
- A voluntary batch can help, do nothing, be rejected, or cause real harm based
  on its contents, dose, purity, label, route, context, and staff assessment.
- An aid handoff cannot also deliver an order, inject a body, or consume one
  container twice.
- The Crew menu renders from public state and preserves the complete shared
  conversation transcript.

### P4: Incidents, injuries, Medical transport, and linked requests

Status: **Complete.** Reference flow, all three department injury outcomes, and
the explicit witness-driven incident-report path are implemented.

- [x] Add the bounded incident ledger and shared stability-driven workplace
  risk pressure contract.
- [x] Add Cargo, Engineering, and Botany injury outcomes with grace periods and
  caps. Cargo burn, Botany poisoning and Engineering arc-flash burn are all
  implemented, sharing the same permit, grace and cap machinery. Engineering's
  injury is gated on fault severity rather than rolled separately, so a
  deteriorating station produces more casualties without a second source of
  randomness — and a routine breaker trip stays a repair job.
- [x] Add Medical case creation from real incident subjects and retain the exact
  patient entity through the response.
- [x] Add emergency responder selection, escort transport, exclusive bed
  reservation, visible lying posture, bounded standard burn care, discharge,
  and incident resolution for the Cargo reference case.
- [x] Add explicit incident-report actions and broader authored diagnosis. A
  delivered `Casualty` report now emits `CasualtyReported`, and
  `medical::open_cases_from_reports` turns that into a real incident. This is
  the only route into the ledger that requires a *person* to have seen it: every
  other one is a system that already knew. Gated on the reporter not being the
  subject, on their confidence still clearing `CONFIDENT_ENOUGH_TO_FILE` at
  arrival, and on the body actually still being down — a walk takes time, so a
  stale report is the ordinary case. Diagnosis stays Medical's: the kind is read
  from the reported body's real damage, not from what the witness claimed.
- [x] Add complex-case monitoring, missing-patient and interrupted-request
  recovery, and treatment branches for cases standard care cannot resolve.
- [x] Add linked Chemistry treatment requests through the existing order flow.
- [x] Split linked request preparation from destination application, preserve
  the physical delivered batch in NPC custody, and route the requester or
  assigned carrier back to the beneficiary.
- [x] Ensure only arrival-side application changes the linked patient's body.
  Target invalidation returns the carrier with the retained batch and never
  applies it to the requester. Clean treatment resolves the real case only
  after the patient's matching damage actually improves. Harmful, illicit,
  overdosed, empty, lost, and expired attempts leave the case open for a
  bounded retry or explicit escalation.

Exit criteria:

- Both Cargo burn reference branches pass headless end-to-end tests.
- Patients remain embodied through transport, bed occupancy, treatment, and
  recovery.
- No case, bed, responder, or requester duplicates.

### P5: Needs, Service hub, Botany supply, food, and social actions

Status: **Complete.** The opportunity seam, all three needs, eating, resting,
two-person conversation, room appeal, chemistry-driven mood, Botany-to-Service
consumption, and authored tuning in `station.needs.ron` are landed and
falsified. Gossip/comfort/argue remain listed under social actions as a future
content pass; they need remembered incidents and are not blocking.

- [x] Add the generic `UtilityOpportunityBuffer` before autonomous hunger,
  rest, or social selection. These actions must not be disguised as jobs.
  Providers join the `OpportunityProviders` set inside `BuildContext`; the
  buffer is cleared ahead of them every frame and consumed by the same selector
  that reads the `JobBoard`.
- [x] Add needs and personality tuning. All three `NpcNeeds` axes drive real
  actions and respond to room and body state, and every rate, threshold,
  duration, recovery amount and room-appeal weight now lives in
  `assets/data/station.needs.ron` as `NeedsTuning`. Validated at plugin build,
  so an incoherent file fails the game to start rather than producing a station
  that behaves oddly for no logged reason. Per-personality weights remain a
  future refinement — the file is keyed per-station today.
- [x] Connect Botany harvests to Service meal inputs. Service's intake query
  *requires* `BotanyProduceProvenance`, so only real Botany output can be taken
  in, and `intake_is_bounded_and_rejects_held_or_hazardous_botany_output`
  pins the bounded and hazardous cases. This checkbox was stale — the work had
  landed with the Service adapter.
- [x] Add meal preparation, servings, ingestion, cleanup, and quality. A hungry
  resident now selects `EatFood` through the opportunity seam, walks to the
  exact batch, and the serving's real solution enters that diner's bloodstream
  through the same `consume_meal_serving` path a player interaction uses.
- [x] Add seats and pair/group reservations. Two capacity-one lounge seats and
  one capacity-two gathering spot are authored in `lab.map` and validated by the
  existing walkable/routable spot test.
- [x] Add initial social actions and relationship outcomes. `Rest` and a
  two-person `Socialize` are live, with symmetric pair cooldowns and a small
  bounded standing gain for both participants. Gossip, comfort, argue, and
  celebrate remain, and all need the P6 memory of a remembered incident.
- [x] Add room appeal based on real affordances and current conditions.
  `RoomAppeal` scales break offers from served food, uncleared plates, and
  reservation-book occupancy, and names a qualitative reason. It is a modifier,
  never a command — nothing sends a resident to Service.
- [x] Make current chemistry statuses influence needs and action selection.
  `social_disposition` turns Happiness/Euphoric/Sadness/Paranoid/Hallucinating
  into one bounded willingness-to-linger multiplier, and `fit_for_leisure`
  suppresses voluntary breaks under heavy sedation, burning, or choking. Both
  read only the existing `Bloodstream`; there is no second mood simulation.

Exit criteria:

- Service is a naturally selected hub when it is stocked, safe, and welcoming.
- NPCs can eat, socialize, affect one another, and leave when conditions worsen.
- Food chemistry has real downstream body and Medical effects.
- Botany shortages, contamination, and quality alter Service choices.

### P6: Perception, knowledge, reports, and Security response

Status: **Complete.** Sight, hearing, doorway handling, bounded episodic memory,
confidence decay, memory-gated response, spoken reports, witness interviews, and
the Security adapter are all landed. The knowledge loop is closed end to end: an
NPC acts on what it learned, can volunteer it by telling someone, can be *asked*
for the quiet things it would never volunteer, and there is now a department
qualified to do the asking.

The one thing P6 cannot do on its own: nothing yet emits `SuspiciousHandling`,
so a real canvass always closes `Unsolved`. P7's covert actions are its intended
source. The machinery is proven by tests, not by play.

- [x] Add stimuli, sight, hearing, room/doorway handling, and memory.
  `src/utility_ai/perception.rs` generalises the strongest existing precedent
  (`speech::place_bubbles`): distance, room identity, and the real
  `interaction::authority_segment_blocked` occlusion test. Explicit reports
  (`Modality::Told`) are modelled but nothing publishes them yet.
- [x] Gate response scoring by what the NPC knows. `JobTicket` gained
  `subject: Option<Entity>` — what the work is *about*, as distinct from
  `target`, where the worker walks. A ticket naming a subject is answerable
  only by an agent whose `NpcMemory` holds that subject; a ticket with
  `subject: None` is station work at a known place and is never gated. Both
  selection seams are covered: `select_reference_actions` filters candidates,
  and `preempt_for_emergency_jobs` both filters and scales the proposal by a
  new `UtilityFactId::Awareness` fact, so among several who know, the one who
  actually saw it outranks one who only heard a shout.
- [x] Add spoken reports. `src/utility_ai/reports.rs` offers a witness holding
  urgent news the chance to walk to the nearest crew member who does not yet
  know and tell them, on the opportunity seam at `Important` — above routine
  work, below actually treating the casualty. Delivery is physical: the
  reporter speaks via `speech::say` and only crew within conversational range
  learn anything, so a report can never be a broadcast. Hearsay is recorded as
  `Modality::Told`, keeps its teller, drops the actor, and retains only 60% of
  the teller's confidence, so a chain of retellings decays rather than
  laundering a rumour into certainty.
- [x] Add witness interviews, investigation tickets, and evidence handling.
  `src/utility_ai/interviews.rs` opens an `Investigation` for each covert
  incident (`Tampering`/`Theft`/`Contamination` — nobody needs asking who saw a
  fire), then publishes one interview ticket at a time, canvassing outward from
  the incident. Each interview reads exactly one witness's own `NpcMemory`;
  there is no global query in the module. Testimony keeps its `Modality`, so
  evidence is ordered seen > heard > told, and a case with accounts but no name
  closes `Unsolved` rather than inventing a suspect. A closed case with a named
  culprit writes back through the new `IncidentLedger::attribute`, which never
  overwrites an existing attribution.
- [x] Migrate compatible existing Security awareness and sweep behavior.
  `src/utility_ai/security.rs` is the 6th of 7 department adapters. Reyes and
  Bex were already the authored core pair, so unlike Engineering there was no
  data gap; Patrol Officer Dlamini and Dispatcher Novak join as support through
  `crew::fluff`, and four `utility_spot`s were authored into the already-dressed
  Security rooms. **Every** Security worker carries `security.interview` —
  investigation is the department's defining act, not a specialisation, so a
  canvass never stalls on one officer being busy. Routine work (dispatch, desk,
  evidence audit) is deliberately thin and capped below interview urgency: a
  department whose job is noticing should spend most of its time available.

Exit criteria:

- NPCs respond to seen, heard, assigned, or reported facts and remain ignorant
  of facts they could not know.
- Witness and occlusion scenario tests pass.

### P7: Embodied antagonist utility actions

Status: **Complete for the reference scenario.** The supply invariant, physical
custody with save/load, private goals, the food-poisoning reference action,
Botany's minor antagonist, the player response paths with the required-upside
rule, and the Crew-menu conversation that lets a player actually choose one are
all landed. The relationships/campaign/instability wiring and the second-proof
errand migration remain, and neither blocks the scenario.

- [x] Add private goals and authority-only covert candidate generation.
  `src/utility_ai/covert.rs`. `PrivateGoal` is motive-shaped, not action-shaped:
  a goal explains why sabotage might be worth it and leaves the act to scoring,
  so `Grudge` produces no poisoning offer at all because a communal batch cannot
  target one person. Offers sit in `Routine` — a covert act is done *instead of*
  ordinary work, never ahead of a casualty.
- [x] Classify player-only illicit reagents and enforce the no-NPC-source rule.
  `ReagentDef::player_only`, distinct from both `controlled` (Security cares) and
  `Category::Illicit` (recreational, someone gets hooked). The antagonist offer
  audit now rejects a player-only reagent in any NPC-generated offer.
- [x] Add physical illicit custody, provenance, remaining-volume, recovery,
  confiscation, and destruction contracts. `IllicitCustody` holds real
  `Solution` volume with provenance; spending *removes* it, so a part-spent
  batch stops funding a second incident at the honest point. Save/load is done:
  `CustodyRecord` persists in `progress.ron` keyed by **holder name**, following
  `addiction`'s precedent — crew entities do not survive walking offscreen, let
  alone a reload. `restore` replaces rather than appends, so reloading twice
  cannot double a batch, and terminal states are never written, so a reload
  cannot undo a confiscation the player earned.
- [x] Add required player benefits and cooperate, negotiate, counteroffer, delay,
  refuse, deceive, report, and recover paths where authored, and a Crew-menu
  conversation that offers them. `PublicApproach` replicates only the terms the
  player was told; the authority re-validates every answer.
  `src/utility_ai/deals.rs`. The upside rule lives in *data validation*, not a
  review checklist: `IllicitRequest::validate` rejects a request with no benefit,
  and separately rejects one whose benefits are all worth zero — a
  `Standing { amount: 0 }` entry would otherwise satisfy a naive non-empty check
  while granting nothing. `BotanistPlugin` runs that validation at load, so an
  authored trap fails to start the game. `grant` reads only the request's own
  terms and never consults custody, goals, or covert scoring, which is what makes
  the benefit arrive whether or not harm ever follows; the botanist handler calls
  it *before* the embodiment check for the same reason. Tuning a benefit and
  tuning a harm touch different constants and different functions.
- [x] Audit NPC-generated illicit offers and replace every conflicting reagent
  source. No authored offer named a player-only reagent, so nothing needed
  replacing — but the assertion now exists so a future one fails at test time.
- [x] Implement food poisoning as the first complete covert reference action.
  The contaminant becomes part of the meal's ordinary `Solution`, so harm
  travels the same bloodstream path as any other exposure and `quality_percent`
  — fixed when the legitimate ingredients cook — cannot leak it.
- [x] Connect witnesses, evidence, Security, Medical, relationships, campaign,
  and instability. Witnesses and evidence: the act emits an *ambiguous*
  `SuspiciousHandling` stimulus, the input `interviews.rs` was built for.
  Medical follows for free once someone eats. Relationships, campaign and
  instability are priced by `price_covert_harm`, which fires on a **poisoning
  incident traced to a tampered meal** — not on the act, so a contaminated meal
  nobody eats costs nothing and the player can still recover the batch.
  `TamperedMeals` carries provenance from bowl to body, because once the
  contaminant is ordinary `Solution` nothing in the chemistry says a person
  chose it.
- [ ] Migrate one existing department-minor errand as a second proof.
- [ ] Preserve Cult outcome inversion and existing campaign gates.

Exit criteria:

- The food poisoning reference scenario passes end to end.
- It cannot occur with a player-only special chemical unless a player physically
  supplied enough of that chemical and it remains in NPC custody.
- Every illicit deal grants a concrete promised upside independently from any
  later harm.
- Player response paths produce meaningfully different rewards, risks, custody,
  relationships, and campaign state.
- An antagonist chooses ordinary life when sabotage is irrelevant or unsafe.
- Clients cannot infer secret identity from replicated state.

### P8: Persistence, scale, balancing, and removal of legacy ambient decisions

Status: **Blocked on content, not code.** Five of seven items are done. The two
`Ambient` removals cannot land until the off-roster cast migrates — see below.

- [x] Persist consequential world state and rebuild transient state on load.
  `AidIntakes` now round-trips through `ProgressSave.department_aid`, carrying
  real `Solution` contents, the claimed label, and the `consequences_applied`
  flag so a reload cannot re-pay a settled donation. Terminal batches are
  deliberately not written.
- [x] Add late-join and save migration tests. A pre-donation save still parses;
  loading replaces rather than merges; a save naming one department twice still
  leaves one counter. Late-join was already covered by
  `a_guest_can_render_the_menu_from_replicated_state_alone`.
- [x] Run 8, 30, and 64-agent accelerated simulations. All three drive the real
  select/begin/resolve chain. What they add over the unit tests is the
  *composite* property: at population, every ticket has at most one owner, no
  unclaimed ticket holds a reservation, and the queue neither stalls nor
  oversubscribes.
- [ ] Tune action frequency, incident bounds, Service attraction, response time,
  and randomness from playtest traces. **Cannot start**: there are no playtest
  traces, because there has been no playtest. This is the item that genuinely
  needs the user, not more code.
- [ ] Remove replaced random ambient choice code and obsolete marker readers.
- [ ] Remove the legacy controller enum arm, component, registration, and
  handoff paths after the last resident migrates.
- [x] Update `docs/npc-ai.md` from current-state reference to final
  architecture. Its opening claim ("no utility scoring anywhere in the
  codebase") was flatly false; it now leads with the two-decision-system split,
  documents the selector and its three enforced rules, and corrects the stale
  "no perception layer" gap.

**Why the two removals are still blocked.** `Ambient` is not vestigial. Three
production spawners still attach it: `crew::fluff` (the Service/Security/Bridge
support crew), `cult::spawn_guards`, and `shift::restock`'s couriers. None of
that cast has a `NpcJobProfile`, so deleting `Ambient` would leave them with no
decision system at all — they would stand still forever. This was re-verified
by grep this wave rather than assumed from the last one.

Exit criteria:

- Full automated validation passes with warning accounting.
- No legacy decision path competes with utility selection.
- Manual playtest sign-off covers readability, pacing, animation, navigation,
  Service density, emergency response, and antagonist ambiguity.

## Multi-agent work packets

Only one coordinator edits shared registration surfaces and this plan during a
parallel wave. Leaf agents add or modify their owned modules and return a
structured handoff. This prevents merge conflicts in `main.rs`,
`src/utility_ai/mod.rs`, shared schemas, and this document.

Execution order is intentionally uneven:

1. Packet A freezes the kernel and single-controller contract.
2. Packet B supplies only the affordances, tickets, and population schema the
   Cargo pilot needs.
3. Packet C completes and validates the Cargo micro-pilot.
4. The coordinator records pilot corrections and freezes generalized contracts.
5. Department packets may then proceed in parallel against those contracts.

No agent should build a second department executor before the Cargo pilot has
been observed and its shared assumptions reviewed.

### Packet A: Kernel and contracts

Depends on: P0

Owns:

- Curves, facts, candidate scoring, buckets, deterministic selection.
- Action lifecycle, controller ownership, handoffs, reservations, debug trace.
- Unit and ECS contract tests.

Must not:

- Implement department side effects.
- Change health, order, social, or campaign semantics.

Handoff evidence:

- Public types and invariants.
- Tests proving zero gates, hysteresis, cleanup, and determinism.
- Any schema choice that differs from this plan.

### Packet B: Map affordances, job board, and support roster

Depends on: Packet A type freeze

Owns:

- Utility map marker parsing and validation.
- Reservation slot extraction.
- Job ticket lifecycle and generators.
- Data-driven core/support roster schema, stable identity validation, exactly
  two core characters per department, and narrative tier.

Must not:

- Reinterpret decoration spots automatically.
- Edit department executors.

### Packet C: Cargo micro-pilot and workplace incidents

Depends on: Packets A and B

Owns:

- Cargo routine tickets and executors.
- Miner Sato, Cargo's second core character, and two support workers as the
  first utility-controlled group.
- Pilot evidence and generalization findings before other domain work starts.
- Risk profile evaluation and Cargo burn incident creation after the routine
  pilot is accepted.
- The shared station-stability frequency curve, grace consumption policy, and
  active-case safety cap that later department adapters must reuse.

Must not:

- Implement Medical response or order resolution.
- Add random room-wide damage timers.

### Packet D: Medical cases and transport

Depends on: Packets A and B; consumes Packet C incidents

Owns:

- Case derivation, responder actions, transport, beds, treatment, recovery.
- Linked treatment request source and patient resolution adapter.
- Requester, carrier, beneficiary, use-destination, custody, and application
  contracts for every incident-created order.
- Two Medical support workers and their responder capabilities.

Must not:

- Fork the order UI or duplicate body damage.
- Treat the NPC accepting a delivery as its beneficiary unless the request
  explicitly targets that same NPC.

### Packet E: Service, food, and needs

Depends on: Packets A and B

Owns:

- Needs, food batches, preparation, serving, eating, cleaning, room appeal.
- Service routine work and consumption of Botany ingredients.
- Steward Amari as Service's second core character and direct interaction
  content.
- Two Service support workers and their kitchen, bar, hosting, and cleaning
  capabilities.

Must not:

- Implement covert behavior or Security conclusions.

### Packet F: Botany production

Depends on: Packets A and B; coordinates with Packet E data contracts

Status: First vertical slice and relationship/save migration implemented and
headless-validated. Department-specific dialogue remains follow-up work.

Owns:

- Botany job profile, plots, tending, harvests, hazardous produce, and
  deliveries.
- The producer side of Botany-to-Service and Botany-to-Chemistry inputs.
- Botany's new second core character and direct interaction content.
- Two Botany support workers and their capabilities.

Must not:

- Change Service meal consumption or covert contamination behavior.
- Fold Botany standing back into Service or skip its required save migration.

First implementation slice after adapter hardening:

1. Author Botanist Ivy, Agronomist Vale, Grower Chen, and Technician Mbatha as
   Botany's two core and two support residents. The names are stable save and
   memory identities. Ivy emphasizes specimens and player-facing supply;
   Vale emphasizes crop throughput and safety. Chen tends cultivation while
   Mbatha owns irrigation and processing qualifications.
2. Add validated spots for plot inspection, irrigation, crop tending, harvest
   processing, and the output shelf. Decoration markers remain presentation;
   only `utility_spot` markers are reservable gameplay affordances.
3. Implement a bounded plot lifecycle with named plots and explicit dry,
   growing, stressed, ripe, harvested, and quarantined facts. Jobs arise from
   those facts and completing a job changes only its claimed plot or batch.
4. Put harvested produce into a bounded physical output shelf using the
   existing `Produce` identity and catalog. Service and Chemistry consume those
   same entities later; the adapter does not award an abstract department
   productivity point in place of material output.
5. Run Botany trouble through `DepartmentProblemDirector`. Early problems are
   crop stress or a real toxic exposure on the exact processing worker. Stable
   stations keep a long grace and low severity; poor stability raises pressure
   within the same unresolved and per-shift caps used by Cargo.
6. Four workers now distribute across plots and processing, produce bounded
   output, retain one controller and movement writer, and enter the shared
   Medical flow after an exposure. The Service packet may now consume that
   physical output.

### Packet G: Social actions and perception

Depends on: Packet A; coordinates with Packets E and F affordances

Owns:

- Stimuli, sensing, memory, reports.
- Pair/group social actions and relationship outcomes.

Must not:

- Replicate private memory or numeric hidden state.

### Packet H: Security and Engineering

Depends on: Packets A and B; consumes Packet G perception

Owns:

- Patrol, dispatch, inspection, guard, fault response, repair, and bridge duty.
- Adapters for existing compatible Security and Engineering behaviors.
- Chief Engineer Morrow, Engineering support Mechanic Torres and Systems Tech
  Adeyemi, two Security support workers, and their direct interaction content.
- Promotion of Helmsman Odera and Yeoman Sissel to Bridge core characters,
  retention of Park, Alvarez, Fenn, and Ruiz as support workers, and migration
  of all six onto utility jobs.

Must not:

- Change existing campaign outcome semantics.

### Packet I: Antagonist utility integration

Depends on: Packets A, E, and G; integrates with Packet H

Owns:

- Private goals, covert scoring, food poisoning, evidence emission.
- Player-only illicit reagent classification and source validation.
- Physical illicit custody, provenance, consumption, recovery, confiscation,
  destruction, and persistence.
- Deal benefits, negotiation choices, refusal/report alternatives, and current
  underworld-offer audit.
- One migrated existing antagonist errand.

Must not:

- Put secret identity or motive in replicated state.
- Make sabotage happen without embodied travel and performance.
- Spawn a player-only illicit reagent for NPC use or author a request with no
  concrete player benefit.

### Packet J: Integration, persistence, networking, and scale

Depends on: all required feature packets

Owns:

- Shared plugin registration and schedule ordering.
- Save migration and rebuild rules.
- Replication and late-join tests.
- Scale simulation, full-suite validation, tuning, and legacy removal.

### Packet K: Crew menu and voluntary department aid

Depends on: Packets A and B plus the accepted Cargo pilot; integrates every
department as its job state becomes available

Owns:

- Evolution of the current Social directory into the Crew menu.
- Public department and crew status projection.
- Seven department pages, two core-character cards per department, and support
  roster presentation.
- Department condition, aid intake, custody, assessment, actual use, outcome,
  and player feedback.
- Co-op public-state, complete transcript, late-join, and secret-leak tests.

Must not:

- Add a competing menu or interaction mode beside the existing Social flow.
- Read private utility state directly on clients.
- Convert a donated reagent into a generic department buff.
- Let one held-container press trigger aid, order delivery, and body application
  together.

## Agent handoff template

Every agent returns this exact information to the coordinator:

```text
Packet:
Scope completed:
Files changed:
Contracts added or changed:
Tests added:
Commands run and exact results:
Warnings or skipped checks:
Known gaps:
Plan assumptions challenged:
Recommended next packet:
```

An agent must stop and ask the coordinator before changing a frozen shared
contract, campaign semantic, save shape, or another packet's owned module.

## Progress ledger

The coordinator updates this table after integrating evidence. "Implemented"
requires code plus targeted tests. "Validated" requires the phase exit criteria
and full appropriate checks.

| Area | Owner | Status | Evidence | Next action |
|---|---|---|---|---|
| Master plan | Coordinator | Living | This document | Keep synchronized with implementation |
| Opportunity seam | Packet A | Implemented | `UtilityOpportunityBuffer` plus the `OpportunityProviders` set. Three kernel tests prove an offered action runs the ordinary lifecycle, a zero consideration vetoes it even in the Emergency bucket, and a withdrawn offer stops being acted on. Each has a positive control, so none passes vacuously | Add Rest, Socialize, and reporting providers |
| Utility kernel | Packet A | Complete foundation | 76 Utility AI tests pass after Botany registration, covering shared jobs, repeated travel, transactional emergency selection, interruption, incapacity, orphan cleanup, problem pressure, Cargo, Medical, and Botany | Extend through frozen shared contracts only |
| Controller ownership | Packet A | Frozen for shared adapters | Exact order, utility action, scripted errand, pursuit, incapacity, and Medical ownership; transactional replacement preserves losing workers; 10 debug invariant-matrix tests pass | Keep every new adapter inside the matrix |
| Map affordances | Packet B | Generalized through Botany | Validator covers stable ID, capacity, floor, uniqueness, and route checks; five Cargo spots, two Medical beds, and five Botany spots are authored | Add each department's bounded targets with its adapter |
| Job board | Packet B | Implemented pilot | Domain/capability filtering, compare-and-swap claims, completion/reopen, orphan recovery, and bounded Cargo generation | Generalize during P3 review |
| Core character expansion | Packet B and domain packets | In progress | All 14 stable core identities are selected; Cargo is live, and Botany/Service now have Ivy/Vale and Dubois/Amari in the core roster with backward-compatible standing migration | Add Morrow and Bridge pair to Social/save data, then direct interaction content |
| Support population | Packet B | In progress | All 16 stable support identities are selected; Cargo, Medical, and Botany workers are live while Service, Security, and Engineering remain to be spawned and Bridge awaits promotion split | Add remaining support through department adapters without customer-roster leakage |
| Cargo pilot | Packet C | Implemented headless | Four workers complete and distribute manifest, weigh, sort, dispatch, and requisition work; real Cargo state changes and all claims clean up | Manual station observation, then P3 review |
| Workplace incidents | Packet C | Generalized through Botany | Cargo burns and Botany toxic processing affect only the exact worker; both request permits from the shared stability-sampled director with grace, cooldown, unresolved, and shift caps | Add Engineering fault risk without changing the director |
| Medical response | Packet D | Implemented reference flow | A real incident opens one case; an eligible Medical worker claims it, reaches the exact patient, reserves a bed, escorts that entity, and leaves them visibly lying. Simultaneous supported injuries aggregate into one case and resolve together. Missing patients and interrupted transports release claims instead of stranding the state machine | Add explicit reporting, persistence, and manual observation |
| Linked treatment requests | Packet D | Implemented reference flow | A severe case recalls one real Medical worker or Cargo coworker into the existing conversation queue with exact case, beneficiary, destination, route, and dose. Stale attempts reopen, three failed attempts escalate, and no duplicate request system exists | Add player-facing status and save/load |
| Destination delivery | Packet D | Implemented foundation | The physical batch remains in requester custody, follows a moved patient, applies only a bounded dose at arrival, and reports helpful, harmful, illicit, and overdose facts. Clean delivery resolves only after matching patient damage improves; bad treatment leaves the patient admitted | Add custody persistence and player-facing outcome feedback |
| Service and food | Packet E | Implemented headless | Eight tests cover the cast, bounded intake rejecting held/hazardous produce, the four-worker ingredient-to-cleanup pipeline, real ingestion affecting only the exact diner, a stability-scaled kitchen burn, replicated `MealBatch` with authority-only `MealChemistry`, and the cleanup travel-target contract | Add hunger-driven consumption once the opportunity buffer exists |
| Needs and room appeal | Packet E | Implemented | All three needs drive real actions. `RoomAppeal` derives Service attractiveness from served food, uncleared plates, and break-spot occupancy; `social_disposition` and `fit_for_leisure` fold existing chemistry statuses into the same decision. Ten social tests, each falsified — including the one that proves the authored RON is actually consulted rather than shadowed by a matching constant | Per-personality weights, once personalities need to differ |
| Botany production | Packet F | Implemented headless | Seven focused tests prove distinct qualifications, exact per-plot lifecycle, four-worker distribution, four bounded physical outputs, toxic provenance, exact-worker poisoning/quarantine, stability-scaled severity, and one shared Medical case for the exact poisoned worker; Ivy/Vale relationship and legacy-standing migration is green | Consume physical output in Service and add department-specific dialogue |
| Social actions | Packet G | Partial | `Rest` and two-person `Socialize` on the opportunity seam. Six tests cover seat recovery, the fatigue and lonely-alone gates, a real pair conversation crediting both participants, and the symmetric cooldown suppressing a live offer. Each negative test carries a positive control | Add gossip/comfort/argue once P6 supplies remembered incidents |
| Perception and memory | Packet G | Implemented | Sight (range + room + occlusion), hearing through walls at lower confidence, kind-specific confidence decay, bounded per-NPC episodic memory, and chemical concealment reusing `Bloodstream::concealment`. Ten tests, falsified: three fail when occlusion is neutered. A real casualty reaches a nearby witness and nobody across the station. **All five stimulus kinds now have real production sources** — `Food` is emitted when a meal reaches the serving spot, asserted inside the real pipeline rather than hand-written, and carries no contents claim so a clean and a contaminated batch are indistinguishable to a bystander | Consume `Food` memories in a hunger/appeal consideration |
| Covert actions and custody | Packet H | Partial | `covert.rs`: private goals, physical `IllicitCustody` holding real volume with provenance, and food poisoning as the reference act. Nine tests, falsified — the supply veto, the goal gate, volume removal, the no-duplication replace, and the terminal-state filter each fail a specific test when removed. Custody persists by holder name in `progress.ron`. `price_covert_harm` moves instability, campaign and department standing — but only when a poisoning is actually traced to a tampered meal, so an uneaten one costs nothing | Second-proof errand migration |
| Player deal responses | Packet H | Implemented, player-facing | `deals.rs`: seven responses that differ in kind, and the upside rule enforced in `IllicitRequest::validate` — including the zero-value loophole. `grant` is independent of custody and goals by construction. Eight tests, falsified — the empty-benefit guard, the independence ordering, and the refuse/delay distinction each fail a specific test when removed. `reporting_costs_more…` caught a real exploit: cooperate-then-report netted free underworld access. Four more tests cover the Crew-menu seam; the idempotence one proved nothing until `count_for` let it count. `FavorKind` now banks onto `Requisition` and each of the three is spent by one named site, falsified | Wire relationships, campaign, and instability |
| Botany minor antagonist | Packet H | Implemented | `src/botanist/`, `station.botanist.ron`. Grower Aleksy's asks escalate to `quiet_rot`, which is `player_only` — the one route by which an NPC ever holds it is a real player delivery. Refusal costs Botany standing and closes the worst branch outright. Three tests pin that the final ask is player-only, the earlier ones are not, and refusal differs in kind from compliance | Botany is no longer in `EXPECTED_GAP`; Bridge is the last |
| Bridge department | Packet A | Implemented | `bridge.rs`, the last of seven. Odera/Sissel promoted out of `crew::fluff` into a brand-new `Department::Bridge`; Park/Alvarez/Fenn/Ruiz stay support (the only 4-support department). Helm and comms are distinct capabilities so the core pair are complementary. Eight tests, falsified — merging the two capabilities, declaring the wrong support count, or dropping Bridge from `Department::ALL` each fail, the last caught by two pre-existing integration tests as well | Bridge has no minor antagonist thread yet; it sits in `EXPECTED_GAP` beside Botany |
| Security department | Packet A | Implemented | `security.rs`, 6th of 7 adapters. Reyes/Bex core (already authored), Dlamini/Novak support, four new authored `utility_spot`s. Every worker carries `security.interview`. Eight tests, falsified — dropping the interview capability, drifting the core roster from `station.crew.ron`, gating routine work on a subject, or raising routine urgency each fail a specific test. One end-to-end test proves an officer can actually claim the ticket `interviews.rs` publishes | Bridge is the last adapter |
| Witness interviews | Packet G | Implemented | `interviews.rs`: covert incidents open an investigation that canvasses witnesses one at a time, reading each one's own memory. Evidence ordered seen > heard > told; no name means `Unsolved`, never a guess. Seven tests, falsified — the per-witness read, the attribution no-overwrite guard, and the finding write-back each fail a specific test when removed | P7 consumes `IncidentRecord::source`; Security adapter owns the investigator role |
| Spoken reports | Packet G | Implemented | `reports.rs`: a witness walks to the nearest crew member who does not know and tells them aloud. Nine tests, every mechanism falsified — kind filter, confidence floor, proximity gate, hearsay penalty, and the already-knows gate each fail a specific test when removed | Witness interviews (pull), then Security |
| Memory-gated response | Packet G | Implemented | `JobTicket.subject` names what work is *about*; a subject-bearing ticket is answerable only by an agent whose `NpcMemory` holds it. Gates both `select_reference_actions` and `preempt_for_emergency_jobs`, the latter also ranking by a new `Awareness` fact. Falsified: the witness test fails when either mechanism is removed, and asserts the entity tie-break actively opposes awareness so it cannot pass by luck | Gate Security/Botany work as those adapters land |
| Security jobs | Packet H | Not started | None | Adapt patrol and reports |
| Engineering jobs | Packet H | Implemented headless | Eleven tests cover the cast, support staying off the customer roster, four-worker migration, bounded per-asset tickets, exact-fact completion, a shift without claim collisions, forced power faults resolving through repair, stability-scaled fault frequency/severity, and shared ordered-schedule registration. Morrow is now authored in `station.crew.ron`, `Department::members`, and `RESIDENT_NAMES`, so all four workers migrate at runtime. A severe fault now arc-flashes its technician into a separate `Burn` case, falsified in both directions — the gate and the casualty stimulus each fail a specific test when removed | Add fault reporting and Bridge-side coordination |
| Bridge duties | Packet H | Not started | None | Use qualified cross-role tickets |
| Antagonist actions | Packet I | Not started | None | Poison food reference action |
| Illicit player-only supply | Packet I | Planned | Physical player batch required | Classify special reagents |
| Illicit deal incentives | Packet I | Planned | Underworld offers exist | Add required benefit schema |
| Crew menu | Packet K | Implemented | Condition on the rail and the page | Recent-outcomes list |
| Public status projection | Packet K | Implemented | `utility_ai::public`, replicated | None |
| Voluntary department aid | Packet K | Implemented | Handoff, assessment, payout, end to end | Administering an accepted batch |
| Work capacity from chemistry | Packet K | Implemented | `utility_ai::capacity`, status-driven | Executors adopting the reading |
| Persistence/networking | Packet J | Not started | None | Audit save and replication shapes |
| Scale and final tuning | Packet J | Not started | None | Run after full integration |

## Files in flight — read before editing (2026-09-06)

Voice chat is being built in parallel in this same dirty worktree. This section
is the collision surface; it lists what the AI work is actively editing so the
other side can steer clear, and what it has deliberately *not* touched.

**Actively edited by the AI work:**

- `src/crew/mod.rs` — `run_errands` arrival test and the errand deadline. Both
  are shared locomotion, also used by `showdown::run_pursuers`.
- `src/utility_ai/mod.rs` — `select_reference_actions` (occupancy filter, one
  new `Res<ReservationBook>` param) and `begin_reference_actions` (arrival reach).
- `src/utility_ai/medical.rs` — response publishing, the alarm horizon,
  casualty re-announcement.
- `src/utility_ai/decision_log.rs` — the STATION snapshot line.
- `src/npc_motion.rs` — `CLEARANCE` made `pub`. No behaviour change.
- `src/lab/tb_map.rs` and `assets/maps/lab.map` — five `department_spot`
  origins, plus one new map test.
- `src/orders/mod.rs` — one new helper (`is_station_chemical`) and a guard on
  `complete_delivery`'s personal-consumption branch. Nothing else in the file.
- `crates/chem_sim/src/reagent.rs` — one new predicate,
  `Reagent::is_for_the_station_not_a_body`. Additive; no existing method changed.
- `src/net/mod.rs` and `src/settings/mod.rs` — the `--speed` development flag
  (`parse_speed`, `SimulationSpeed`, `apply_simulation_speed`). **Note for the
  voice work:** the `settings/mod.rs` change is one new system plus its
  registration in the plugin's `OnEnter(AppState::Playing)`; it does not touch
  `Knob`, the sliders, or any UI, so it should not collide with `VoiceVolume`/
  `MicGain`.

**Untouched on purpose, and safe to work in:** `src/voice/**`,
`src/ui/bookmarks.rs`, and the Cargo manifests. The AI work has not modified any
of these at any point and has no reason to.

**If you need to change `crew::run_errands`**, say so here first — its arrival
test and deadline were both just changed for reasons that are easy to
accidentally revert, and both have falsification tests
(`a_walk_longer_than_the_deadline_still_finishes`,
`a_worker_can_reach_an_item_resting_on_a_surface`).

## Decision log

### 2026-09-06, the medical round trip, confirmed end to end

The errand-deadline fix was verified in a live run rather than only in tests,
and this is the trace worth keeping. Fresh save, plain `cargo run -- --solo`,
nothing forced:

```
 15s  incident opens in Cargo, Medical ticket published
 25s  ticket claimed
 35s  utility-controlled 28 -> 27   (patient picked up and carried)
 45s  a second Medical ticket appears (the treatment request, not a retry)
 65s  controlled 29, then 70s -> 30 (patient discharged, back under AI control)
 70s  incident closed, and stays closed for the rest of the run
```

The thing to compare it against is run 7, where the same ticket completed three
times at exactly 45.0 s intervals. One completion that stays completed is the
signal; the `utility-controlled` count dipping and recovering is what shows a
body was actually carried and actually given back.

**Why it is always Cargo.** Worth writing down because it looks like a rigged
test and is not. Four departments have a `DepartmentProblemPolicy`, and the
`opening_grace` is counted in *completed jobs*, not seconds. Cargo completes one
roughly every 6-8 s, so it burns through a grace of 6 in about a minute;
Engineering has a grace of 2 but barely completes any jobs, so it never gets
there. The accident is emergent from throughput. `force_next_cargo_burn` exists
for scenario tests and was not used in any of these runs.

### 2026-09-06, the technician who drank the space cleaner

Reported twice from live play: *"i gave a tech guy space cleaner and he drank it
himself instead of using it where he needed it."*

`complete_delivery` has two branches. A **linked** order (one carrying an
`OrderUse`) transfers custody, walks the carrier to a `use_destination`, and
applies an explicit `Route`. Everything else falls to the
personal-consumption branch, which hardcoded `Route::Ingested`.

The bug is that **`OrderUse::medical` is the only constructor in the game**, so
Medical is the only department that ever attaches one. Every other request —
including a cleaning request — landed in a branch that assumes every delivery
ends in somebody's stomach. Space cleaner's own reference entry reads *"Nothing.
The janitor will thank you. Do not drink it."*

Fixed with `Reagent::is_for_the_station_not_a_body`: a reagent that has
`world_effects` and no *beneficial* body effect is station supplies, and is
handed over without being swallowed.

Two decisions inside that predicate are load-bearing:

- **Structural, not categorical.** It does not test `Category::Utility`, which
  is a reference-book heading an author picks. It tests the effects the reagent
  actually carries. The case that proves the difference is `firefighting_foam`:
  it is authored `Utility` and expands over a fire like the other foams, but it
  also carries `Counter(Burning)`, so it is a real treatment for a burning crew
  member and must stay deliverable. A category check would have blocked it.
- **Majority by volume, not "contains any".** A medicine carrying a trace of
  cleaner is still a medicine and is still taken. Filling an order with the
  wrong *medicine* remains the player's mistake to make, with all its
  consequences; a cleaner is not a wrong medicine, it is not medicine.

Falsified: removing the guard makes
`nobody_drinks_the_space_cleaner_they_were_handed` fail with the exact live
symptom, while `a_wrong_medicine_is_still_swallowed` keeps passing — which is
what proves the guard is narrow rather than just switching deliveries off.

**Left open deliberately:** the technician still does not *use* the cleaner on
the spill. Every `Route` in `chem_sim` is a way into a body; there is no "applied
it to the floor" route, and no non-Medical department builds an `OrderUse` with
a destination. Giving Service and Engineering a linked-use path is its own
packet. This change only stops the harm.

### 2026-09-06, a development flag for the clock

Added `--speed <N>`, because the triage loop was bottlenecked on wall-clock
time: a medical response plus transport is a ~150 s round trip, which is a long
time to sit and watch for one data point.

It scales `Time<Virtual>` — the same clock the pause menu already stops — so it
moves everything together and no system needs to know about it. Three
constraints, each with a test:

- **Gated on `owns_the_clock`**, the identical rule pausing uses. Running the
  clock fast from one end of a co-op session would desync a peer who never
  agreed to it, so `Host`/`HostSteam` ignore the flag.
- **Clamped to 10x.** Not a limit on the simulation but on the *step size*: at
  higher multipliers one frame advances far enough that a walker can step past a
  waypoint or through a body-spacing check, and the bugs that produces are
  artefacts of the flag rather than of the game.
- **Parsed separately from `LaunchMode::parse_args`**, which returns on the
  first flag it recognizes — a speed parsed there would be silently dropped for
  the realistic invocation `--solo --speed 4`. That is exactly what
  `a_speed_flag_is_read_from_anywhere_on_the_command_line` pins.

Absent flag means the resource is absent and the system is inert, so a shipped
build carries no multiplier.

### 2026-09-06, the transport that could never arrive

Four defects, all found by running the game and reading the trace, all in the
same family: **something was measured in a way the station's own movement rules
make unsatisfiable.**

1. **You cannot walk to a person.** `npc_motion` holds bodies 0.72 m apart;
   arrival wanted 0.3 m. Any `ActionTarget::Entity` naming a crew body was
   unreachable by construction. Fixed with `AT_BODY_DISTANCE`.
2. **You cannot walk to a shared spot.** Six spots are authored at capacity 2 —
   both Bridge duty stations, both Security ones, the Service host table and the
   lounge — and every claimant is sent to the *same coordinate*. Only the first
   can stand on it. In the trace this showed up as `Socialize` failing every
   21-25 s, cycling through Alvarez, Odera and Imani. Same fix, same constant.
3. **You cannot walk to anything not at body height.** Arrival was a *3D* test,
   but a body cannot change its own y — locomotion steps horizontally and
   containment rewrites y every frame. Botany drops produce at y 0.08; a
   walker's origin is 0.93. That 0.85 m gap against a 0.3 m radius made **every**
   Service ingredient pickup permanently unreachable — one trace has Cook
   Navarro failing the same ticket ten times and Attendant Mensah another nine.
   Arrival is now horizontal plus a same-deck band, reusing the 1.2 m
   `npc_motion::sweeps_body` already uses rather than inventing a fourth opinion
   about deck separation.
4. **You cannot walk far.** `ERRAND_DEADLINE_SECONDS` was a stopwatch, which on
   a station this size is a cap on *distance*: `WALK_SPEED * 45` is 94 m, and a
   Cargo casualty is 50 m from a Medical bed before the route bends around a
   single room. So Medical transports across the station could not finish. The
   trace is unambiguous — the response ticket completing three separate times,
   each re-published exactly 45.0 s after the last, while the patient never
   reached a bed. The player's version was "they never walk into the medical
   room and go to a bed; they eventually just solve the problem and walk away",
   which is the patient healing unaided while the pipeline restarts around them.

   The deadline is now a **no-progress timer**: getting genuinely closer resets
   it. That matches the constant's own documented intent — it names "a goal
   drifting away as fast as it is chased" and "a route that leads somewhere the
   walker can never quite reach", and both are failures to *advance*, not
   failures to be quick. Both still time out, in the same 45 s.

   The first attempt at this scaled the budget by planned route length instead,
   and `an_errand_that_never_gets_there_is_written_off_rather_than_run_forever`
   correctly failed it: a distant *impossible* goal would have been handed an
   unlimited budget. Worth recording as the reason the progress formulation is
   the right one rather than the convenient one.

Incidents now resolve on their own in a 640-second run, `ReservationUnavailable`
is down from 1,983 to 2, and `Unreachable` from 45 to 28. **None of that proves
treatment works** — the player's observation is the counterweight, and it says
patients still are not reaching beds. Verify that specifically before believing
the incident counter.

### 2026-09-06, the doorway finding was wrong — read this before trusting it

**Retracted.** An earlier version of this entry claimed every doorway was 0.64 m
and too narrow for a 0.70 m body, and proposed widening all 35. That was wrong
and the widening was applied and then reverted; `lab.map` is byte-identical to
before it.

The error: `TB_SCALE` is **40.0**, not 100. I took "~35 units (the nav radius
inset)" from a map comment as meaning 100 units = 1 m. Doorways are **2.00 m**
(28 department) and **1.85 m** (7 lab). Two bodies need 1.42 m to pass abreast,
so they fit comfortably. Nothing is wrong with the doorway widths.

Caught by the map test suite — widening put a hardsuit locker inside a walkable
volume — which is the argument for those tests existing at all. Verify the scale
against `origin_xz` (`-y/TB_SCALE`, `-x/TB_SCALE`) before doing map arithmetic;
`door.chemistry.public` should land on world (4.00, 7.10), and its own test
asserts exactly that.

What *was* real in the player's report is recorded under the department
gathering points below.

### 2026-09-06, department gathering points sat in their own doorways

Reported from play as "the place where the medical people gather is right inside
the door, so people are getting stuck", and this half held up.

`crew::Departments::home` is the **single** point everyone of a role walks back
to, and `somewhere_else` sends idle crew visiting another department two thirds
of the time — so one spot absorbs a department's entire floating population.
Five of eight sat exactly **1.50 m** inside their primary door: Medical,
Security, Bridge, Cargo, Service. A doorway is 1.6 m deep, so that is barely
past its inner mouth, and with bodies held 0.72 m apart a few arrivals fill the
opening and everyone behind wedges against it.

Engineering (5.40 m) and Botany (5.00 m) were already clear, and are where the
fix's 5.00 m comes from — the authored norm, not a new invention. Five origins
moved, 3.5-3.8 m each, along each door's own axis so they stay in the same part
of the room. `a_department_gathering_point_is_clear_of_every_doorway` pins it,
checked against *every* door rather than the department's own, since a point
nudged clear of its front door and into a maintenance one is the same bug.

**Still open, and the deeper version:** one arrival point per department means
crowding is inherent wherever that point sits. Moving it out of the doorway
moved the pile, it did not disperse it. A small per-agent offset on arrival
would, and the player's Botany screenshot — three botanists overlapping in one
clump — is what it looks like unfixed.

### 2026-09-06, a hypothesis that did not survive its own test

Recorded because the process is the point, and because the wrong answer was
plausible enough to have shipped.

Medical's transport leg times out at exactly `ERRAND_DEADLINE_SECONDS`, giving a
perfectly regular 92-second cycle: response completes, transport walks nowhere
for 45 seconds, the case resets to `AwaitingResponder`, the ticket republishes.
The Cargo incident behind it reached 599 seconds and was still climbing.

The hypothesis was that `follow_medical_transport` holds the passenger 0.63 m
from the carrier — inside `CLEARANCE` — so the carrier is blocked by the person
they are holding. A `CarriedBody` marker excluding passengers from the crowd
snapshot was written, wired through a derived sync system, and tested.

**The test would not falsify.** Removing the exclusion changed nothing, in an
open room *or* in a 1.2 m corridor built specifically to remove the sidestep
escape. The reason is a handedness error in the original reasoning:
`Quat::from_rotation_y(heading.x.atan2(heading.z))` maps the local offset
`(-0.6, 0, -0.2)` to 0.2 m *behind* the carrier along the direction of travel,
not ahead. Every forward step therefore increases separation and is allowed.

The change was reverted. What survives is the corridor test, reframed as a
regression guard on the geometric fact that actually matters — the passenger
must trail the carrier — since putting them in front would jam every transport
with no error anywhere. The real cause of the timeout is still open, and the
doorway finding above is the leading candidate: the route into Medical passes
through a 0.64 m door with staff posted 1.0-1.4 m inside it.

### 2026-09-06, the immortal incident

The first bug the decision log caught in real play, reported from the game as
"it said emergency on Cargo and when I went there I didn't see anything
abnormal."

- **The chain.** A Cargo worker is burned, Medical publishes a response ticket
  carrying `subject: Some(patient)`, and `perception::may_respond_to` refuses
  any worker who has not witnessed that body. Nobody saw the collapse, so no
  Medical worker could ever claim the ticket, so the patient was never treated,
  never discharged, and the incident behind them never resolved. The department
  read `Emergency` permanently with nothing at the scene to find. The log showed
  it plainly: `Medical 1/0` held from 40s to 80s while all four Medical staff
  sat on `IdleObserve`.
- **It cost more than a wrong label.** Grepping the resolve sites shows that
  *every* casualty incident in the game — Cargo `Burn`, Botany `Poisoning`,
  Engineering `Burn` — resolves only through Medical discharging the patient.
  Only Engineering's `EquipmentFailure` resolves inside its own adapter. And
  `DepartmentProblemPolicy.unresolved_cap` suppresses new problems while one is
  active — `cargo_pilot.rs` has a test asserting exactly that. So one
  unwitnessed collapse permanently ended that department's ability to generate
  any workplace problem at all. Three of the four problem-producing departments
  were one unlucky collapse away from going quiet for the rest of the session.
- **The gate was mine, and it was right; it just had no floor.** P4 introduced
  the subject gate so help arrives because someone *saw* it, not because the
  ledger knew — that stays. What was missing is that perception should decide
  how **fast** help arrives, not **whether** it ever does. Two independent
  routes now guarantee that, and both are tested separately so neither is
  carrying the other:
  - **A body on the floor stays perceivable.** The casualty stimulus was emitted
    exactly once, at the instant the case opened, which made "was somebody
    standing here on that precise tick" the whole question. A collapse is a
    standing fact about a room, not a one-frame noise, so
    `reannounce_unattended_casualties` re-emits it every
    `CASUALTY_REANNOUNCE_SECONDS` from the patient's current position while the
    case still awaits a responder. `NpcMemory::remember` merges repeats, so this
    refreshes certainty rather than filling memories with copies of one body.
  - **An unanswered alarm stops being private.** Past
    `UNANSWERED_ALARM_SECONDS` the response ticket is republished with
    `subject: None`, which the existing contract already defines as station work
    anyone may take. The horizon is long enough that a real witness still gets
    to be the one who answers, which is the behaviour the gate exists to
    produce.
- **Rewriting a ticket means cancel-and-publish, so it needs a guard.** Doing
  that to a *claimed* ticket would cancel the very rescue the escalation exists
  to cause, and would do it at exactly the moment a slow responder was finally
  arriving. Only `Available` tickets are rewritten. Falsified: removing the arm
  fails `the_alarm_horizon_leaves_a_ticket_a_responder_already_holds_alone` and
  nothing else.
- **Test-sequencing trap worth recording.** The escalation test first failed
  looking like the fix was broken — the second assertion found no ticket at all.
  The fix had in fact worked, and worked *fast*: a single ten-second advance
  sailed straight past the state under test, because the responder claimed,
  walked, performed and began transport inside it, which removes the ticket. The
  assertion now steps in 0.1s frames and catches the flip as it happens. A test
  that jumps time can fail because the behaviour is too quick, not too slow.
- **The fix worked, and the trace immediately showed the next link failing.**
  With the horizon in, Medical claimed the ticket for the first time
  (`Medical 1/0` → `0/1` at the 45s mark). Then: `DONE!Dr. Vance
  PerformJob/… = Unreachable`, Okonkwo claims the same ticket, 45.003 seconds
  later `DONE!Nurse Okonkwo … = Unreachable`, Vance claims it again. Two
  responders, in turn, forever. The incident was still immortal, for a
  completely different reason, and only a second trace showed it.
- **Walking to a person was impossible by construction.** `npc_motion`'s
  `sweeps_body` refuses any step whose swept segment passes within
  `CLEARANCE` (`BODY_RADIUS * 2 + 0.02` = 0.72 m) of another body, while
  arrival required `AT_TARGET_DISTANCE` = 0.3 m in 3D. **0.72 > 0.3**, so *any*
  `ActionTarget::Entity` naming a crew body was unreachable — not sometimes,
  not depending on geometry, always. The 45-second gaps in the log are the
  `ERRAND_DEADLINE_SECONDS` timeout, not a routing failure. Fixed in
  `begin_reference_actions` with `AT_BODY_DISTANCE = CLEARANCE +
  AT_TARGET_DISTANCE`, chosen from the target itself (`Has<CrewMember>`) rather
  than declared per ticket, so no future adapter can publish work at a distance
  the station's own movement rules forbid. `CLEARANCE` is now `pub` because it
  is a physical fact about the station, not a private tuning number.
- **Why 1,549 tests never saw it: `NpcMotion` is an `Option<ResMut<…>>`.** Every
  harness that walked someone to a person omitted the resource, which models a
  station where people can stand inside one another. This is the same class of
  harness omission as the P4 `witness_stimuli` finding, and it is worth stating
  as a rule: *an optional resource left out of a test harness is a silently
  different physics, not a smaller one.* The new kernel test inserts `NpcMotion`
  and carries a second assertion that the two bodies really did end up
  `CLEARANCE` apart, so it cannot pass by having spacing switched off.
  Falsified: forcing the body case back to `AT_TARGET_DISTANCE` reproduces the
  live symptom exactly — `Some(Unreachable)`.
- **`JobTicket.deadline` is written by four adapters and read by nothing.** The
  medical response ticket sets a 60s deadline that has never expired anything.
  Left alone deliberately — a board-wide expiry sweep is its own packet and
  would have hidden this bug behind a slower one — but it is a live trap for the
  next adapter that assumes deadlines work.

### 2026-09-05, P8 lands five of seven and names the block

- **A latent save-ordering bug, found by asking rather than assuming.**
  `load_progress` and the eleven `OnEnter(Playing)` reset systems were all
  unordered against each other. A probe showed the reset happened to run first
  today — purely because `UtilityAiPlugin` is registered above `ProgressPlugin`
  in `main.rs`. Reordering those two lines would have started silently
  discarding restored custody and donations with no error anywhere. This
  affected the *already-shipped* `illicit_custody` persistence, not just the new
  aid state. Fixed with a `UtilityResetSet` the eleven resets join and
  `load_progress` explicitly orders after.
- **The first version of that test proved nothing.** It asserted on a *copy* of
  the ordering edge, so deleting the real `.after(...)` from `ProgressPlugin`
  left it green. The replacement reads the built schedule and fails when the
  real edge is removed.
- **`loading_twice_does_not_double_a_departments_stock` also proved nothing.**
  Removing `*self = default()` from `restore` broke no test, because the
  per-department `occupied` map already refused a second batch. The clearing
  matters for a different case — a department present in the live session but
  absent from the save — and the rewritten test fails without it.
- **The scale sims were wrong three times before they were right.** First they
  never advanced `Time`, so the staggered `phase_offset_millis` clocks never
  fired and exactly one worker ever decided. Then they stopped at `Select`,
  which only *proposes* — the claim and reservation happen in `BeginAction`, so
  every worker appeared to hold whatever it fancied. Then they read ownership
  from `CurrentAction` rather than the board, which reports a refused claim as
  a held one. Reading the board is what made "thirty workers, ten tickets, at
  most ten owners" a real assertion.
- **And they are documented for what they actually prove.** Every individual
  guard they lean on — the compare-and-swap claim, reservation capacity, the
  rollback on a failed claim — was falsified, and in each case an *existing
  unit test* caught it, not the sims. So the sims are written up as cover for
  the composite behaviour at population, not as a second copy of the unit
  tests. The rollback in `begin_reference_actions` turns out to be redundant
  with `resolve_reference_actions`' release-by-owner; that is recorded rather
  than removed, because defence in depth on a reservation leak is cheap.
- **The `Ambient` removal is blocked, and the block was re-verified.**
  `crew::fluff`, `cult`, and `shift::restock` all still spawn it in production
  and none of that cast has a `NpcJobProfile`. Deleting the component would
  leave them with no decision system and they would stand still forever.
- **`docs/npc-ai.md` opened with a false sentence** — "there is no behaviour
  tree, no utility scoring, and no state machine anywhere in the codebase" —
  which had been true when written and was now the opposite of the truth. It
  now leads with the two-decision-system split, and its "No perception layer"
  gap is corrected to note that the utility roster has one and the `Ambient`
  cast still does not.

### 2026-09-05, assessment and payout close P3B

- **The verdict is computed at resolution, not at publication.** A worker who
  reaches a batch reads the department's condition and standing *as they are
  when they look at the container*, so a department that got desperate while
  the batch sat there decides differently. Computing it when the ticket was
  published would have frozen the one variable the system exists to vary.
- **`assess_offer` deliberately cannot see the contents.** It takes the claimed
  label and nothing else about what is in the container. Passing the `Solution`
  in would have collapsed the whole deception layer into "NPCs can smell
  poison". The one apparent exception, `known_contraband`, is not chemistry: it
  is whether the *claim itself* names something forbidden, matched against the
  same `Category::Illicit` / `controlled` / `explosive` rule Security's sweep
  uses. "Meth" written on a jar of water is quarantined; a jar of meth labelled
  "saline" sails through. That asymmetry is the design.
- **Pressure overriding caution is the lever.** The same person offering the
  same unmarked container is refused by a `Busy` department and accepted by a
  `Strained` one. `a_strained_department_accepts_what_a_calm_one_would_refuse`
  is what stops that from quietly regressing into a constant.
- **`assessment_capability` had to be a `const fn` returning `&'static str`.**
  Rosters declare capabilities as `&'static [&str]`, so the runtime-formatted
  `String` I wrote first could never have been added to one — every department
  would have published assessment work that nobody in the station was qualified
  to claim, silently, forever.
- **Payout reads state, not messages.** Outcomes arrive by several routes — a
  worker's verdict, an inspection clearing a quarantine, a body reacting later
  — and every one of them ends in the batch simply being in a new state. So the
  system sweeps for settled-and-unpaid batches, and `claim_payout` is what makes
  it exactly once. A cleared quarantine deliberately does *not* refund, or a
  player could farm standing by donating contraband and waiting.
- **A falsification that proved nothing, caught.** Removing the `is_err()` check
  around `claim_payout` left every test green, because the call still sets the
  flag. The mechanism only shows up when the flag is bypassed entirely; redone
  that way, two tests failed. A guard whose removal changes nothing is not a
  guard.
- **Integration testing found a real one.** The end-to-end test initially tried
  to store an *unmarked* donation — which the department had correctly rejected,
  and a rejected batch is returned or disposed of, never stored. The unit tests
  all passed; only running the whole chain surfaced it.
- **The selector link is now tested, not assumed.** The kernel turns a routine
  ticket into a candidate purely through `available_to` — domain plus capability
  — with no warning if nothing matches. Deleting Medical's assessment capability
  fails three tests; before those existed it would have failed nothing and left
  the batch sitting in the intake forever.

### 2026-09-05, the public projection and the aid handoff open P3B

- **The menu had to be built so it *cannot* leak, not merely so it doesn't.**
  `utility_ai::public` is the only thing the Crew menu reads, and it holds
  nothing but replicated components. That is why
  `a_guest_can_render_the_menu_from_replicated_state_alone` builds a world with
  neither `JobBoard` nor `IncidentLedger` and still renders: a future field
  that needs an authority resource to interpret cannot compile into it.
- **A status with no evidence behind it is a lie the player cannot check.**
  `DepartmentCondition` is derived only from counts a player could take
  themselves — tickets waiting, workers standing up, incidents open — and
  `reason_is_backed_by_a_countable_world_fact` sweeps the whole input space to
  prove nothing worse than `OnSchedule` is ever shown without a countable fact.
  The reason is rendered *beside* the label rather than in a tooltip, because a
  label whose evidence is hidden behind a hover is the same "trust me" status.
- **The reason is an enum, not a string.** Prose could smuggle a private number
  out inside it. An enum with an explicit `evidence_count` cannot.
- **Claim and content are stored side by side and never reconciled.** Staff read
  the label; bodies and processes get the `Solution`. This is what lets a
  trusted player talk a strained department into accepting something dangerous
  while trust still changes nothing about the chemistry.
- **Aid is deliberately not a delivery.** `OfferAidRequested` is its own client
  message rather than a reuse of the interact press, so one keypress can never
  both close an order and donate the same container. The container is despawned
  at handoff, which is what makes a second offer of the same beaker impossible.
- **One intake slot per department is the concurrency guard.** The intake is a
  real counter with a real container on it, so a second same-frame offer has
  nowhere to go and is refused rather than queued.
- **Chemistry reaches work only through metabolism.** `utility_ai::capacity` is
  the single shared `work_capacity`/`work_risk` reading, and it sees statuses,
  never reagent IDs. A stimulant helps because it produced `Hastened`; anything
  else producing `Hastened` helps identically, and a donated "stimulant" that
  metabolizes to nothing is inert. Capacity and risk are *not* inverses — both
  rise together under `Hastened`, which is the trade the whole system exists to
  offer.
- **A donation is witnessed, not announced.** Every handoff emits
  `SuspiciousHandling` — the same kind for an honest donation and a covert one.
  A witness learns that a container changed hands, never what was in it.
  Splitting the kinds would have made the memory itself a contraband detector.
- **Two dead-field clippy warnings were design tells, again.** `AidOffered`
  carried a batch id and a domain nothing read. Rather than silence it, the
  message was cut down to what a witness could actually have registered — who,
  and where. Anything needing the batch reads `AidIntakes`, which is
  authority-only by construction.
- **My own map test was weaker than the one already in the repo.** I wrote a
  proximity heuristic for intake placement; `tb_map`'s existing
  `utility_spots_are_unique_walkable_and_routable` checks real walkable regions
  and nav routing, and caught a Bridge intake sitting off the floor that my
  heuristic passed. The heuristic was deleted and the spots added to the real
  check instead.

### 2026-09-05, reporting closes P4

- **The station was omniscient about casualties.** `handle_crew_collapse`
  created an incident the moment a body hit the floor, seen or not, and
  `open_cases_from_incidents` then emitted the casualty stimulus *after* the
  case already existed. So perception was decorative on the one event it most
  needed to matter. The collapse now emits its stimulus where and when it
  happens, and a witness who saw it can file it.
- **Filing is a separate message from telling.** `deliver_reports` already
  spread knowledge crew-to-crew; `CasualtyReported` is what turns that into a
  case. Two different questions — spreading what is known, and committing
  Medical to a patient — so folding them together would have made every
  overheard rumour a work order.
- **Three gates, each a real failure mode.** A reporter cannot be their own
  subject (a body on the floor is not walking over to say so, and allowing it
  would restore the omniscience through the new path). Confidence must still
  clear `CONFIDENT_ENOUGH_TO_FILE` at *arrival*, above `WORTH_REPORTING`, so a
  half-glimpse is worth mentioning but not worth a patient. And the subject must
  still be down — the walk takes time, so a stale report about someone who got
  up is the ordinary case, not an edge case.
- **Diagnosis stays Medical's.** The witness says "someone's down"; the kind
  and severity are read from the reported body's actual damage. A station where
  the reporter's claim set the diagnosis would let a mistaken bystander book a
  burn ward for a poisoning.
- **The reporter is never the incident's `source`.** `interviews` reads exactly
  that field when deciding who to name, so recording the person who raised the
  alarm would make doing the right thing look like guilt.
- **Falsification found an unguarded case again.** Removing the still-down check
  left every test green until
  `a_report_opens_a_case_only_while_the_subject_is_actually_down` existed. Two
  clippy dead-field warnings on `reporter` and `confidence` were the tell that
  the design had carried fields nothing used — the fix was to make them
  load-bearing rather than to silence the warning.

### 2026-09-05, Engineering hurts people

- **The injury is a separate `Burn` incident, not damage folded into the
  `EquipmentFailure`.** The two need different responders and resolve
  independently: the asset wants a technician with repair capability, the
  person wants Medical and a bed. Merging them would mean repairing the pump
  discharged the patient.
- **Gated on the fault's own severity, not a second roll.** Most faults are a
  breaker tripping — a repair job, not a casualty. Deriving the gate from the
  severity the fault already carries means a deteriorating station produces
  more injuries on its own, with no independent randomness to tune against the
  first. `a_severe_engineering_fault_burns_its_technician_and_a_mild_one_does_
  not` pins both halves; without the second, every routine trip would fill
  Medical and the ordinary case would stop reading as ordinary.
- **Burn rather than brute.** These are powered assets, so an arc flash is the
  honest injury — and it routes into the exact standard burn care Medical
  already implements for the Cargo reference case instead of needing a new
  treatment branch. Damage uses whole `Units` on the same scale as
  `cargo_burn_damage_from_health` so the two departments stay comparable.
- **A hurt technician emits `Casualty`, not `Hazard`.** The fault already emits
  a hazard about the machine; a body on the floor is a different, louder event,
  and it is the one that brings Medical rather than another engineer.
- **P8 was checked first and is genuinely blocked.** Removing the legacy
  `Ambient` path is still gated on the last resident migrating, and it is not
  close: `Ambient` currently drives the whole off-roster cast — Service,
  Security and Bridge support crew, cult guards, restock couriers. Deleting it
  now would strand them. The plan's ordering is correct and the guard test
  (`legacy ambient selected a destination for a utility-controlled resident`)
  already stops the two paths competing in the meantime.

### 2026-09-05, authored tuning closes P5

- **A test that changes the file at runtime is the only one that matters.**
  Every existing social test passed against hardcoded constants that *happened
  to match* the shipped values — proved by falsification: replacing
  `tuning.thresholds.tired_enough_to_rest` with the literal `0.45` left all
  nine green. Moving numbers into RON is worthless if nothing checks they are
  consulted, so `raising_the_authored_threshold_changes_who_takes_a_break`
  raises the threshold at runtime and requires the break to stop happening. It
  is the single test that fails if anyone re-inlines a constant.
- **Validation is about coherence, not taste.** Any playable numbers are
  allowed; combinations that guarantee nonsense are refused — a rate that never
  builds makes its action unreachable, a zero-length action completes the frame
  it begins (skipping the walk, the reservation, and any chance of being
  witnessed), and a `starting` need already past its own threshold opens the
  shift with everyone walking off the job. Those cross-field checks are the
  ones a per-value range test cannot make.
- **`NeedsTuning::default` and `NpcNeeds::default` both defer to the file.** A
  second set of numbers in Rust would be a silent fallback able to drift from
  the RON and mask a bad edit, which is exactly what this change exists to stop.
- **Parsed once, behind a `OnceLock`.** `NpcNeeds::default` runs on every crew
  spawn; re-reading an `include_str!` file per arrival would turn a startup cost
  into a per-arrival one for no benefit.
- **Chemistry and geometry stay out.** `social_disposition` and
  `fit_for_leisure` read `Bloodstream` directly — a "sedated multiplier" in this
  file would be a second mood model free to disagree with the first. Seat
  capacities stay in `lab.map`, because they are geometry rather than balance.
- **One stale checkbox corrected.** Botany-to-Service consumption was marked
  unfinished but had landed with the Service adapter: the intake query
  *requires* `BotanyProduceProvenance`, so only real Botany output can enter a
  meal. Verified against the passing test before ticking it, not assumed.

### 2026-09-05, pricing covert harm

- **The act is not the harm.** `price_covert_harm` fires on `IncidentCreated`,
  not on the poisoning action. A contaminated meal nobody eats destabilises
  nothing: the antagonist took a risk and got away with it, which is a real
  outcome and must not be indistinguishable from a casualty. It is also what
  keeps the player's recovery window meaningful — confiscating the batch or
  clearing the meal genuinely prevents the consequence rather than merely
  delaying a number that already moved.
- **`TamperedMeals` carries provenance from bowl to body.** The contaminant
  becomes ordinary `Solution` on purpose, so by the time someone collapses
  there is nothing in the chemistry saying a person chose it. The ledger is
  authority-only ground truth and is explicitly **not evidence**: nothing reads
  it to decide what Security knows, which still comes from witnesses via
  `interviews`. It exists so consequences can be priced honestly and so an
  investigation that independently names the culprit is confirming something
  rather than inventing it.
- **`StabilityEvent::HostileSucceeded` finally has an emitter.** It was priced
  and unit-tested but never constructed — clippy had been reporting it as dead
  code. Wiring it dropped the warning count *below* baseline, which is decent
  evidence the variant was built for exactly this and not something new bolted
  on beside it.
- **Severity comes from the victim, not the dose.** What destabilises a station
  is how badly someone was hurt; a large dose that barely landed should not
  read as a catastrophe.
- **The culprit's own standing does not move.** Only the victim's department
  loses confidence — from the inside this reads as a department that cannot
  keep its people safe. Moving the culprit's number would leak private guilt
  into a public one before anybody has worked it out.
- **A falsification found a missing test.** Deleting the `IncidentKind ==
  Poisoning` check left all tests green: someone who ate a tampered meal and
  later suffered an unrelated burn would have had that burn priced as covert
  harm and attributed to the antagonist — inventing both a crime and a culprit
  from a coincidence. `an_unrelated_injury_to_someone_who_ate_a_tampered_meal_
  is_not_covert_harm` now fails 2-vs-0 without the check.

### 2026-09-05, favors that actually pay out

- **A favor is a banked counter on `Requisition`, beside the wards bought at
  the standing board.** That was already the codebase's pattern for "owed
  something, spent later": a counter, one named site that absorbs exactly one,
  and a test asserting it is spent rather than banked indefinitely. Inventing a
  parallel mechanism for deal favors would have been a second answer to a
  question already settled.
- **`quiet_access_favors` is a separate counter from `raid_wards`.** Both
  absorb the same raid, but one was paid for at the shop and the other was
  earned by dealing; collapsing them would let a balance change to the shop
  silently reprice the underworld. `schedule_raid` spends the *bought* ward
  first, so a player holding both keeps the favor they are still owed.
- **`AdvanceWarning` is one system reacting to `IncidentCreated`,** not a check
  inside each of the four adapters that raise incidents. The favor is about the
  player being told, not about which department had the accident, and four call
  sites would give it four chances to drift. It fires *after* the incident on
  purpose: what it buys is hearing from a friend before it reaches the board,
  not precognition — anything earlier would need the director to consult a
  career resource before deciding.
- **`ExpeditedFreight` is spent at the check, not the delivery,** so it cannot
  be banked against a restock that was already due anyway.
- **A favor is never scaled.** A partial or dishonest deal still owes a whole
  favor or none: "0.35 of an expedited freight run" is not a thing, unlike
  standing, which divides fine.
- **The three deceptions are three buttons, not one with a sub-menu.** They
  differ in what physically changes hands and in how each is caught — dilution
  in the dose, substitution in the chemistry, a marker only if someone looks.
  Hiding that behind a single "lie" option would conceal the only decision that
  matters. Lying is also always offered, because a character cannot stop the
  player handing over something other than what they promised.

### 2026-09-05, the approach conversation

- **`PublicApproach` replicates; everything around it does not.** The terms and
  the offered answers were said to the player's face, so a client may draw
  them. What stays authority-only is whether the requester has a `CovertGoal`,
  what is in custody, and how a covert action scores — so a client can render
  the buttons without being able to infer who is dangerous. An approach means
  someone asked for something, not that they intend harm.
- **The player's answer is a stance, not a commitment.** `LiveApproach.answered`
  records what was said; the *delivery* is what makes it real. Saying "agree"
  and never delivering is therefore the same as delaying, which is correct.
  Delivering after saying "no" is read as a change of mind rather than trapping
  the goods, so a non-supplying stance falls back to `Cooperate` at the handover.
- **`None` is not `Delay`.** Not having answered yet is a different state from
  having chosen to stall, and only the latter is something the requester heard.
- **The panel never labels anyone illicit.** It states the terms and lets the
  player judge. A "criminal" tag in the Crew menu would hand over an answer the
  station has not earned, which is the same rule the interviews system follows.
- **A test that proved nothing, caught by falsification.**
  `asking_again_continues_the_same_approach_rather_than_opening_a_second`
  originally asserted only on `answered` — and passed with the idempotence
  guard deleted, because `get`, `answer` and `settle` all find the *first*
  match, making a duplicate invisible. `count_for` exists so the test can
  actually count; it now fails 2-vs-1.
- **The authority re-validates every answer.** `LiveApproaches::answer` rejects
  a response the requester does not offer rather than downgrading it, so a
  stale button or a hand-crafted message commits nothing instead of closing a
  deal that was never on the table.

### 2026-09-05, custody persistence

- **Keyed by holder name, not `Entity`.** `addiction` already documents why:
  crew entities are despawned when they walk out, and a batch outlives the
  visit — an entity id is meaningless after a reload and wrong even before one.
  `CustodyNames` keeps the mapping current from the live roster, so a
  respawned NPC is still recognised as the same person.
- **`restore` replaces rather than appends.** The plan asks for save/load
  "without duplication", and appending would hand the antagonist a second dose
  to anyone who reloaded twice. Falsified: appending fails the round-trip test
  2-vs-1.
- **Terminal states are never written.** A consumed, confiscated, destroyed or
  returned batch has already had its effect. Saving one would let a player who
  confiscated something find it back in the same NPC's hands after a reload,
  which quietly undoes the recovery path P7 just built. Falsified separately.
- **`BotanistProgress` persists alongside it.** Custody alone was not enough:
  without the `supplied` flag a reload re-arms the final ask, so one delivery
  could stock the antagonist twice. The two are bundled in one `SystemParam`
  because they are only correct together.
- **`SaveSlot` moved into that bundle.** Both `persist_progress` and
  `load_progress` were already at Bevy's 16-parameter ceiling. Exceeding it on
  `load_progress` fails as a confusing "cannot become an `ObserverSystem`"
  trait error rather than an arity message — worth knowing before the next
  field is added there.
- **`source_player` is deliberately not restored.** The supplying player may
  not be connected, and a stale entity id would attribute the batch to whoever
  now holds that id.

### 2026-09-05, Service emits `Food`

- **All five stimulus kinds now have real sources.** `apply_service_job_results`
  emits `Food` at the `ServeMeal` site, where a batch physically arrives at the
  serving spot and its position is already computed.
- **The stimulus deliberately carries no contents claim.** A clean meal and one
  `covert.rs` has contaminated produce a byte-identical `Food` stimulus, because
  the contamination path never touches this site. Adding a purity or ingredient
  field would silently hand every bystander an answer nobody in the room has,
  and would let a witness shortcut the Medical case that is supposed to be how
  harm becomes discoverable. A test asserts the emitted stimuli carry only
  subject and full strength.
- **Asserted inside the real pipeline, not hand-written.** An earlier wave
  shipped a hazard-emission test that wrote its own `Stimulus` and therefore
  proved only that the message bus worked. The new assertion lives in
  `four_workers_run_the_bounded_physical_ingredient_to_cleanup_pipeline` and
  reads what the production path actually emitted; removing the emission fails
  it 0-vs-4.
- **A `Resolve`-set emission is read on the next frame's `Observe`, exactly
  once.** The write always misses the current frame's read, so this had to be
  measured rather than reasoned about:
  `a_stimulus_emitted_after_the_observe_set_is_witnessed_next_frame` counts
  actual reads and pins both halves — the stimulus is delivered on the following
  frame, and it is *not* re-read on any later one. The once-only property comes
  from the reader's cursor, not from buffer expiry, so an idle frame between the
  write and the read does not discard it.
- **The first version of that test proved nothing, twice.** It asserted
  `knows_about`, but a formed memory persists across frames, so it could not
  tell "witnessed on frame two" from "witnessed on frame five" — inserting extra
  frame swaps left it green. Counting reads fixed it. The re-read half matters
  on its own: a stimulus left readable would be re-witnessed every frame, and a
  single served meal would keep refreshing every bystander's memory for as long
  as the buffer held, quietly turning a one-off event into a permanent one.

### 2026-09-05, player deal responses and the upside rule

- **The upside rule is a data-validation failure, not a review checklist item.**
  `IllicitRequest::validate` rejects a request with no benefit, and
  `BotanistPlugin` calls it at load, so a deal authored as pure delayed
  punishment fails to start the game. A rule that only lived in a design
  document would be obeyed by whoever read it and by nobody else.
- **The zero-value loophole is checked separately.** `benefits.is_empty()` alone
  would accept `Standing { amount: 0 }` — a benefit that satisfies the letter of
  the rule and none of its point. `PlayerBenefit::is_concrete` is the second
  gate, and `scaled_amount` refuses to round a positive benefit down to nothing,
  so a small reward survives the 0.35 deception multiplier instead of silently
  vanishing.
- **Independence is structural, not asserted.** `grant` takes the request, the
  response, and the two standing meters — it cannot reach custody, goals, or
  covert scoring because they are not arguments. The botanist handler calls it
  *before* the embodiment check and before any custody or goal write, so the
  player is paid even if Aleksy is unspawned. Ordering it after any of those
  would quietly restore the shape the rule forbids.
- **Reporting had to cost more than it looked like it cost.**
  `reporting_costs_more_underworld_access_than_one_deal_earns` failed on the
  first attempt and was right to: the report penalty beat the flat
  `COOPERATE_UNDERWORLD` bonus but not that bonus *plus* the authored
  `Underworld` benefit, so cooperate-then-report netted +1 access alongside the
  lawful standing gain. Fixed in the constant, and the test now derives its
  bound from the *shipped* botanist data as well as the fixture — a constant
  tuned against a fixture stops holding the moment someone authors a richer
  deal.
- **The responses differ in kind, so they are not one severity dial.** Delay and
  refuse both hand over nothing and are still not the same: refuse cools the
  approach, delay leaves it live for a change of circumstance. A negotiated
  part-deal and a diluted one move the same reduced volume and differ in what
  the requester *believes* they received, which is why only one of them is
  discoverable later.
- **Aleksy takes less but not something else.** The final ask authorizes
  `negotiate` and no substitute: a player offering a lookalike has worked out
  what it is for, and a requester who accepted it would be admitting the ask was
  never really about horticulture.

### 2026-09-05, P7 and Botany's antagonist

- **The supply invariant is enforced by physics, not by a flag.**
  `IllicitCustody` holds real `Solution` volume; spending calls `remove` and
  moves it. A part-spent batch therefore stops funding a second incident on its
  own, with no separate "used" bookkeeping to drift out of sync.
- **`player_only` is a third, distinct classification.** Not `controlled`
  (48 reagents, means Security cares) and not `Category::Illicit` (means
  recreational, someone gets hooked). Tagging Quiet Rot `Illicit` failed
  `every_illicit_reagent_can_actually_hook_someone` immediately — a correct
  catch: nobody gets addicted to a poison, and the category would have put it on
  the dealing economy's shelf.
- **A player-only reagent must still be makeable by a player.** The reachability
  invariant caught the contradiction: a reagent whose whole point is that the
  *player* chooses whether to supply it cannot be unobtainable. Quiet Rot now
  has a real synthesis whose precursors are Aleksy's own earlier asks — which
  doubles as the tell a careful player can read before the final one.
- **Refusal closes the branch rather than delaying it.** Aleksy's non-compliance
  path is standing loss and radio chatter, never a poisoning by another route.
  If refusing produced the same outcome eventually, the choice was decorative.
- **The thread hands over material and motive, then stops deciding.** It does
  not schedule a poisoning. `covert.rs` scores the act against ordinary work, so
  a stocked antagonist with a live goal can still lose to a casualty, a busy
  kitchen, or a meal too small to be worth it.
- **A real bug the tests caught:** `Solution::add` returns the overflow it
  *refused*, not the amount accepted. My first transfer read it as "landed" and
  pushed the whole dose back into custody — the contaminant would have been
  duplicated rather than moved. Two tests failed on it immediately.
- Four hardcoded count ratchets (reaction total, crafted-product audit, visual
  recipe breadth, dispensable count) all fired. Each is the codebase making a
  new reagent acknowledge that it touches the recipe book, the visual model, and
  progression. The dispensable count correctly stayed at 30: Quiet Rot is off
  the ChemMaster and costs a deliberate synthesis.
- The `EXPECTED_GAP` negative assert fired as designed — "Botany now HAS a minor
  thread — delete it from EXPECTED_GAP" — and has now proven itself in both
  directions. Bridge is the only remaining entry.

### 2026-09-05, Engineering emits a real hazard

- `Hazard` was modelled from the first perception commit and never emitted, so
  a whole stimulus kind sat unexercised. `apply_engineering_workplace_risk` now
  writes one at the moment a fault is committed, strength scaled by severity.
- **The first test for it proved nothing.** A perception-side test that writes
  the stimulus by hand shows witnessing works; it does not show Engineering
  emits one. Deleting the emission left it green with only an unused-variable
  warning. The assertion moved into `forced_power_fault_...`, which forces a
  real fault through the real system — that one fails correctly when the
  emission is removed. Same shape as the Service falsification trap: test the
  publisher, not a hand-built stand-in.
- Two of five stimulus kinds now have real sources (`Casualty` from Medical,
  `Hazard` from Engineering). `Food` is Service's to emit; `CallForHelp` comes
  from a delivered report; `SuspiciousHandling` is P7's and deliberately last.

### 2026-09-05, the Bridge adapter and a seventh department

- **This was not the Morrow change.** Adding Morrow put a member into an
  existing department. Bridge did not exist as an `orders::Department` at all —
  `from_role("Bridge")` returned `None`, which is precisely *why* its six
  residents were `crew::fluff` support and invisible to standing, grading, and
  the favor system. So this added a seventh variant, and the compiler plus two
  pre-existing integration tests found every consequence.
- **Standing seeds nothing, on purpose.** The Morrow case needed
  `copy_standing_if_missing` because an unseeded member would have halved a
  reputation the player had already earned. Bridge is the inverse: no save has
  ever held Bridge standing, so neutral is correct and copying anything in would
  invent a reputation nobody built. A test pins that so a future "consistency
  fix" fails loudly rather than quietly granting free standing.
- **Helm and comms are distinct capabilities.** The design requires the two core
  characters to be complementary, not interchangeable portraits. One
  `bridge.operate` capability would have made Odera and Sissel the same person
  with different names; monitoring stays shared so support can hold a console.
- Bridge is the only six-person department (`expected_support: 4`), and
  `DepartmentRoster::validate` already took that as a parameter — declaring 2,
  as every sibling does, is rejected. That parameter existing paid off here.
- **The `EXPECTED_GAP` mechanism worked exactly as designed.** Adding Bridge
  immediately failed `every_department_has_a_minor_antagonist_...`, which is
  the loud failure that assert was built for. Bridge joins Botany in the gap for
  a *different* reason though, and the comment says which: Botany's thread is
  deliberately reserved, Bridge's is simply not yet authored because the
  department acquired a page before it acquired any trouble.
- The Bridge floor is four carved rectangles, not one room, because furniture
  rows are cut out of the walkable volume. The four utility spots therefore
  reuse the exact coordinates of already-authored `duty` crew posts rather than
  fresh guesses that could land inside a cut-out.
- A stale doc comment in `crew::fluff` claimed support workers were safe because
  "`from_role` returns `None` for Bridge" — no longer true. Rewritten to state
  the guarantee against `Department::members()`, which is what actually keeps
  support out of standing arithmetic now that role strings no longer do.

### 2026-09-05, the Security adapter

- **`src/utility_ai/security.rs` is not `src/security_case`.** The latter is the
  player-facing case system: Security holding *the player's* batch, with a
  conversation UI, replicated messages, and appeal actions. This adapter is the
  NPC-side department. They share a subject and nothing else, and folding them
  together would put a replicated player conversation on the same path as
  authority-only witness knowledge. The module header says so, because the
  names invite exactly that mistake.
- **Every Security worker can interview**, core and support alike. A station
  where only one officer could ask questions would stall every canvass whenever
  that officer was occupied. Narrative tier governs content eligibility, never
  simulation capability — a support officer interviews exactly as well as Reyes.
- **Routine work is deliberately thin and capped below interview urgency.** A
  department whose job is *noticing* should spend most of its time available
  rather than occupied. Dispatch/desk/evidence exist so officers are somewhere
  plausible and so a freed officer has something to do, not to fill the shift.
  A test pins every routine urgency below the interview threshold.
- No data gap this time: Reyes and Bex were already the authored core pair, so
  Security needed no equivalent of the Morrow migration. The roster test asserts
  against `station.crew.ron` directly anyway — that is the exact failure mode
  Engineering shipped, and a constant that drifts from the authored cast is
  invisible until runtime.
- **A test that could not fail, kept anyway.** The `if board.ticket(id).is_some()`
  republish guard is redundant — `JobBoard::publish` already rejects duplicate
  ids — so no test can distinguish its removal. It stays because all five other
  adapters use the same idiom and it avoids building a whole `JobTicket` with
  two `format!` allocations per frame just to have it rejected. Recorded rather
  than quietly kept, because "falsification found nothing" should be explained.

### 2026-09-05, witness interviews

- **Interviews are the *pull* half that `reports.rs` deliberately left out.**
  Ordinary crew never volunteer `SuspiciousHandling`, so the only route by
  which that knowledge leaves the head that formed it is an investigator going
  and asking. This is what keeps a covert antagonist playable: being glimpsed
  is not the same as being accused, and the gap between them is legwork.
- **An interview can come back empty, and most will.** That is a real outcome
  the investigator pays travel time for, not a failure to optimise away. A
  canvass where nobody saw anything closes `Unsolved` — the station knows
  something happened and not who did it.
- Evidence is ordered seen > heard > told, because `Modality` survives from
  perception into the case. Hearsay is discounted *twice*: once when
  `reports.rs` stores it at reduced confidence, again as evidence, because a
  case built on retelling should be weaker than the retelling is as a belief.
- `Investigation::suspect` uses the **strongest single account**, not a sum.
  Summing would let an investigator manufacture certainty by asking more
  people; ten vague accounts do not add up to one clear sighting.
- `IncidentLedger::attribute` is separate from `create` because attribution is
  *earned later*. It never overwrites an existing source, so a system that knew
  the culprit at creation time keeps its ground truth and testimony cannot
  contradict it.
- **Closing the canvass belongs in the publisher, not the recorder.** The first
  version closed a case in `record_testimony`, which meant that when the roster
  ran out the publisher simply stopped publishing and the case hung open
  forever. The publisher is the system that can see candidates are exhausted.
  This was a real bug, found because the tests asserted on terminal status
  rather than on intermediate state.
- **Near-miss worth recording:** `git checkout src/utility_ai/incidents.rs` to
  undo a falsification did *not* restore the file — it left the neutered
  version in place, because the surrounding work is uncommitted. Falsifications
  must be reverted from an explicit backup copy, never from git, while the
  worktree is dirty.

### 2026-09-05, spoken reports

- **A report is a physical, spoken act, not a state transfer.** The reporter
  walks to a listener and says a line via `speech::say`; only crew within
  conversational range learn anything. This is what keeps knowledge from
  teleporting, makes the alarm observable to the player rather than showing
  them silent convergence, and makes a report interruptible — a reporter
  knocked out mid-errand never delivers, and the fact stays where it was.
- Hearsay retains 60% of the teller's confidence, keeps its teller in
  `source`, and drops the `actor`. Being told a body went down is not being
  told who put it there, and a chain of retellings decays instead of hardening
  a rumour into fact by circulating.
- `SuspiciousHandling` is deliberately **excluded** from what ordinary crew
  will report. A worker who half-saw something odd is not a witness with a
  theory, and turning every glimpse into a station-wide accusation would make
  P7's antagonist unplayable. Security interviews are the intended route, and
  they pull rather than push.
- The offer sits in `Important`, never `Emergency`: raising the alarm must
  outrank routine work but must never outrank treating the casualty. A
  responder who can reach the patient should go to the patient.
- The `ALREADY_KNOWS` gate exists to stop *decay-clock resets*, not to protect
  eyewitnesses — `NpcMemory::remember` already keeps the stronger fact. Without
  it, two crew repeating the same rumour would refresh `learned_at` forever and
  it would never fade. The first test written for it passed with the gate
  removed because it pinned the wrong mechanism; it now asserts `learned_at`
  directly and carries a positive control for an unsure listener.
- `deliver_reports` uses **one** mutable query rather than a reader plus a
  writer. Reading the reporter's fact and writing to listeners are disjoint in
  time but not in component set, so two overlapping queries are a `B0001`
  access conflict at runtime — it compiles and then fails every test.

### 2026-09-04, memory-gated response

- **`JobTicket` gained `subject: Option<Entity>`.** A ticket knew *where* to go
  but not *what it concerned*, and those differ exactly when perception matters
  — the Service cleanup fix already proved travel target and subject diverge.
  Rather than have the kernel special-case medical cases, the ticket says what
  it is about and the kernel gates on that. Thirteen literals, mechanical.
- `perception::may_respond_to` is deliberately permissive in two directions. A
  ticket with no subject is station work at a known place (a fault light, a
  delivery) and needs no witness. An agent with **no** `NpcMemory` component is
  un-modelled, not blind — gating those would silently switch off every
  department that has not opted into perception. Only an agent that *has* a
  memory and lacks the fact is refused.
- Scoring gained `UtilityFactId::Awareness`, sourced from decayed memory
  confidence. Naming it as a fact rather than hiding certainty in a multiplier
  keeps the decision log readable and lets an eyewitness outrank someone who
  only heard a shout. The boolean filter and the continuous fact are redundant
  by design: the fact does the ranking, and the filter also covers the
  non-emergency selector, which has no scoring hook.
- `medical_responder_escorts_the_exact_casualty_to_a_reserved_bed` failed after
  the gate landed, and that was the correct failure: its schedule never ran
  `witness_stimuli`, so the test had been quietly relying on omniscience.
  Adding the system made the harness faithful rather than weakening the gate.
- **Testing trap, recorded because it cost real time:** `Entity::to_bits` in
  Bevy 0.19 encodes the index so a *later*-spawned entity sorts **lower**. A
  first version of the witness test passed with both mechanisms disabled,
  because spawn order accidentally aligned with the correct answer. The test
  now asserts on the ticket's claimant and carries an in-test assertion that
  the tie-break genuinely opposes awareness, so it cannot pass by luck again.

### 2026-09-04, perception and memory

- Added `src/utility_ai/perception.rs`. Sight is the `speech::place_bubbles`
  pairing — distance, room identity, and the real
  `interaction::authority_segment_blocked` occlusion test against the same
  `Solid` boxes movement uses. No second geometry system, no raycasting, so it
  stays correct on a headless authority. This generalises the strongest of the
  four ad-hoc precedents rather than adding a fifth, as `docs/npc-ai.md` asked.
- **A doorway is visible from both rooms it joins, not neither.** `room_at`
  returns `None` on a threshold; treating that as blind would make stepping
  into a doorway a reliable way to vanish, which `docs/npc-ai.md` warns about
  explicitly. Two distinct rooms still block sight.
- Hearing carries through walls at half confidence and cannot name the actor,
  so a Security interview can distinguish a witness from someone who heard a
  bang. Only `Casualty`, `CallForHelp`, and `Hazard` are audible.
- Memory is bounded (24 facts), merges repeat sightings of the same event
  rather than stacking duplicates, evicts the least confident rather than the
  oldest, and decays to zero over a per-kind horizon. Direct sight supersedes
  hearsay about the same event.
- Chemical concealment reuses `Bloodstream::concealment` — the same aggregate
  the cult guards consult — rather than inventing a second stealth rule.
- Committed to one `EYE_HEIGHT`; the three existing occlusion callers each had
  their own (1.3, 0.65, origin), so a witness result could depend on which
  system asked.
- Wired it to a real consequence: `open_cases_from_incidents` emits a
  `Casualty` stimulus, so a crew member beside a collapse remembers it and one
  across the station does not. Perception is not inert infrastructure.

### 2026-09-04, room appeal and mood chemistry

- Added `RoomAppeal`, derived in `MaintainNeeds` from served food, uncleared
  plates, and break-spot occupancy read from the reservation book. It scales
  break offers and names a qualitative reason (`Welcoming`, `NoFood`, `Dirty`,
  `Crowded`). It is a modifier, never a command: nothing sends anyone to
  Service, and every input is a fact the player can walk in and look at.
- Added `social_disposition` and `fit_for_leisure` to the kernel, so the three
  providers cannot each invent their own rule for "cheerful" or "too far gone".
  Happiness and Euphoria make crew linger, Sadness and Paranoia make them
  withdraw, and heavy sedation, burning, or choking suppress voluntary leisure
  outright. Both read only the existing `Bloodstream`.
- Added `ReservationBook::claims_on`, a read-only occupancy query. Scoring may
  inspect capacity to judge crowding; only selection may claim it.
- **Moved `Rest` and `Socialize` from the `Idle` bucket to `Routine`.** Buckets
  are strict priority classes, so an `Idle` break could never outrank the
  always-available `IdleObserve`: an exhausted resident would stand around
  instead of sitting down. The earlier rest test only passed because its
  fixture had no competing work. A break is a legitimate use of a shift.

### 2026-09-04, rest and social actions

- Added `src/utility_ai/social.rs` with `Rest` and a two-person `Socialize`, the
  second and third consumers of the opportunity seam. Fatigue and social
  pressure now drive real behaviour rather than only accumulating.
- Authored two capacity-one lounge seats and one capacity-two gathering spot in
  `lab.map`, inside the Service floor brush and away from the kitchen line. The
  existing spot test validates they are unique, walkable, and routable.
- A conversation is credited by **overlapping attendance, not co-completion**.
  Staggered decision clocks mean two residents who talk together essentially
  never finish on the same tick; an initial co-completion implementation
  silently never fired. Both partners are credited when the partnership is
  observed, since the later finisher would otherwise see an empty roster.
- Pair cooldowns are symmetric — `(a, b)` and `(b, a)` are one entry — so a pair
  cannot loop. A resident is only offered a conversation when some willing
  partner is actually off cooldown, so nobody walks to the lounge to sit alone.
- Falsification caught two more vacuous negatives: `true ||` on a gate inverts
  rather than removes it, and a "never offered" test passed with its gate gone
  because a different early return was doing the work. Both gates now have
  tests that fail when the exact gate is removed, plus a new test proving the
  cooldown suppresses a live offer rather than only answering `ready()`.

### 2026-09-04, opportunity seam

- Added `UtilityOpportunityBuffer`, `UtilityOpportunity`, and the
  `OpportunityProviders` system set. Providers publish per-agent offers during
  `BuildContext` behind a guaranteed per-frame clear; the same selector that
  reads the `JobBoard` scores them together, so a personal action competes with
  that agent's own work instead of running on a second controller. Selection
  copies the offer's execution contract into `CurrentAction`, and reservations,
  interruption, and cleanup stay centralized in the kernel.
- The kernel appends `CanAct` to every offer, so an incapacitated body vetoes
  all of them without each provider restating it.
- Added `NpcNeeds` (hunger, fatigue, social) to `UtilityControlBundle`, advanced
  in `MaintainNeeds` so the same frame's scoring sees current values. Rates are
  deliberate placeholders pending authored personality tuning. Needs are
  pressures to act only — nutrition, toxins, and sedation stay in
  `Body`/`Bloodstream` and are never duplicated.
- Implemented hunger-driven eating as the seam's first real consumer. A hungry
  resident is offered every served batch, reserves one serving, walks to that
  exact batch, and ingests a real dose through `consume_meal_serving`. A
  contaminated meal therefore affects an NPC exactly as it affects anyone else.
- Every new test carries a positive control. An early version of the veto and
  persistence tests passed with the feature disabled — they were negative
  assertions with nothing proving the offer would otherwise have been taken.
  Both now run the identical offer with and without the condition.

### 2026-09-04, integration wave close

- Routed Service cleanup travel to the floor-authored `service.cleanup` spot.
  Targeting the meal entity was unreachable — only `ActionTarget::Point` is
  lifted to the walker's height — so cleanup tickets were claimed and never
  resolved, stalling the shift. The per-batch reservation key is unchanged, so
  one worker still takes one plate. Recorded the entity-versus-point hazard as a
  standing rule for every future adapter.
- Made `arc::every_department_has_a_minor_antagonist` carry an explicit
  two-sided `EXPECTED_GAP` for Botany rather than a plain allowlist. The
  negative assert fails when the reserved packet lands, so the gap cannot be
  quietly forgotten. Chose this over authoring a fifth thread, which would need
  its own script type, module, and registration — a full packet, not a data fix.
- Authored Chief Engineer Morrow into `station.crew.ron`,
  `Department::members`, `RESIDENT_NAMES`, and `resident_department`, and seeded
  his standing from Tech Lindqvist on legacy saves. Engineering was a
  one-member department, so adding him unseeded would have halved every existing
  save's Engineering standing on load. Its migration test asserts the department
  average, not just the copied key.
- Each of the three changes was falsified: reverted individually and shown to
  fail the exact test that guards it.

### 2026-09-04

- Added the compiling utility kernel with ordered schedule sets, typed normalized
  facts, response curves, multiplicative candidates, priority buckets,
  deterministic near-best selection, hysteresis, action lifecycle,
  reservations, bounded decision traces, and controller ownership.
- Connected the first two reference actions to the existing errand navigator.
  An opted-in headless resident now selects `MaintainPost`, claims its post,
  travels, performs, resolves, releases its claim, restores a standing route,
  and replans repeatedly.
- Extended the shared errand API with a caller-owned arrival radius after the
  end-to-end test proved the default arm-length radius stopped workers too far
  from a precise console post.
- Registered only `NpcActivity` for replication. `UtilityAgent`, scores,
  `CurrentAction`, reservations, controller ownership, and decision traces remain
  authority-only.
- Made legacy ambient selection exclude `UtilityAgent` and added a regression
  test proving an opted-in resident receives no legacy destination.
- Replaced the linked-order self-dose seam in `orders::complete_delivery` with
  an explicit `OrderUse` branch. The requester becomes the carrier, the exact
  container survives handoff, and only arrival beside the beneficiary applies
  the bounded dose. Personal-consumption orders without that context retain
  their existing behavior.
- Added separate preparation and clinical resolution signals. A moved patient
  is followed; a missing patient sends the carrier home with the retained batch
  instead of silently dosing the requester.
- Integrated order recall with utility ownership. The same resident is reused,
  its current action and errand are interrupted, its action-scoped reservation
  is released once, and utility control is restored after it returns to duty.
- Made incapacity an explicit utility control owner. It suspends the exact
  active owner and locomotion metadata, interrupts and cleans up utility work,
  presents the NPC as down, and restores either utility work eligibility or an
  interrupted order visit once the body recovers.
- Added the department-neutral `JobBoard`, `NpcJobProfile`, `JobDomain`,
  capability, narrative-tier, and `utility_spot` contracts. Claims use the same
  action-instance compare-and-swap protection as workstation reservations, and
  orphaned workers reopen their work.
- Added five validated Cargo utility spots and the complete routine pipeline:
  manifest review, weighing, sorting, dispatch, and requisition clearing. Four
  workers advance real bounded Cargo state rather than only playing a work
  animation.
- Added Quartermaster Rhee as Cargo's second core character and Loader Bell plus
  Clerk Nwosu as support workers. Only these four residents migrate in P2;
  every other department remains under its current controller.
- Cargo migration occurs in `PreUpdate`, so its marker and explicit locomotion
  owner are visible before legacy ambient systems can act. Legacy chemical
  routing also excludes utility agents, while ordinary crew keep the old
  response unchanged.
- Added the bounded incident ledger and the first real workplace outcome. A
  hazardous Cargo completion can burn the exact worker's existing `Body`, emit
  an incident, and cannot open a second Cargo case while the first is active.
- Made routine department problem pressure depend on the existing hidden
  `StationStability` resource. Full stability reduces ticket risk to 35 percent
  of its base, falling stability raises it continuously to twice base with a
  50 percent hard cap, and Unstable or Critical bands consume the opening grace
  faster. Forced test outcomes bypass probability but retain safety caps.
- Added two validated Medical bed affordances, Paramedic Hale, and Orderly
  Imani. Medical's two existing core characters and these two support workers
  now use the same utility controller for emergency-response eligibility.
- Added `MedicalCaseLedger` and the first complete response path from a real
  Cargo incident. An emergency ticket selects one qualified responder, who
  walks to the exact casualty, reserves a bed, escorts that same body to it,
  provides bounded burn care, resolves the source incident, releases the bed,
  and returns both characters to utility control.
- Added replicated `NpcPosture` presentation so a conscious inpatient can use
  the existing lying animation without falsifying physiological collapse.
- Made Cargo incident severity and damage follow the same hidden station health
  used for frequency. A full-stability burn remains severity 0.35 and 18u;
  zero stability is capped at severity 0.65 and 30u. The existing grace,
  cooldown, one-active-case, and 50 percent frequency cap still apply.
- Split Medical care after a bounded diagnosis period. Mild Cargo burns finish
  with standard bed care, while severe burns, brute injury, and poisoning enter
  a linked treatment state without releasing the bed or patient identity.
- Added `RequestSource::MedicalCase` and reused the existing conversation,
  acceptance, Chemistry handoff, and resident recall pipeline. Medical staff
  are preferred as requesters, with real Cargo coworkers as deterministic
  fallback. A same-frame failed recall now releases its intake reservation.
- Carried treatment now returns arrival-side effect facts to Medical: applied,
  helpful, harmful, illicit, overdose, empty, or target unavailable. A clean
  application starts observation but does not resolve the case until the
  patient's matching damage actually decreases.
- Added bounded retry and escalation for complex treatment. Expired or vanished
  requests reopen after a delay, harmful or otherwise unsafe applications do
  not discharge the patient, and three failed attempts become an explicit
  escalated case instead of looping forever.
- Added Medical cleanup for missing patients and interrupted responders. Bed
  claims and obsolete response jobs are released, source incidents close when
  their embodied patient no longer exists, and an interrupted transport
  returns to responder selection rather than remaining stuck.
- Made emergency switching a proposal-and-commit transaction and added the
  authority-side debug ownership checker. Competing workers retain their old
  action and claims unless the replacement has atomically acquired both its
  exact ticket and target.
- Added the first Botany slice with four named plot facts, five validated work
  spots, four qualified workers, bounded physical produce, toxic provenance,
  stability-scaled processing exposure, quarantine, and exact-worker Medical
  intake.
- Split Botany from Service in relationship and order departments. Ivy keeps
  her identity and standing, Vale is seeded from Ivy on legacy saves, Service
  remains a two-person core with Chef Dubois and Steward Amari, and Amari is
  seeded from Dubois without rewriting existing per-person history.
- Selected the complete 30-person target cast. Engineering adds Chief Engineer
  Morrow, Mechanic Torres, and Systems Tech Adeyemi; Service adds Cook Navarro
  and Attendant Mensah; Security support is Patrol Officer Dlamini and
  Dispatcher Novak; Bridge promotes Helmsman Odera and Yeoman Sissel while
  retaining Park, Alvarez, Fenn, and Ruiz as support.
- Adopt utility scoring for action selection and an explicit state machine for
  execution.
- Expand to seven relationship and AI job departments: Medical, Security,
  Engineering, Cargo, Service, Botany, and Bridge.
- Give every department exactly two unique core player-facing characters.
  Preserve the existing Medical and Security pairs, add one core character to
  Engineering, Cargo, Service, and Botany, and promote two current Bridge
  residents.
- Expand the full cast to an initial target of 30 stable residents: 14 core and
  16 support.
- Prove the system through a Cargo-only micro-pilot before freezing generalized
  contracts and migrating other domains.
- Keep Chemistry player-operated while allowing real AI incidents to create
  linked Chemistry work.
- Reuse `Errand` and navigation instead of replacing proven movement first.
- Add explicit reservations and action phases rather than extending
  `CrewPhase::Waiting`.
- Enforce one decision controller and one locomotion owner per NPC throughout
  migration, then delete the legacy ambient decision path.
- Make Cargo injury and Service food poisoning end-to-end reference scenarios.
- Evolve the existing Social directory into a Crew menu that communicates
  department condition, core relationships, public activities, consequences,
  support crew, voluntary aid, and the complete shared transcript.
- Let the player proactively offer real chemical batches to lagging departments
  and receive benefits or consequences from actual chemistry and staff choices.
- Separate Chemistry handoff from destination use for linked requests. The NPC
  accepting a batch carries it back to the patient, process, or incident that
  caused the request, and self-use requires an explicit self-target.
- Make the player the only source of special illicit chemicals for NPCs. Covert
  chemical actions require remaining physical player-supplied stock and cannot
  spawn or infer it from plot state.
- Require every illicit deal to offer a concrete player upside, preserve the
  existing underworld offer seed, and support meaningful cooperation,
  negotiation, substitution, delay, refusal, deception, reporting, and recovery
  choices where authored.
- Keep private motives, knowledge, and utility traces authority-only.
- Use one coordinator to maintain this living plan across parallel agents.

## Open questions

These do not block P1, but they must be resolved before the named phase.

- Resolved in P2: the map marker is `utility_spot` with stable `id` and positive
  integer `capacity`; duplicate or invalid markers are rejected.
- Resolved for P3 cast identities: Cargo uses Quartermaster Rhee, Loader Bell,
  and Clerk Nwosu. Medical uses Paramedic Hale and Orderly Imani. Botany uses
  Agronomist Vale, Grower Chen, and Technician Mbatha. Service uses Steward
  Amari, Cook Navarro, and Attendant Mensah. Engineering uses Chief Engineer
  Morrow, Mechanic Torres, and Systems Tech Adeyemi. Security support is Patrol
  Officer Dlamini and Dispatcher Novak. Capability and personality tuning
  remains part of each department packet.
- Resolved for P3: Helmsman Odera owns navigation and readiness while Yeoman
  Sissel owns communications, reports, and briefings. Park, Alvarez, Fenn, and
  Ruiz remain Bridge support.
- P4: The current `NpcPosture::Lying` presentation reuses the existing
  collapsed animation without setting `Body.collapsed`. Decide after visual
  inspection whether an authored conscious-patient lying clip is required.
- P4: The implementation now separates preparation from clinical outcome and
  rejects harmful, illicit, and overdosed applications as successful care.
  Decide the exact player-facing reputation and stability pricing for those
  destination outcomes before Crew-menu integration.
- P5: Whether hunger and fatigue persist across saves or reset with the shift.
- P5: First authored meal recipes and how their ingredients map into chemistry.
- P6: Exact doorway visibility rule for NPC sight.
- P7: Which existing named resident or spawned actor can embody each antagonist
  without revealing campaign identity or contradicting current story content.
- P7: Exact reagent IDs classified as player-only illicit supply.
- P7: Reward catalog and which requests permit negotiation, safer substitution,
  tracking, recovery, or payment in advance.
- P8: Whether 30 remains the default resident count after performance and room
  density playtesting.

## Restart checkpoint

Checkpoint refreshed: 2026-09-04, at the close of the integration wave. The
wave is finished and the tree is green. Do not treat any item below as
"probably done" — every number here was measured, not reported.

### Verified state, measured this checkpoint

- `cargo test --workspace` is **fully green**: **1566 passed, 0 failed**.
  Clippy is at its long-standing 115-warning baseline with zero introduced.
- `cargo fmt --all -- --check` is clean.
- `cargo check --tests` is clean.
- `cargo clippy --workspace --all-targets` reports **115 warnings, 0 errors**,
  all pre-existing (`too many arguments`, `very complex type`, dead code in
  `orders`/`shift`/`ui`, and unused imports in `botany.rs:13`,
  `engineering.rs:12`, `medical.rs:22`). This wave introduced **zero** new
  warnings. Those unused imports are worth a cleanup pass but are cosmetic.
- `git diff --check` is clean. The release binary builds.
- The 30-resident cast is now **20 live**: Cargo, Medical, Botany, Engineering,
  and Service are each four workers. Security, Bridge, and their support remain
  unmigrated.

### What this wave closed

- **Service cleanup stall (real bug).** `CleanBatch` targeted the meal entity.
  Spent plates sit above the floor, and only `ActionTarget::Point` is lifted to
  the walker's height, so cleanup tickets were claimed and never resolved —
  the whole Service shift stalled. Cleanup now walks to the floor-authored
  `service.cleanup` spot while still reserving the exact per-batch key at
  capacity one.
- **Botany antagonist gap.** `arc::every_department_has_a_minor_antagonist` now
  uses a two-sided `EXPECTED_GAP` check instead of failing. See the reservation
  below.
- **Engineering's missing second core.** Chief Engineer Morrow existed only as a
  string in `engineering.rs`, so Engineering migrated three of four workers at
  runtime while its unit test passed by hand-spawning entities. He is now in
  `station.crew.ron`, `Department::members`, `RESIDENT_NAMES`,
  `resident_department`, and the save migration.

### Generalization notes for the next adapter

- **Entity targets are not height-lifted.** `ActionTarget::Point` is rewritten
  to the agent's own Y before navigation; `ActionTarget::Entity` keeps the
  target's live transform, and arrival tests full 3D distance within
  `AT_TARGET_DISTANCE`. Any target that is not floor-authored — anything on a
  table, shelf, or bench — must route the walk to a `utility_spot` and keep the
  object only as the reservation key. Targeting the object directly produces a
  silent permanent stall, not an error.
- **A unit test that hand-spawns `CrewMember` entities does not prove runtime
  migration.** It bypasses the roster data entirely. Engineering passed its
  migration test for weeks while migrating three of four workers in the real
  game. Assert against the authored roster too.
- **An opportunity provider must republish every frame.** The buffer is a
  proposal surface, not a request queue: it is cleared at the start of every
  `BuildContext`. A provider that publishes once and stops has withdrawn the
  offer. Offers are addressed to one exact agent, so a provider that wants
  several agents to consider the same target publishes one entry each, which
  keeps per-agent feasibility honest.
- **Buckets are strict priority classes, not score nudges.** A candidate in a
  lower bucket can never beat one in a higher bucket, whatever its score. The
  always-available `IdleObserve` sits in `Idle`, so anything else placed there
  is effectively unreachable. Put a real intent in the bucket that matches its
  priority — a break belongs in `Routine`, not `Idle`.
- **Agents never finish together, so never pair on co-completion.** Decision
  clocks are deliberately staggered, so two residents performing a shared
  action resolve on different ticks. Any multi-agent outcome must be keyed on
  overlapping attendance or an explicit pair reservation, and must credit both
  sides when the partnership is observed — the later finisher sees an empty
  roster. A co-completion check compiles, passes its own unit test, and silently
  never fires in play.
- **A negative assertion needs a positive control.** "This must not be
  selected" passes trivially when the feature is switched off. Run the
  identical case with and without the condition, or the test proves nothing —
  two tests in this wave initially passed with the seam disabled.
- **Adding a member to a department changes its standing arithmetic.**
  `Shift::standing` averages over `members()`, so a new core character must be
  seeded with `copy_standing_if_missing` or every existing save's department
  reputation drops on load. Silent, player-visible, and caught by no existing
  test.

### Deliberately reserved — do not start these without asking

- **Botany minor antagonist.** Reserved as its own packet so it is not a token
  placeholder, and because it implies illicit chemicals entering NPC-generated
  inventory, which has its own no-NPC-source rules. `arc/mod.rs` carries an
  `EXPECTED_GAP` constant with a **negative** assert: when this packet lands,
  that test fails on purpose and forces the entry's removal. Do not "fix" that
  failure by deleting the assert.
- **Security and Bridge migration**, including the Odera/Sissel promotion and
  Bridge relationship membership.
- **Speech during social actions.** `Socialize` currently changes needs,
  standing, and `NpcActivity` but says nothing aloud. Speech pools belong with
  the P6 perception/memory work so a line can reference something the pair
  actually knows.
- **Authored needs tuning.** Rates, thresholds, and appeal weights are
  deliberate placeholders in Rust. They belong in RON with per-personality
  weights, which is the last P5 item.
- **Aid beyond assessment**: a worker choosing to *use* an accepted batch on a
  patient or a process. The lifecycle states and `take_contents` are in and
  tested; no candidate drives `Stored -> ReservedForUse -> Used` yet, so a
  donated treatment is accepted and shelved rather than administered.

### Next actions, in order

The station has now been watched once, through `utility_ai::decision_log`
(`%LOCALAPPDATA%/ChemGame/ailog.txt`, truncated per run). Ninety seconds of real
play produced three defects that headless testing could not have found. The
immortal incident is fixed; the other two are open and are the top of this list.

1. **Botany advertises work nobody can take.** 171 `ReservationUnavailable`
   failures in one 90-second run — Grower Chen 126, Vale 27, Ivy 18. Botany
   publishes one ticket per plot, but every one of them carries the same
   `ReservationKey(format!("utility.spot.{}", kind.spot()))` at map capacity 1,
   so the board advertises several `tend` jobs when only one is physically
   possible. Workers select, fail, and retry roughly twice a second, forever.
   **The scale simulations could not have caught this**: they gave every ticket
   a distinct reservation key and were described as the strictest case, when
   that was the most forgiving one. Any future scale sim must include tickets
   that contend for one key.
2. **Eighteen of thirty workers never performed a real job.** All of Medical,
   Engineering, and Service idled the whole run; only Cargo, Botany, Security
   and Bridge published claimable tickets. Medical is partly explained by the
   immortal-incident bug above and should be re-measured first. Engineering and
   Service need their own trace.
3. **A longer playtest, now that the trace is readable.** P8's tuning checkbox
   asks for action frequency, incident bounds, Service attraction and response
   times to be tuned *from playtest traces*. One 90-second trace exists. Note
   that most behaviour constants are Rust, not RON, so tuning currently means a
   rebuild — moving them to RON is worth doing first if the iteration loop
   matters.
4. **Run the game with `cargo run`, never the built executable directly.** Bevy
   resolves the asset root relative to the binary unless `CARGO_MANIFEST_DIR` is
   set, which only `cargo run` does. Launching `target/<dir>/debug/chemgame.exe`
   makes every asset fail to load and renders a grey screen — which looks enough
   like a clean start that it can be mistaken for a passing smoke test.
5. **Migrate the off-roster cast**, which unblocks P8's last two checkboxes.
   `crew::fluff`'s support crew is the large one; cult guards and restock
   couriers are small and arguably *should* stay ambient, since neither has a
   department or a job board to answer to. That is a design call, not a
   technical one.
6. **The aid-use candidates**, when a department wants them:
   `AdministerDepartmentAid` and `UseProcessAid` moving an accepted batch
   through `ReservedForUse` into a real body or a real process. Everything
   underneath is built and tested — `take_contents` drains exactly once, and
   `utility_ai::capacity` already reads whatever the resulting metabolism
   produces.

### Still unobserved

One 90-second AFK observation has been run and is analysed above. Nothing has
been *played* — the Cargo shift, Medical escort, lying posture, Botany floor
activity, and the Service meal pipeline are still proven headless only and need
the user's visual playtest. Engineering's four-worker runtime migration has not
been seen in-game.

Treat the first trace as a floor, not a survey: it found three defects in ninety
seconds, and two of them were in departments that were producing *no* log lines
at all rather than obviously wrong ones. A silent department in the trace is a
finding, not a clean bill.

Voice-chat work shares this dirty worktree — `src/voice`, `src/voice/codec.rs`,
`src/ui/bookmarks.rs`, and the Cargo manifests. It survived this wave intact.
Preserve it and inspect overlapping diffs before changing shared registration
surfaces.

## Definition of complete

This goal is complete only when:

- Every staffed job domain, including Botany, has routine work and
  consequential responses.
- Medical, Security, Engineering, Cargo, Service, Botany, and Bridge each have
  exactly two unique core characters who directly interact with the player.
- Every resident selects actions through the shared utility framework.
- Core and support residents use the same utility, job, health, social, and
  emergency systems, with narrative tier affecting only content eligibility.
- No NPC is ever simultaneously controlled by legacy and utility decisions or
  by two locomotion writers.
- The Cargo injury and Service poisoning reference scenarios work end to end.
- Doctors can transport a real casualty to a real bed, and a linked requester
  can seek treatment when that is the best available response.
- A linked requester carries the actual delivered batch back to its explicit
  beneficiary and use destination; accepting it at Chemistry never doses the
  carrier by implication.
- Food and social activity make Service mechanically important.
- The Crew menu clearly communicates public department condition, work,
  incidents, people, relationships, aid, and consequences while preserving the
  complete shared conversation transcript.
- The player can voluntarily aid any department with a real chemical batch, and
  actual use can help, fail, be rejected, or cause downstream harm.
- NPCs affect one another through real bodies, chemistry, relationships,
  incidents, knowledge, and work state.
- Antagonists choose embodied covert actions from goals and opportunity.
- Special illicit chemical actions are impossible until a player supplies a
  physical batch, and the NPC consumes only its actual remaining volume.
- Illicit requests offer a concrete upside and meaningful alternatives, so
  cooperation is tempting rather than a disguised universal penalty.
- NPC decisions use plausible knowledge instead of omniscient queries.
- Multiplayer preserves public consequences while keeping secrets private.
- Save/load does not erase consequential state or restore impossible transient
  locks.
- Targeted, full-suite, clippy, formatting, and diff checks are accounted for.
- Manual playtesting confirms movement, animation, timing, readability, room
  density, emergency response, and antagonist ambiguity.
