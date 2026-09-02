# Conversation-first orders

Security follow-up: [Rogue Security — Deferred](rogue-security-plan.md).

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

The current paths provide space for the entire eligible cast. Tests validate
their standing footprints against authored walkability and collision brushes.
Queue assignments are released on completion, cancellation, or departure;
remaining customers move forward.

`NpcMotion` is shared by ordinary routes, errands, and hostile pursuit. Swept
substeps keep bodies separate at low frame rates. Stable detours preserve the
original destination, and crowd waits do not trigger wall recovery. Departing
traffic receives room to pass. Stationary and incapacitated bodies remain
obstacles; separate floor heights do not collide. Player collision is unchanged.

## Verification

Focused suites cover conversation validation, carried items, greeting clocks,
admission fairness, campaign wording, shared acknowledgment, wire privacy,
late joining, direct/tray delivery gates, emergency behavior, and long lines
through both real doorways, including departure from the middle of a line.

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
