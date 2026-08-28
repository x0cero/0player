//! Headless wrapper around the gameboy core: run frames, press buttons,
//! grab PNG screenshots, peek memory.

use gameboy::bus::Bus;
use gameboy::cartridge::Cartridge;
use gameboy::cpu::Cpu;
use gameboy::ppu::{HEIGHT, WIDTH};
use gameboy::CYCLES_PER_FRAME;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    A,
    B,
    Start,
    Select,
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "A" => Some(Self::A),
            "B" => Some(Self::B),
            "START" => Some(Self::Start),
            "SELECT" => Some(Self::Select),
            "UP" => Some(Self::Up),
            "DOWN" => Some(Self::Down),
            "LEFT" => Some(Self::Left),
            "RIGHT" => Some(Self::Right),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::Start => "START",
            Self::Select => "SELECT",
            Self::Up => "UP",
            Self::Down => "DOWN",
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}

enum Core {
    /// Game Boy / Game Boy Color.
    Gb(Cpu),
    /// Game Boy Advance, driven through the gba crate's C FFI surface.
    Gba { handle: *mut std::ffi::c_void, keys: u16 },
}

// The GBA handle is only ever touched from the agent thread.
unsafe impl Send for Core {}

pub struct Emulator {
    core: Core,
    title: String,
}

impl Emulator {
    pub fn new(rom_path: &str) -> std::io::Result<Self> {
        if rom_path.to_ascii_lowercase().ends_with(".gba") {
            let rom = std::fs::read(rom_path)?;
            let title = String::from_utf8_lossy(&rom[0xA0..0xAC])
                .trim_end_matches(['\0', ' '])
                .to_string();
            let handle = gba::gba_create(rom.as_ptr(), rom.len(), std::ptr::null(), 0);
            Ok(Self {
                core: Core::Gba { handle, keys: 0x3FF },
                title,
            })
        } else {
            let cart = Cartridge::load(rom_path)?;
            let title = cart.title();
            let bus = Bus::new(cart);
            Ok(Self {
                core: Core::Gb(Cpu::new(bus)),
                title,
            })
        }
    }

    pub fn title(&self) -> String {
        self.title.clone()
    }

    /// A held d-pad press on GBA needs enough frames to take a full step;
    /// a short tap only turns the character in many games.
    pub fn is_gba(&self) -> bool {
        matches!(self.core, Core::Gba { .. })
    }

    /// Serialize full machine state (GBA only for now).
    pub fn state_save(&mut self) -> Option<Vec<u8>> {
        match &self.core {
            Core::Gba { handle, .. } => {
                // First call with a null buffer reports the required size.
                let need = gba::gba_state_save(*handle, std::ptr::null_mut(), 0);
                if need == 0 {
                    return None;
                }
                let mut buf = vec![0u8; need];
                let n = gba::gba_state_save(*handle, buf.as_mut_ptr(), buf.len());
                if n == 0 {
                    return None;
                }
                buf.truncate(n);
                Some(buf)
            }
            Core::Gb(_) => None,
        }
    }

    pub fn state_load(&mut self, data: &[u8]) -> bool {
        match &self.core {
            Core::Gba { handle, .. } => gba::gba_state_load(*handle, data.as_ptr(), data.len()),
            Core::Gb(_) => false,
        }
    }

    /// Read GBA memory (the FFI handle is a boxed gba::cpu::Cpu, same crate).
    pub fn gba_read32(&mut self, addr: u32) -> Option<u32> {
        match &mut self.core {
            Core::Gba { handle, .. } => {
                let cpu = unsafe { &mut *(*handle as *mut gba::cpu::Cpu) };
                Some(cpu.bus.read32(addr))
            }
            Core::Gb(_) => None,
        }
    }

    pub fn gba_read16(&mut self, addr: u32) -> Option<u16> {
        match &mut self.core {
            Core::Gba { handle, .. } => {
                let cpu = unsafe { &mut *(*handle as *mut gba::cpu::Cpu) };
                Some(cpu.bus.read16(addr))
            }
            Core::Gb(_) => None,
        }
    }

    /// Run exactly `n` frames of emulated time.
    pub fn run_frames(&mut self, n: u32) {
        match &mut self.core {
            Core::Gb(cpu) => {
                for _ in 0..n {
                    let mut cycles = 0u32;
                    while cycles < CYCLES_PER_FRAME {
                        let m = cpu.step();
                        cpu.bus.tick(m * 4);
                        cycles += m * 4;
                    }
                }
            }
            Core::Gba { handle, keys } => {
                for _ in 0..n {
                    gba::gba_run_frame(*handle, *keys);
                }
            }
        }
    }

    pub fn hold(&mut self, button: Button) {
        self.set_button(button, true);
    }

    pub fn release(&mut self, button: Button) {
        self.set_button(button, false);
    }

    fn set_button(&mut self, button: Button, down: bool) {
        match &mut self.core {
            Core::Gb(cpu) => {
                // Active-low: 0 = pressed. joy_buttons bits 3-0 = start,
                // select, B, A; joy_dpad bits 3-0 = down, up, left, right.
                let (field, bit): (&mut u8, u8) = match button {
                    Button::A => (&mut cpu.bus.joy_buttons, 0),
                    Button::B => (&mut cpu.bus.joy_buttons, 1),
                    Button::Select => (&mut cpu.bus.joy_buttons, 2),
                    Button::Start => (&mut cpu.bus.joy_buttons, 3),
                    Button::Right => (&mut cpu.bus.joy_dpad, 0),
                    Button::Left => (&mut cpu.bus.joy_dpad, 1),
                    Button::Up => (&mut cpu.bus.joy_dpad, 2),
                    Button::Down => (&mut cpu.bus.joy_dpad, 3),
                };
                if down {
                    *field &= !(1 << bit);
                } else {
                    *field |= 1 << bit;
                }
            }
            Core::Gba { keys, .. } => {
                // KEYINPUT, active-low: 0 A, 1 B, 2 Select, 3 Start,
                // 4 Right, 5 Left, 6 Up, 7 Down.
                let bit = match button {
                    Button::A => 0,
                    Button::B => 1,
                    Button::Select => 2,
                    Button::Start => 3,
                    Button::Right => 4,
                    Button::Left => 5,
                    Button::Up => 6,
                    Button::Down => 7,
                };
                if down {
                    *keys &= !(1 << bit);
                } else {
                    *keys |= 1 << bit;
                }
            }
        }
    }

    fn screen(&self) -> (&[u32], usize, usize) {
        match &self.core {
            Core::Gb(cpu) => (&cpu.bus.ppu.framebuffer, WIDTH, HEIGHT),
            Core::Gba { handle, .. } => {
                let ptr = gba::gba_framebuffer(*handle);
                let fb = unsafe {
                    std::slice::from_raw_parts(ptr, gba::ppu::WIDTH * gba::ppu::HEIGHT)
                };
                (fb, gba::ppu::WIDTH, gba::ppu::HEIGHT)
            }
        }
    }

    /// PNG screenshot scaled up by an integer factor (vision models read the
    /// tiny native frame much better at 3-4x).
    pub fn screenshot_png(&self, scale: u32) -> Vec<u8> {
        let (fb, w, h) = self.screen();
        let mut img = image::RgbImage::new(w as u32 * scale, h as u32 * scale);
        for y in 0..h {
            for x in 0..w {
                let px = fb[y * w + x];
                let rgb = image::Rgb([(px >> 16) as u8, (px >> 8) as u8, px as u8]);
                for dy in 0..scale {
                    for dx in 0..scale {
                        img.put_pixel(x as u32 * scale + dx, y as u32 * scale + dy, rgb);
                    }
                }
            }
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png encode");
        out
    }
}
