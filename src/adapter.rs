//! Game-specific state adapters: read ground truth out of the game's RAM so
//! the model doesn't have to guess positions from pixels.

use crate::emu::Emulator;

/// FireRed/LeafGreen: gSaveBlock1Ptr lives at 0x03005008 and points into
/// EWRAM; the player's tile coordinates are the first two u16 fields.
const FRLG_SAVEBLOCK1_PTR: u32 = 0x0300_5008;

/// Raw tile coordinates, when readable.
pub fn coords(emu: &mut Emulator) -> Option<(u16, u16)> {
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
    Some((x, y))
}

/// FireRed party arrays. The first 80 bytes of a party entry are encrypted,
/// but status/level/HP live unencrypted at the tail of the 100-byte struct.
const FRLG_PLAYER_PARTY: u32 = 0x0202_4284;
const FRLG_ENEMY_PARTY: u32 = 0x0202_402C;

fn mon_stats(emu: &mut Emulator, base: u32) -> Option<(u8, u16, u16)> {
    let level = emu.gba_read16(base + 84)? as u8;
    let hp = emu.gba_read16(base + 86)?;
    let max_hp = emu.gba_read16(base + 88)?;
    if level == 0 || level > 100 || max_hp == 0 || max_hp > 999 || hp > max_hp {
        return None;
    }
    Some((level, hp, max_hp))
}

pub fn probe(emu: &mut Emulator) -> Option<String> {
    let (x, y) = coords(emu)?;

    // Battle telemetry when both sides look sane.
    if let Some((lv, hp, max)) = mon_stats(emu, FRLG_PLAYER_PARTY) {
        let mut line = format!(
            "\nYour lead Pokemon: level {lv}, HP {hp}/{max}.{}",
            if hp == 0 { " It has FAINTED." } else { "" }
        );
        if let Some((elv, ehp, emax)) = mon_stats(emu, FRLG_ENEMY_PARTY) {
            line.push_str(&format!(
                " Enemy Pokemon: level {elv}, HP {ehp}/{emax}.{}",
                if ehp == 0 { " It fainted; you won." } else { "" }
            ));
        }
        return Some(format!("{}{}", position_line(x, y), line));
    }
    Some(position_line(x, y))
}

fn position_line(x: u16, y: u16) -> String {
    format!(
        "Player tile position from game memory: x={x}, y={y}. \
         AXES: UP decreases y, DOWN increases y, LEFT decreases x, RIGHT increases x. \
         If the position did not change since last turn, that move was blocked by a wall or object; do not repeat it."
    )
}
