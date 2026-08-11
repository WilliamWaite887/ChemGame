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