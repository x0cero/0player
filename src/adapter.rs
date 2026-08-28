//! Game-specific state adapters: read ground truth out of the game's RAM so
//! the model doesn't have to guess positions from pixels.

use crate::emu::Emulator;

/// FireRed/LeafGreen: gSaveBlock1Ptr lives at 0x03005008 and points into
/// EWRAM; the player's tile coordinates are the first two u16 fields.
const FRLG_SAVEBLOCK1_PTR: u32 = 0x0300_5008;

pub fn probe(emu: &mut Emulator) -> Option<String> {
    if !emu.title().starts_with("POKEMON FIRE") && !emu.title().starts_with("POKEMON LEAF") {
        return None;
    }
    let ptr = emu.gba_read32(FRLG_SAVEBLOCK1_PTR)?;
    if !(0x0200_0000..0x0204_0000).contains(&ptr) {
        return None; // not initialized yet (intro/title screen)
    }
    let x = emu.gba_read16(ptr)?;
    let y = emu.gba_read16(ptr + 2)?;
    if x > 1000 || y > 1000 {
        return None;
    }
    Some(format!("Player tile position: x={x}, y={y} (from game memory; if it did not change since last turn, your move was blocked by a wall or object)"))
}
