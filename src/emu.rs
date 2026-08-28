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

pub struct Emulator {
    cpu: Cpu,
    title: String,
}

impl Emulator {
    pub fn new(rom_path: &str) -> std::io::Result<Self> {
        let cart = Cartridge::load(rom_path)?;
        let title = cart.title();
        let bus = Bus::new(cart);
        Ok(Self { cpu: Cpu::new(bus), title })
    }

    pub fn title(&self) -> String {
        self.title.clone()
    }

    /// Run exactly `n` frames of emulated time.
    pub fn run_frames(&mut self, n: u32) {
        for _ in 0..n {
            let mut cycles = 0u32;
            while cycles < CYCLES_PER_FRAME {
                let m = self.cpu.step();
                self.cpu.bus.tick(m * 4);
                cycles += m * 4;
            }
        }
    }

    /// Hold a button for `hold` frames, release, then run `settle` frames so
    /// the game can react before the next screenshot.
    pub fn press(&mut self, button: Button, hold: u32, settle: u32) {
        self.set_button(button, true);
        self.run_frames(hold);
        self.set_button(button, false);
        self.run_frames(settle);
    }

    fn set_button(&mut self, button: Button, down: bool) {
        // Active-low: 0 = pressed. joy_buttons bits 3-0 = start, select, B, A;
        // joy_dpad bits 3-0 = down, up, left, right.
        let (field, bit): (&mut u8, u8) = match button {
            Button::A => (&mut self.cpu.bus.joy_buttons, 0),
            Button::B => (&mut self.cpu.bus.joy_buttons, 1),
            Button::Select => (&mut self.cpu.bus.joy_buttons, 2),
            Button::Start => (&mut self.cpu.bus.joy_buttons, 3),
            Button::Right => (&mut self.cpu.bus.joy_dpad, 0),
            Button::Left => (&mut self.cpu.bus.joy_dpad, 1),
            Button::Up => (&mut self.cpu.bus.joy_dpad, 2),
            Button::Down => (&mut self.cpu.bus.joy_dpad, 3),
        };
        if down {
            *field &= !(1 << bit);
        } else {
            *field |= 1 << bit;
        }
    }

    pub fn read_memory(&self, addr: u16) -> u8 {
        self.cpu.bus.read(addr)
    }

    /// PNG screenshot scaled up by an integer factor (vision models read the
    /// tiny 160x144 frame much better at 3-4x).
    pub fn screenshot_png(&self, scale: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(WIDTH as u32 * scale, HEIGHT as u32 * scale);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let px = self.cpu.bus.ppu.framebuffer[y * WIDTH + x];
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
