use rand::Rng;
use serde::{Deserialize, Serialize};

/// The deterministic part of the proof-of-concept character creation: Cairn
/// 2e attributes and hit protection. Background choice and starting equipment
/// deliberately wait for the player-agent creation conversation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CharacterSheet {
    pub hp: u8,
    pub str: u8,
    pub dex: u8,
    pub wil: u8,
}

impl CharacterSheet {
    pub fn roll_adventurer() -> Self {
        Self {
            hp: roll(1, 6),
            str: roll(3, 6),
            dex: roll(3, 6),
            wil: roll(3, 6),
        }
    }
}

fn roll(dice: u8, sides: u8) -> u8 {
    (0..dice).map(|_| rand::rng().random_range(1..=sides)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adventurer_rolls_follow_cairn_starting_ranges() {
        for _ in 0..64 {
            let sheet = CharacterSheet::roll_adventurer();
            assert!((1..=6).contains(&sheet.hp));
            assert!((3..=18).contains(&sheet.str));
            assert!((3..=18).contains(&sheet.dex));
            assert!((3..=18).contains(&sheet.wil));
        }
    }
}
