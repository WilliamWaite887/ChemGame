//! ChemGame.
//!
//! M2 replaces this with the bevy app. For now it exercises the chemistry
//! simulation from the real data files so the core is demonstrably working.

use chem_sim::{resolve, ChemData, Solution, Units};

const REAGENTS_RON: &str = include_str!("../assets/data/reagents.ron");
const REACTIONS_RON: &str = include_str!("../assets/data/reactions.ron");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = ChemData::from_ron(REAGENTS_RON, REACTIONS_RON)?;
    println!(
        "loaded {} reagents ({} dispensable) and {} reactions\n",
        data.reagents.len(),
        data.reagents.dispensable().count(),
        data.reactions.len()
    );

    // The three-step chain, in one beaker: dylovene -> hyronalin -> arithrazine.
    let mut beaker = Solution::new(Units::whole(300));
    for (reagent, amount) in [
        ("silicon", 15),
        ("potassium", 15),
        ("nitrogen", 15),
        ("radium", 45),
        ("hydrogen", 90),
    ] {
        let _ = beaker.add(data.reagent(reagent), Units::whole(amount));
    }

    println!("beaker in:  {}", describe(&data, &beaker));
    let report = resolve(&mut beaker, &data.reactions);
    println!("beaker out: {}", describe(&data, &beaker));

    println!("\nreactions fired:");
    for event in &report.events {
        let reaction = data.reactions.get(event.reaction);
        println!("  {:<14} at {}x", reaction.key, event.scale.as_f64());
    }

    Ok(())
}

fn describe(data: &ChemData, solution: &Solution) -> String {
    if solution.is_empty() {
        return "empty".to_string();
    }
    solution
        .iter()
        .map(|(id, qty)| format!("{} {}", qty, data.reagents.get(id).name))
        .collect::<Vec<_>>()
        .join(", ")
}
