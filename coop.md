cargo run --release -- --host                    # you
cargo run --release -- --join 192.168.68.53      # them


The host prints the exact line to send: lab open — the other chemist runs: chemgame --join 192.168.68.53:5327. A bare IP or a hostname both work; the port fills itself in. You'll likely need to allow UDP 5327 through Windows Firewall the first time.

What was actually broken

The handshake had worked for a while, which hid the fact that a joining client couldn't play. Six things were wrong and none of them errored — the client connected, got a chemist, and stood in an empty room:

The host bound 127.0.0.1. A second window on this machine worked; nothing else on the network could get in. That failure looks exactly like a firewall problem. It now binds 0.0.0.0 and advertises both the routable address and loopback, since netcode matches the address the client dialled against that list.
Authority was ClientState::Disconnected, which a joining client reads as true while handshaking. The lab is built on entering Playing, so whenever assets loaded first, the client built its own lab and then received the server's on top. Now net::is_authority, derived from LaunchMode — a launch-time fact, not a connection-state guess.
spawn_equipment was never gated at all, so the client raycast its own machines and sent the server entity ids it had never issued.
Meshes are built at spawn time, and meshes don't replicate — every replicated beaker, plant, crew member and machine arrived invisible. Split spawn (authority, state only) from dress (everywhere, on Added<T>, derived from the replicated kind). hazards already worked this way; that was the model.
The client's own chemist had no Look, Focus or InteractionMode — all deliberately local, so replication never delivered them, and every system that walks, aims or uses anything filters on them.
InteractionMode::UsingMachine was set server-side on a local component, so a client's panel never opened. The server now confirms a claim with MachineOpened and honours LeaveMachineRequested.
Two more surfaced from tests rather than the run: chemist body parts needed an explicit Visibility (a replicated chemist has none, and a parent without it can't propagate — Bevy calls this a warning), and Knowledge/Shift/RadioLog broadcast only on change, so a client joining between two discoveries ran on its own save.ron indefinitely. That last one was the nastiest: two chemists reading different books, with no symptom but a recipe one can make and the other can't.

229 tests, clippy clean. The new integration test drives a real replicon pair and asserts the client ends up with a dressed machine, beaker and chemist — I mutation-checked it by deleting .replicate::<Player>(), which fails it.

Worth knowing: you committed mid-session as ec8ac49 work on coop, so everything above is already in that commit. I also updated the plan file's M7 section.

Two things I did not do: there's still no in-game host/join menu, so your friend needs a terminal — say the word and I'll add one. And nobody has played a full shift with two humans yet; connection, visibility and input are confirmed, but the order → fabricate → hand off → radio loop hasn't been run by two people at once.

## The in-game menu, and what broke when Steam arrived

The in-game host/join menu did get built, alongside Steam Networking Sockets
support (`src/net/steam.rs`) — Steam is what a real player actually uses: no
address to type, no port to forward, no NAT problem. The CLI flags above still
work exactly as documented and are the direct/LAN transport, kept around
specifically for two-terminal testing without Steam running.

What broke: the menu's **Host** button was wired to Steam, but **Join**
(typed address) was left on the old direct/LAN transport. Click Host, have a
friend type your IP into Join, and the friend's packets dial a UDP port your
process never opened — Steam and direct/LAN are two separate, non-interoperable
transports that happened to share one menu. The host saw no logs at all
(nothing was listening), and the joining client sat on "attempting to join"
forever, with no timeout ever surfacing and the full simulation already
running client-side against a connection that didn't exist — which is what
made it look like a freeze and spike CPU.

Fixed by:
- Reworded the Join screen so it reads as the LAN/dev path it actually is,
  paired with `--host`, not with the menu's Steam-backed Host button.
- Added `AppState::Connecting`: clicking Connect (or accepting a Steam
  invite) no longer jumps straight into `Playing`. It waits — with a
  lightweight "Connecting…" screen and a Cancel button, no lab, no gameplay
  systems running — until `ClientState::Connected` actually fires, which
  both transports produce identically.
- A failed or timed-out handshake (`ClientState::Disconnected`, or Steam
  unavailable, or a synchronous socket/transport error) now writes a
  `ConnectFailed` message that bounces the player back to the mode screen
  with the reason on screen, instead of leaving `Connecting` with nothing
  watching it.
- `menu::hint_for` (the live "connects to …" line while typing an address)
  stopped calling DNS resolution on every keystroke — it was blocking the
  whole app on `getaddrinfo` for nearly every partial string typed. It now
  only recognises literal IPs live; a hostname resolves once, on the actual
  Connect click, same as before.