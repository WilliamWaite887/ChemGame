//! ChemGame — a focused recreation of the Space Station 13/14 chemist.

mod addiction;
mod analysis_reports;
mod antagonist;
mod arc;
#[cfg(feature = "packed-assets")]
mod asset_pack_format;
mod audio;
mod bent_guard;
mod body;
mod botanist;
/// Screenshot-capture dev tool: HUD hide + free camera. See its own doc
/// comment for why this is safe to toggle mid-session.
mod capture;
mod character_lab;
mod chem_data;
mod chem_world;
mod containers;
mod crew;
mod crisis;
mod cult;
mod door;
mod ending;
mod estrangement;
/// Cargo's conveyor line. Map-driven, so it has nothing to build without one.
#[cfg(feature = "trenchbroom")]
mod freight;
mod fx;
mod hazards;
mod inspection;
mod instability;
mod interaction;
mod knowledge;
mod lab;
/// Writing on the bottle: what a container claims to be, as distinct from
/// what it is.
mod labels;
mod machines;
mod menu;
mod nav;
mod net;
mod npc_motion;
mod obsessed;
mod order_intake;
mod orders;
mod player;
mod produce;
mod quack;
mod radio;
#[cfg(feature = "packed-assets")]
mod release_assets;
mod rogue_security;
mod saboteur;
mod saves;
mod security;
mod security_case;
mod session;
mod settings;
mod shift;
mod showdown;
mod smuggler;
mod social;
mod speech;
mod textbook;
mod threat;
mod tutorial;
mod ui;
pub mod utility_ai;
mod voice;
mod world_state;

use bevy::prelude::*;

fn main() {
    let mut app = App::new();

    // A Steam build has no loose fallback for original data, maps, models or
    // textures. Credited sounds remain normal files under assets/sounds.
    // Register before DefaultPlugins installs AssetPlugin.
    #[cfg(feature = "packed-assets")]
    release_assets::register(&mut app);

    // A profile identity belongs to this installation, not to a save slot or
    // one network connection. Load it before networking can begin.
    app.insert_resource(net::LocalAccount::load_or_create());

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

    app.add_plugins(
        DefaultPlugins
            // TrenchBroom's station surfaces are deliberately low-resolution
            // sprite art. Keep their texels crisp instead of blurring panel
            // seams and hazard marks between pixels.
            .set(ImagePlugin::default_nearest())
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "ChemGame — Chemistry Lab".into(),
                    ..default()
                }),
                ..default()
            })
            // Mirrors every warn!/error! onto the co-op debug log
            // (net::diagnostics), so a networking bug report carries the
            // actual failure line, not just "a client disconnected". Has to
            // be set here, not in `NetPlugin`: by the time any plugin
            // builds, `LogPlugin`'s global subscriber already exists.
            .set(bevy::log::LogPlugin {
                custom_layer: net::diagnostics::tracing_layer,
                ..default()
            }),
    )
    .init_state::<AppState>();

    // Before the plugins, so `NetPlugin` sees a mode already chosen and the
    // loader knows whether there is a menu to show.
    net::apply_command_line(&mut app);

    app.add_plugins((
        (
            net::NetPlugin,
            // The live lab snapshot is separate from career progress. It reads
            // the slot before player/glassware OnEnter systems decide whether
            // to spawn defaults, then preserves the authority-owned world.
            world_state::WorldStatePlugin,
        ),
        chem_data::ChemDataPlugin,
        lab::LabPlugin,
        // After the lab: it rebuilds off `WalkableAreas`, which the lab owns.
        nav::NavPlugin,
        machines::MachinePlugin,
        (
            containers::ContainerPlugin,
            inspection::InspectionPlugin,
            analysis_reports::AnalysisReportPlugin,
        ),
        (
            crew::CrewPlugin,
            // Registered after crew because utility actions reuse its movement
            // and arrival contracts. No resident is opted in by default yet,
            // so this remains an inert migration seam until the Cargo pilot.
            utility_ai::UtilityAiPlugin,
        ),
        knowledge::KnowledgePlugin,
        (
            orders::OrderPlugin,
            order_intake::OrderIntakePlugin,
            npc_motion::NpcMotionPlugin,
        ),
        // Nested so the outer tuple stays inside Bevy's 16-plugin limit.
        // The phase machine, the supply it schedules, and its save file.
        (
            shift::ShiftPlugin,
            shift::RestockPlugin,
            shift::ProgressPlugin,
            // What it costs to leave the sign down. After `ShiftPlugin`,
            // which owns the sign it watches.
            shift::ImpatiencePlugin,
        ),
        produce::ProducePlugin,
        // The station's two talking channels, nested together to stay inside
        // Bevy's 16-plugin tuple limit. `speech` is the radio's sibling: what
        // the crew say out loud where they are standing, rather than what the
        // station reports about them half a minute later. After `radio` for
        // the same reason `radio` is after `orders` — it reacts to
        // `OrderResolved` — and after `crew`, whose `Errand` it watches.
        (radio::RadioPlugin, speech::SpeechPlugin),
        (
            // The campaign spine. First in this tuple: it owns the
            // `Campaign` every thread below either gates on or feeds, and
            // `arc::is_active` has to see an assigned campaign before a
            // main antagonist's own thread decides whether to run.
            arc::ArcPlugin,
            // The crew-instability meter. Early, alongside `arc`, since it
            // is fed by name from several plugins below (`estrangement`,
            // `security`, `rogue_security`, `smuggler`, `saboteur`, `quack`,
            // `obsessed`) and the `Instability` resource has to exist before
            // any of them can nudge it.
            (
                instability::InstabilityPlugin,
                // Career-persistent personalities, favors, and the one
                // resident who may replace a department minor's outsider.
                social::SocialPlugin,
            ),
            // The hidden antagonist thread. After orders/shift/radio,
            // since it leans on `Order`, `current_rules` and
            // `PendingBroadcasts`.
            antagonist::AntagonistPlugin,
            // What dealing actually buys you, and what it costs. After
            // `antagonist`, whose illicit-resolution handler it leans on
            // for every consequence except the payment itself.
            addiction::AddictionPlugin,
            // Security's response to it. After antagonist, since it
            // reads the suspicion antagonist builds, and since
            // antagonist's Spy-flavoured sting arms its `RaidSchedule`
            // directly.
            (security::SecurityPlugin, security_case::SecurityCasePlugin),
            // The station-wide fallout of it. After antagonist, since it
            // reads `UnderworldStanding`; independent of security.
            crisis::CrisisPlugin,
            // Security, gone bad — a wholly separate threat from the
            // antagonist thread, arming off `Department::Security`
            // standing rather than anything hidden.
            rogue_security::RogueSecurityPlugin,
            // A burned personal relationship, gone bad the same way. After
            // `antagonist`, whose `SecuritySuspicion` it feeds a bump into
            // on estrangement.
            estrangement::EstrangementPlugin,
            // The department minors: one per department, running in
            // every save regardless of which main antagonist was drawn.
            // Security's legacy state remains for earned rewards and saves;
            // the unified Reyes case pilot owns new corruption incidents.
            // The department minors, grouped: one per department, all running
            // in every save. Nested because `add_plugins` tuples cap at 15.
            (
                obsessed::ObsessedPlugin,
                smuggler::SmugglerPlugin,
                saboteur::SaboteurPlugin,
                quack::QuackPlugin,
                botanist::BotanistPlugin,
            ),
            // After `security`, whose `Requisition::raid_wards` a sale to
            // him banks, and after `antagonist`, whose illicit-resolution
            // handler charges that sale the ordinary way.
            bent_guard::BentGuardPlugin,
            // A main antagonist, not a minor: gated on the save having
            // drawn the Cult. Cargo's minor is `smuggler`, above.
            cult::CultPlugin,
            // The arc's third ending. After `arc`, which owns the meter it
            // arms off, and after `hazards`-adjacent threads since a siege
            // vents through the ordinary smoke pipeline.
            showdown::ShowdownPlugin,
        ),
        // The chemist themselves: who they are, what they are looking at,
        // and what the chemistry is doing to them.
        (
            player::PlayerPlugin,
            interaction::InteractionPlugin,
            // After `interaction`, whose `InteractionMode` it adds a variant
            // to and whose cursor handling it relies on.
            labels::LabelPlugin,
            chem_world::ChemWorldPlugin,
            body::BodyPlugin,
            // Debug builds get the imported character, sample rack and reset
            // loop used to develop status-driven rigs. The plugin is inert in
            // release builds.
            character_lab::CharacterLabPlugin,
            hazards::HazardPlugin,
            // What the chemistry above does to the camera and the
            // models — reads `Bloodstream`, never mutates it.
            fx::FxPlugin,
            // After `crew` (above, outside this tuple) and `player`
            // (this tuple): the proximity trigger reads `Chemist` and
            // `CrewMember` alike to decide when to open.
            door::DoorPlugin,
        ),
        // `settings` owns the pause overlay as well as the knobs, and
        // reuses the menu's own shell to draw it — so it goes with them.
        (
            ui::UiPlugin,
            menu::MenuPlugin,
            settings::SettingsPlugin,
            // Draws on `settings`' overlay when the arc resolves, which is
            // why it goes here rather than with `arc`.
            ending::EndingPlugin,
            // Unwinds a session on the way out, so quitting to the menu
            // and opening another save does not inherit this one's career.
            session::SessionPlugin,
            textbook::TextbookPlugin,
            tutorial::TutorialPlugin,
            // Reads `Settings::master_volume` and every other module's
            // state/messages; nothing reads it back. Goes last for that
            // reason, not because build order matters — every plugin
            // above has already registered whatever message type it
            // reacts to before any system actually runs.
            audio::SfxPlugin,
            // After `audio`, for the same non-reason: by now `net`, `player`
            // and `settings` have all registered what they own, and voice
            // only reads from them, never the other way round.
            voice::VoicePlugin,
        ),
    ));

    // Also kept out of the tuple above, because a tuple element cannot carry a
    // `#[cfg]`. Everything it draws is assembled from `conveyor_spot` markers,
    // so a `--no-default-features` build has nothing for it to do and does not
    // compile it in at all.
    #[cfg(feature = "trenchbroom")]
    app.add_plugins(freight::FreightPlugin);

    // Kept out of the tuple above for the same "near the 16-plugin limit"
    // reason as `freight` — a dev-only screenshot tool, unrelated to any of
    // the groupings above, so it has no natural home in one of them anyway.
    app.add_plugins(capture::CapturePlugin);

    // Kept out of the tuple above (already near Bevy's 16-plugin limit) and
    // added only when Steam actually initialised — its systems assume
    // `SteamworksEvent` is registered, which only `SteamworksPlugin`
    // succeeding guarantees.
    if steam_ready {
        app.add_plugins(net::steam::SteamPlugin);
    }

    app.run();
}

/// Marks an entity as belonging to *this* visit to the lab.
///
/// Everything a session spawns — the room shell, the machines, the glassware,
/// the crew, the chemists, their cameras, the HUD — carries this, so leaving
/// for the main menu unwinds the whole world in one move rather than each of
/// fifteen modules owning a teardown system that could be forgotten.
///
/// A function rather than a constant because `DespawnOnExit` holds the state
/// value, and a helper is what makes the rule greppable: if you spawn a root
/// entity while `Playing` and it does not carry this, it will still be sitting
/// in the world when the next save opens. `spawned_for_this_session_is_cleaned_up`
/// in `src/net/mod.rs` is the guard on that.
///
/// Children need no marker of their own — despawning a root takes its
/// hierarchy with it.
pub fn until_we_leave_the_lab() -> DespawnOnExit<AppState> {
    DespawnOnExit(AppState::Playing)
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
