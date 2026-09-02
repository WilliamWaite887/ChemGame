# Conversation-first orders verification

Checked on September 2, 2026, using the authored TrenchBroom station.

## Automated checks

- `cargo test --workspace`: 1,241 passing tests, including 1,052 game tests.
- Conversation checks cover hidden payloads, carried beakers, reopen timing,
  one-time greeting expiry, full preparation allowance, stale IDs, reach and
  wall validation, concurrent acknowledgments, and preparation during movement.
- Intake checks cover shared reservations, FIFO admission among eligible
  generators, and more than five accepted requests.
- Campaign checks cover neutral authored pools and reminders, actual department
  support requests, and undercover requests. Ordinary requests remain ordinary.
- Actual Replicon serialization and delivery exercise private conversation
  replies, shared acceptance, and late-joining waiting/accepted states.
- Delivery tests exercise both handovers and trays against unaccepted requests,
  accepted customers walking to pickup, and existing emergency behavior.
- Fourteen-person Public and Medical lines form through the authored doors;
  middle customers leave and remaining customers advance. Movement tests cover
  swept collision, opposing traffic at low frame rates, stationary obstacles,
  floor separation, and existing wall recovery.
- `cargo clippy --workspace --all-targets` completes with warnings: 71 for the
  game target and 65 for its test target (62 shared). These include existing
  unused items and complex Bevy system signatures; this is not a warning-free
  workspace.

Logs are under `target/intake-complete-tests.log` and
`target/intake-complete-clippy.log`.

## Rendered solo and LAN co-op scenarios

The opt-in debug scenario was run in isolated application-data directories.
Screenshots were inspected for the conversation, order directory, pickup line,
and combined standing board at 1280 by 720.

| Check | Solo | LAN host/client |
|---|---|---|
| Accepted requests after conversation | 11 | 11 on both peers |
| Remaining unaccepted requests | 0 | 0 on both peers |
| Customers reaching their pickup positions | 11 | 11 on authority |
| Closest same-floor NPC centers at report time | 0.869 m | 0.867 m on both peers |
| Who accepted the eleventh request | Local player | Client |

Solo screenshots show the large beaker retained through the conversation and
acceptance. Both co-op directories show the shared eleven orders. The line
continues through the lobby doorway into the corridor, leaving crossing gaps.
The HUD displays five accepted requests and the additional count.

Screenshots and reports: `target/order-playtest/{solo,host,client}`.
These were automated rendered scenarios; they do not substitute for a human
assessment of pacing over a full shift. Middle-line delivery and departure were
exercised by the movement/queue tests, rather than scripted in these screenshots.

## Remaining diagnostics

The LAN client logged 54 Bevy `B0004` hierarchy warnings during initial scene
replication. Order replication and conversation acceptance completed, with
15–21 ms measured RTT and 0% reported packet loss. These hierarchy warnings
remain unresolved. The final client disconnect message coincides with the
scenario's automatic host shutdown.

Steam was unavailable in this environment, so the two-process playtest used
LAN transport. Steam transport was not playtested in this pass.

The rendered pass also exposed and fixed a chemistry-loading race: the first
asset could be removed before the second asset had loaded. A regression test
now ensures partial loading preserves the first result.
