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
    let map = map_id(emu)
        .map(|m| format!(" Current map id: {m} (note map names as you learn them)."))
        .unwrap_or_default();

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
        return Some(format!("{}{}{}", position_line(x, y), map, line));
    }
    let mini = minimap(emu).unwrap_or_default();
    Some(format!("{}{}{}", position_line(x, y), map, mini))
}

fn position_line(x: u16, y: u16) -> String {
    format!(
        "Player tile position from game memory: x={x}, y={y}. \
         AXES: UP decreases y, DOWN increases y, LEFT decreases x, RIGHT increases x. \
         If the position did not change since last turn, that move was blocked by a wall or object; do not repeat it."
    )
}

/// Current map as "group.number"; pairs with notebook lessons like
/// "map 3.19 is Route 1" so the model learns its own geography.
pub fn map_id(emu: &mut Emulator) -> Option<String> {
    if !emu.title().starts_with("POKEMON FIRE") && !emu.title().starts_with("POKEMON LEAF") {
        return None;
    }
    let ptr = emu.gba_read32(FRLG_SAVEBLOCK1_PTR)?;
    if !(0x0200_0000..0x0204_0000).contains(&ptr) {
        return None;
    }
    let group = emu.gba_read16(ptr + 4)? & 0xFF;
    let num = (emu.gba_read16(ptr + 4)? >> 8) & 0xFF;
    Some(format!("{group}.{num}"))
}

/// FireRed's live bordered map grid: {s32 width, s32 height, u16 *tiles}.
/// Collision lives in bits 10-11 of each tile; the playable area is inset
/// 7 tiles into the border, so grid position = (x+7, y+7).
const FRLG_VMAP: u32 = 0x0300_5040;

pub struct WalkMap {
    pub width: i32,
    pub height: i32,
    tiles: Vec<u16>,
}

impl WalkMap {
    /// Is the map-coordinate tile walkable?
    pub fn walkable(&self, x: i32, y: i32) -> bool {
        let gx = x + 7;
        let gy = y + 7;
        if gx < 0 || gy < 0 || gx >= self.width || gy >= self.height {
            return false;
        }
        let t = self.tiles[(gy * self.width + gx) as usize];
        (t >> 10) & 3 == 0
    }
}

pub fn walk_map(emu: &mut Emulator) -> Option<WalkMap> {
    if !emu.title().starts_with("POKEMON FIRE") && !emu.title().starts_with("POKEMON LEAF") {
        return None;
    }
    let width = emu.gba_read32(FRLG_VMAP)? as i32;
    let height = emu.gba_read32(FRLG_VMAP + 4)? as i32;
    let ptr = emu.gba_read32(FRLG_VMAP + 8)?;
    if !(1..=200).contains(&width)
        || !(1..=200).contains(&height)
        || !(0x0200_0000..0x0204_0000).contains(&ptr)
    {
        return None;
    }
    let mut tiles = vec![0u16; (width * height) as usize];
    for (i, t) in tiles.iter_mut().enumerate() {
        *t = emu.gba_read16(ptr + 2 * i as u32)?;
    }
    Some(WalkMap { width, height, tiles })
}

/// ASCII walkability window centered on the player: rows north to south.
pub fn minimap(emu: &mut Emulator) -> Option<String> {
    let (px, py) = coords(emu)?;
    let map = walk_map(emu)?;
    let (px, py) = (px as i32, py as i32);
    let mut out = String::from(
        "\nWalkability map around you (from game memory; '#' blocked, '.' walkable, 'P' you; top row is NORTH/UP):\n",
    );
    for y in (py - 4)..=(py + 4) {
        for x in (px - 7)..=(px + 7) {
            out.push(if (x, y) == (px, py) {
                'P'
            } else if map.walkable(x, y) {
                '.'
            } else {
                '#'
            });
        }
        out.push('\n');
    }
    Some(out)
}

/// BFS path from the player to a map tile; returns per-step buttons.
pub fn find_path(emu: &mut Emulator, tx: i32, ty: i32) -> Result<Vec<crate::emu::Button>, String> {
    use crate::emu::Button;
    let (px, py) = coords(emu).ok_or("position unreadable")?;
    let map = walk_map(emu).ok_or("map unreadable")?;
    let (sx, sy) = (px as i32, py as i32);
    if !map.walkable(tx, ty) {
        return Err(format!("tile ({tx},{ty}) is not walkable"));
    }
    let w = map.width;
    let h = map.height;
    let idx = |x: i32, y: i32| ((y + 7) * w + (x + 7)) as usize;
    let mut prev: Vec<i32> = vec![-1; (w * h) as usize];
    let mut queue = std::collections::VecDeque::new();
    prev[idx(sx, sy)] = -2;
    queue.push_back((sx, sy));
    let dirs = [
        (0, -1, Button::Up),
        (0, 1, Button::Down),
        (-1, 0, Button::Left),
        (1, 0, Button::Right),
    ];
    while let Some((x, y)) = queue.pop_front() {
        if (x, y) == (tx, ty) {
            let mut steps = Vec::new();
            let (mut cx, mut cy) = (tx, ty);
            while (cx, cy) != (sx, sy) {
                let p = prev[idx(cx, cy)];
                let (pxx, pyy) = (p % w - 7, p / w - 7);
                let d = dirs
                    .iter()
                    .find(|(dx, dy, _)| (pxx + dx, pyy + dy) == (cx, cy))
                    .unwrap();
                steps.push(d.2);
                cx = pxx;
                cy = pyy;
            }
            steps.reverse();
            steps.truncate(40);
            return Ok(steps);
        }
        for (dx, dy, _) in dirs {
            let (nx, ny) = (x + dx, y + dy);
            if nx < -7 || ny < -7 || nx + 7 >= w || ny + 7 >= h {
                continue;
            }
            if map.walkable(nx, ny) && prev[idx(nx, ny)] == -1 {
                prev[idx(nx, ny)] = idx(x, y) as i32;
                queue.push_back((nx, ny));
            }
        }
    }
    Err(format!("no walkable path to ({tx},{ty}) on this map"))
}
