//! Lab machines.
//!
//! M2 defines what a machine *is* and where it sits. M3 gives each one a
//! solution and a control panel.

use bevy::prelude::*;

pub struct MachinePlugin;

impl Plugin for MachinePlugin {
    fn build(&self, _app: &mut App) {
        // Behaviour lands in M3. The components below are needed now so the
        // lab can place machines.
    }
}

/// Which piece of equipment this is.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MachineKind {
    /// Produces base reagents on demand.
    Dispenser,
    /// Buffers, separates and packages into pills and bottles.
    ChemMaster,
    /// Grinds solids into reagent.
    Grinder,
    /// Reports the exact composition of a sample. The main route to
    /// reverse-engineering an unknown recipe (M6).
    Analyzer,
    /// Unlimited base reagents, but its output cannot be delivered for credit.
    /// Experimenting costs time, not materials (M6).
    TestBench,
    /// Where crew collect what they ordered (M4).
    DeliveryWindow,
}

impl MachineKind {
    pub fn label(self) -> &'static str {
        match self {
            MachineKind::Dispenser => "Chemical Dispenser",
            MachineKind::ChemMaster => "ChemMaster 4000",
            MachineKind::Grinder => "Reagent Grinder",
            MachineKind::Analyzer => "Sample Analyzer",
            MachineKind::TestBench => "Test Bench",
            MachineKind::DeliveryWindow => "Delivery Window",
        }
    }

    /// Colour of the machine's idle screen. Purely cosmetic, but it makes the
    /// lab readable at a glance.
    pub fn screen_color(self) -> Color {
        match self {
            MachineKind::Dispenser => Color::srgb(0.30, 0.75, 0.95),
            MachineKind::ChemMaster => Color::srgb(0.45, 0.90, 0.55),
            MachineKind::Grinder => Color::srgb(0.95, 0.65, 0.25),
            MachineKind::Analyzer => Color::srgb(0.85, 0.45, 0.95),
            MachineKind::TestBench => Color::srgb(0.95, 0.85, 0.35),
            MachineKind::DeliveryWindow => Color::srgb(0.95, 0.35, 0.35),
        }
    }
}

/// A machine's shared state.
///
/// `in_use_by` exists from the start on purpose: two chemists reaching for the
/// dispenser is the normal case in co-op, not an edge case, and retrofitting
/// occupancy after the fact means touching every interaction path.
#[derive(Component, Debug)]
pub struct Machine {
    pub kind: MachineKind,
    pub in_use_by: Option<Entity>,
}

impl Machine {
    pub fn new(kind: MachineKind) -> Self {
        Machine {
            kind,
            in_use_by: None,
        }
    }

    /// Whether `player` may open this machine's panel.
    pub fn available_to(&self, player: Entity) -> bool {
        match self.in_use_by {
            None => true,
            Some(current) => current == player,
        }
    }
}
