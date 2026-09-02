# Conversation-first orders

Security follow-up: [Rogue Security disputed-seizure pilot](rogue-security-plan.md).

## Player flow

Two visitors can be on their way to, or waiting at, the delivery windows.
Their HUD indicators disclose only their names, windows, and waiting status.
Interact with a visitor to hear the explanation and see the requirements.
Holding a beaker does not prevent this conversation or consume the beaker.

“I'll handle it” accepts the request and starts its full preparation allowance.
Closing the panel leaves it unaccepted. The world continues running while any
panel is open. Accepted customers move into the pickup line; ready deliveries
can serve any accepted customer in the appropriate lane.

The order desk shows the five most urgent accepted requests and an additional
count. **Tab → Orders** contains every accepted request, including its explanation,
requirements, and remaining preparation time. Either chemist can accept;
teammates review the resulting shared order there.

The station standing board presents service controls and station radio together.
Its radio history scrolls independently under the pointer.

## Authority and lifecycle

- `IntakeState` serializes admission from all participating generators. Travelling
  requests count against the shared limit of two. Per-frame identity reservations
  cover deferred ECS spawns; due sources retain admission order while eligible.
  Retry timers do not accumulate missed arrivals into bursts.
- `PendingOrder` owns the undisclosed payload. It is never registered for
  replication. `AwaitingConversation` replicates only identity and arrival state.
- `OpenOrderConversation` validates identity, reach, visibility, and live bodies.
  Only the requesting player receives `OrderConversationOpened`.
- `AcceptOrder` requires a recorded conversation with that player and the current
  request. Concurrent acknowledgments resolve once. Acceptance promotes the
  payload to the existing replicated `Order`; its pickup sequence follows
  acceptance order, independently of request creation order.
- Greeting time begins at arrival: 180 seconds, one reminder at 120. Reopening
  or changing greeting position does not reset it. Unheard expiry costs one
  point of individual standing and emits no accepted-order failure.
- Obsolete campaign situations, departures, incapacity, and unrecoverable
  routing release pending requests. Cult offers clear their offered stage
  without advancing the chain; independent waves can withdraw obsolete offers.
- Accepted requests never consume intake capacity. Their clocks run during
  movement. Existing emergency orders remain directly active.
- Temporary state clears on session changes. Live orders keep the existing
  session lifetime; this change does not add live-order save persistence.

## Campaign wording

`RequestContext` explicitly stores the source, campaign identity, greeting kind,
and an optional authored step. Context is authority-only. The greeting pools in
`assets/data/station.greetings.ron` distinguish ordinary and campaign requests
using neutral wording, including reminders. Speech and radio use the same
selected call-over.

Campaign-related sources include counter-support, Cult, illicit requests,
addict return orders, and minor requests whose outcomes directly affect the
active campaign (Quack, Smuggler, Saboteur, and Bent Guard). Ordinary/exact work
and the Obsessed thread's general stability effects use ordinary greetings.
Greeting kind changes no timing, queue priority, color, or allegiance icon.

## Queue authoring and movement

`queue_point` map entities supply an ordered path for each delivery lane. Standing
positions start one metre apart along that path, with extra clearance at corners
and greeting positions. A point with lane `clearance` and a positive `clearance`
radius reserves crossing space at a corridor mouth. Keep these spaces open so
the line does not seal off its own approach or exit.

Ordered `public_access` and `medical_access` points reserve continuous walking
aisles from the corridor to the greeting windows. Pickup slots leave two NPC
body radii plus walking tolerance beside every aisle segment, not just its
endpoints. These remain open with a full pickup line so new requesters can reach
either greeting position and departing customers can pass beside the queue.

The current paths provide space for the entire eligible cast. Tests validate
their standing footprints against authored walkability and collision brushes.
Queue assignments are released on completion, cancellation, or departure;
remaining customers move forward.

`NpcMotion` is shared by ordinary routes, errands, and hostile pursuit. Swept
substeps keep bodies separate at low frame rates. Stable detours preserve the
original destination, and crowd waits do not trigger wall recovery. Departing
traffic receives room to pass. Stationary and incapacitated bodies remain
obstacles; separate floor heights do not collide. Player collision is unchanged.
Collision detours search far enough beside a nearby destination to round a
long pickup line and reach its aisle; a fixed node budget bounds that search.
The search allows 12 metres beside the route, capped at 12,000 examined nodes.
Departure priority applies only on the same floor.
The failed-recovery budget resets after a metre of forward route progress, so
separate difficult corners on a long resident trip do not add up to a false
routing failure. Jitter and recovery loops do not reset that budget.
At narrow doorway portals, walkers finish entering the next region before
turning toward the following waypoint. Arrival tolerance cannot skip the
remaining wall clearance. Errands and pursuers likewise consume waypoints
only after their actual movement reaches them, rather than after a requested
step that collision may have shortened.

## Verification

Focused suites cover conversation validation, carried items, greeting clocks,
admission fairness, campaign wording, shared acknowledgment, wire privacy,
late joining, direct/tray delivery gates, emergency behavior, and long lines
through both real doorways, including departure from the middle of a line.
The arrival regression uses both actual map window orientations, fills the
pickup line with fourteen stationary customers, then walks two new requesters
from the corridor to their greeting positions while checking body separation.

```powershell
cargo test --bin chemgame order_intake
cargo test --bin chemgame npc_motion
cargo test --bin chemgame conversation_payload
cargo test --bin chemgame pickup_queues
cargo test --workspace
cargo clippy --workspace --all-targets
```

Debug builds also accept `--order-playtest` alongside `--solo`, `--host`, or
`--join 127.0.0.1`. This opt-in rendered scenario seeds a long line and exercises
the conversation, shared acceptance, order directory, and standing board.
It writes screenshots and `result.txt` under `target/order-playtest/<role>` and
exits automatically. Run it with a separate `LOCALAPPDATA` directory per process
to isolate test saves and account identities. It is absent from release builds.
The pending visitor starts in the corridor and must walk to the window; the
scenario report records that arrival rather than teleporting the visitor there.

The September 2 queue follow-up passed all five focused queue tests, the
authored aisle/furniture/cast-capacity map test, and all five shared motion
tests, including different-floor departure priority. The Medical test now uses
the actual Medical window instead of
the default Public fallback; it exposed and verified the longer collision
detour needed to join the Medical line. A narrow-aisle regression also checks
that incoming traversal fails without aisle reservation and succeeds with it.
After both incoming visitors reach their windows, the map regression accepts
one and verifies that they join the pickup tail while the other keeps waiting.
The final workspace run also passed a sixth queue regression that walks Reyes
from both his authored Security post and an observed difficult corner to the
Public greeting window using the real map and collision resolver. It checks
for routing failure during the entire trip. Two crew regressions verify that
meaningful forward progress clears old recovery attempts while jitter does not.
The rendered Security follow-up exposed an early turn at its narrow exit:
a 0.12 m arrival tolerance could skip a portal whose safe overlap was only
0.1 m wide. The real-map test reproduced the resulting withdrawal at 33 ms
frames before the fix and now covers eleven frame durations from 10 to 200 ms.
The original failing trace remains in `target/security-route-framerate.log`.

Rendered solo and LAN host/client scenarios also passed on September 2:
all roles observed 11 shared accepted orders, no pending visitors, and the
incoming visitor reaching the window from the corridor. Solo and host reached
all 11 pickup assignments; the client's queue assignments remain authority-only.
Final closest-body spacing was 0.871 m solo and 0.870 m in co-op. The corridor
queue, full order directory, and one-screen standing board were inspected in
the captured images. The greeting screenshot exposed speech above the order
panel; conversation/directory panels now use the same modal layer as Inspect.
These runs used LAN because Steam was unavailable. The client logged 108
existing Bevy `B0004` hierarchy warnings and a disconnect when the host exited
first; there were no panics. Solo and host logged no gameplay errors.

After `cargo build`, the PowerShell helper sets isolated application-data paths
and points Bevy at the workspace assets:

```powershell
./tools/run-order-playtest.ps1 -Mode solo
./tools/run-order-playtest.ps1 -Mode coop
```

The scenario recalls the ordinary residents and adds absent authored occasional
visitors, then seeds ten accepted requests and one conversation. The client takes
the eleventh request in the co-op run. Four screenshots cover the conversation,
full directory, physical pickup line, and combined standing board.

The final integrated September 2 rerun also passed in solo and LAN co-op after
the Security doorway fix. Every peer observed eleven accepted orders and the
incoming requester reaching the window; both authority runs reached all eleven
pickup slots. Minimum spacing was 0.873 m solo and 0.876 m in co-op. The final
conversation capture confirms that NPC speech no longer overlays the panel.
Doorway turning is covered at eleven frame steps (10–200 ms) from both Security
origins, and errand/hostile movement has a blocked-final-step regression.
