//! ChemGame — a focused recreation of the Space Station 13/14 chemist.

mod antagonist;
mod body;
mod chem_data;
mod containers;
mod crew;
mod crisis;
mod cult;
mod fx;
mod hazards;
mod interaction;
mod knowledge;
mod lab;
mod machines;
mod menu;
mod net;
mod obsessed;
mod orders;
mod player;
mod produce;
mod radio;
mod rogue_security;
mod saves;
mod security;
mod shift;
mod ui;

use bevy::prelude::*;

fn main() {
    let mut app = App::new();

    // Before `DefaultPlugins`: `bevy_steamworks` requires this, because it
    // must be up before the render plugin inside `DefaultPlugins` builds.
    // Not fatal on failure (Steam not running, App ID mismatch, no SDK) —
    // LAN/direct play (`--host`/`--join`, and every headless test) works
    // with no Steam dependency at all. `net::warn_if_steam_unavailable`
    // reports the failure in-game if someone then tries `LaunchMode::
    // HostSteam`/`JoinSteam` anyway, rather than that silently doing
    // nothing.
    let steam_ready = match bevy_steamworks::SteamworksPlugin::init_app(net::steam::STEAM_APP_ID) {
        Ok(steam) => {
            app.add_plugins(steam);
            true
        }
        Err(e) => {
            // `LogPlugin` (part of `DefaultPlugins`, added next) is not up
            // yet, so `info!`/`error!` have nothing to write to.
            eprintln!(
                "Steam did not initialise ({e:?}) — Steam co-op will be \
                 unavailable this session. Is Steam running, and does the \
                 App ID match (steam_appid.txt in dev, App ID {} on the \
                 Steamworks dashboard)?",
                net::steam::STEAM_APP_ID
            );
            false
        }
    };

    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "ChemGame — Chemistry Lab".into(),
            ..default()
        }),
        ..default()
    }))
    .init_state::<AppState>();

    // Before the plugins, so `NetPlugin` sees a mode already chosen and the
    // loader knows whether there is a menu to show.
    net::apply_command_line(&mut app);

    app.add_plugins((
            net::NetPlugin,
            chem_data::ChemDataPlugin,
            lab::LabPlugin,
            machines::MachinePlugin,
            containers::ContainerPlugin,
            crew::CrewPlugin,
            knowledge::KnowledgePlugin,
            orders::OrderPlugin,
            // Nested so the outer tuple stays inside Bevy's 16-plugin limit.
            // The phase machine, the supply it schedules, and its save file.
            (
                shift::ShiftPlugin,
                shift::RestockPlugin,
                shift::ProgressPlugin,
            ),
            produce::ProducePlugin,
            radio::RadioPlugin,
            (
                // The hidden antagonist thread. After orders/shift/radio,
                // since it leans on `Order`, `current_rules` and
                // `PendingBroadcasts`.
                antagonist::AntagonistPlugin,
                // Security's response to it. After antagonist, since it
                // reads the suspicion antagonist builds, and since
                // antagonist's Spy-flavoured sting arms its `RaidSchedule`
                // directly.
                security::SecurityPlugin,
                // The station-wide fallout of it. After antagonist, since it
                // reads `UnderworldStanding`; independent of security.
                crisis::CrisisPlugin,
                // Security, gone bad — a wholly separate threat from the
                // antagonist thread, arming off `Department::Security`
                // standing rather than anything hidden.
                rogue_security::RogueSecurityPlugin,
                // Two more crew-visit archetypes, same shape as antagonist,
                // each independent of every other in this tuple.
                obsessed::ObsessedPlugin,
                cult::CultPlugin,
            ),
            // The chemist themselves: who they are, what they are looking at,
            // and what the chemistry is doing to them.
            (
                player::PlayerPlugin,
                interaction::InteractionPlugin,
                body::BodyPlugin,
                hazards::HazardPlugin,
                // What the chemistry above does to the camera and the
                // models — reads `Bloodstream`, never mutates it.
                fx::FxPlugin,
            ),
            (ui::UiPlugin, menu::MenuPlugin),
        ));

    // Kept out of the tuple above (already near Bevy's 16-plugin limit) and
    // added only when Steam actually initialised — its systems assume
    // `SteamworksEvent` is registered, which only `SteamworksPlugin`
    // succeeding guarantees.
    if steam_ready {
        app.add_plugins(net::steam::SteamPlugin);
    }

    app.run();
}

#[derive(States, Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
pub enum AppState {
    /// Waiting on the chemistry data files. `ChemDataPlugin` moves us on once
    /// they have parsed.
    #[default]
    Loading,
    /// Picking how to play and which save. Skipped when the command line
    /// already said.
    MainMenu,
    /// Dialling out, over whichever transport `LaunchMode` picked. No lab, no
    /// gameplay systems run here — every such system is gated on `Playing`
    /// specifically, so simply not being `Playing` yet is what keeps a
    /// doomed or slow handshake cheap instead of running the full simulation
    /// uncapped while nothing is connected. Only `Join`/`JoinSteam` pass
    /// through here; `Host`/`HostSteam`/`Singleplayer` own the simulation
    /// immediately, since they have nothing to wait for.
    Connecting,
    Playing,
}
