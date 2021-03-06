use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::bus::*;

pub mod header;
use header::CartridgeHeader;

mod backup;
use backup::eeprom::EepromController;
use backup::flash::Flash;
pub use backup::BackupType;
use backup::{BackupFile, BackupMemoryInterface};

mod gpio;
mod rtc;
use gpio::Gpio;

mod builder;
mod loader;
pub use builder::GamepakBuilder;

pub const GPIO_PORT_DATA: u32 = 0xC4;
pub const GPIO_PORT_DIRECTION: u32 = 0xC6;
pub const GPIO_PORT_CONTROL: u32 = 0xC8;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum BackupMedia {
    Sram(BackupFile),
    Flash(Flash),
    Eeprom(EepromController),
    Undetected,
}

pub type SymbolTable = HashMap<String, u32>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Cartridge {
    pub header: CartridgeHeader,
    bytes: Box<[u8]>,
    size: usize,
    gpio: Option<Gpio>,
    symbols: Option<SymbolTable>, // TODO move it somewhere else
    pub(in crate) backup: BackupMedia,
}

impl Cartridge {
    pub fn get_symbols(&self) -> &Option<SymbolTable> {
        &self.symbols
    }
    pub fn get_gpio(&self) -> &Option<Gpio> {
        &self.gpio
    }
}

use super::sysbus::consts::*;

pub const EEPROM_BASE_ADDR: u32 = 0x0DFF_FF00;

fn is_gpio_access(addr: u32) -> bool {
    match addr & 0x1ff_ffff {
        GPIO_PORT_DATA | GPIO_PORT_DIRECTION | GPIO_PORT_CONTROL => true,
        _ => false,
    }
}

impl Bus for Cartridge {
    fn read_8(&self, addr: Addr) -> u8 {
        let offset = (addr & 0x01ff_ffff) as usize;
        match addr & 0xff000000 {
            SRAM_LO | SRAM_HI => match &self.backup {
                BackupMedia::Sram(memory) => memory.read((addr & 0x7FFF) as usize),
                BackupMedia::Flash(flash) => flash.read(addr),
                _ => 0,
            },
            _ => {
                if offset >= self.size {
                    0xDD // TODO - open bus implementation
                } else {
                    unsafe { *self.bytes.get_unchecked(offset as usize) }
                }
            }
        }
    }

    fn read_16(&self, addr: u32) -> u16 {
        if let Some(gpio) = &self.gpio {
            if is_gpio_access(addr) {
                if !(gpio.is_readable()) {
                    warn!("trying to read GPIO when reads are not allowed");
                }
                return gpio.read(addr & 0x1ff_ffff);
            }
        }

        if addr & 0xff000000 == GAMEPAK_WS2_HI
            && (self.bytes.len() <= 16 * 1024 * 1024 || addr >= EEPROM_BASE_ADDR)
        {
            if let BackupMedia::Eeprom(spi) = &self.backup {
                return spi.read_half(addr);
            }
        }
        self.default_read_16(addr)
    }

    fn write_8(&mut self, addr: u32, value: u8) {
        match addr & 0xff000000 {
            SRAM_LO | SRAM_HI => match &mut self.backup {
                BackupMedia::Flash(flash) => flash.write(addr, value),
                BackupMedia::Sram(memory) => memory.write((addr & 0x7FFF) as usize, value),
                _ => {}
            },
            _ => {} // TODO allow the debugger to write
        };
    }

    fn write_16(&mut self, addr: u32, value: u16) {
        if let Some(gpio) = &mut self.gpio {
            if is_gpio_access(addr) {
                gpio.write(addr & 0x1ff_ffff, value);
                return;
            }
        }

        if addr & 0xff000000 == GAMEPAK_WS2_HI
            && (self.bytes.len() <= 16 * 1024 * 1024 || addr >= EEPROM_BASE_ADDR)
        {
            if let BackupMedia::Eeprom(spi) = &mut self.backup {
                return spi.write_half(addr, value);
            }
        }
        self.default_write_16(addr, value);
    }
}

impl DebugRead for Cartridge {
    fn debug_read_8(&self, addr: Addr) -> u8 {
        let offset = (addr & 0x01ff_ffff) as usize;
        self.bytes[offset]
    }
}
