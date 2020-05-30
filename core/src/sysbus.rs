use std::cell::Cell;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::bus::*;
use super::cartridge::Cartridge;
use super::dma::DmaNotifer;
use super::iodev::{IoDevices, WaitControl};
use super::util::{BoxedMemory, WeakPointer};
use super::GameBoyAdvance;

pub mod consts {
    pub const WORK_RAM_SIZE: usize = 256 * 1024;
    pub const INTERNAL_RAM_SIZE: usize = 32 * 1024;

    pub const BIOS_ADDR: u32 = 0x0000_0000;
    pub const EWRAM_ADDR: u32 = 0x0200_0000;
    pub const IWRAM_ADDR: u32 = 0x0300_0000;
    pub const IOMEM_ADDR: u32 = 0x0400_0000;
    pub const PALRAM_ADDR: u32 = 0x0500_0000;
    pub const VRAM_ADDR: u32 = 0x0600_0000;
    pub const OAM_ADDR: u32 = 0x0700_0000;
    pub const GAMEPAK_WS0_LO: u32 = 0x0800_0000;
    pub const GAMEPAK_WS0_HI: u32 = 0x0900_0000;
    pub const GAMEPAK_WS1_LO: u32 = 0x0A00_0000;
    pub const GAMEPAK_WS1_HI: u32 = 0x0B00_0000;
    pub const GAMEPAK_WS2_LO: u32 = 0x0C00_0000;
    pub const GAMEPAK_WS2_HI: u32 = 0x0D00_0000;
    pub const SRAM_LO: u32 = 0x0E00_0000;
    pub const SRAM_HI: u32 = 0x0F00_0000;

    pub const PAGE_BIOS: usize = (BIOS_ADDR >> 24) as usize;
    pub const PAGE_EWRAM: usize = (EWRAM_ADDR >> 24) as usize;
    pub const PAGE_IWRAM: usize = (IWRAM_ADDR >> 24) as usize;
    pub const PAGE_IOMEM: usize = (IOMEM_ADDR >> 24) as usize;
    pub const PAGE_PALRAM: usize = (PALRAM_ADDR >> 24) as usize;
    pub const PAGE_VRAM: usize = (VRAM_ADDR >> 24) as usize;
    pub const PAGE_OAM: usize = (OAM_ADDR >> 24) as usize;
    pub const PAGE_GAMEPAK_WS0: usize = (GAMEPAK_WS0_LO >> 24) as usize;
    pub const PAGE_GAMEPAK_WS1: usize = (GAMEPAK_WS1_LO >> 24) as usize;
    pub const PAGE_GAMEPAK_WS2: usize = (GAMEPAK_WS2_LO >> 24) as usize;
    pub const PAGE_SRAM_LO: usize = (SRAM_LO >> 24) as usize;
    pub const PAGE_SRAM_HI: usize = (SRAM_HI >> 24) as usize;
}

use consts::*;

#[derive(Debug, Copy, Clone)]
pub enum MemoryAccessType {
    NonSeq,
    Seq,
}

impl fmt::Display for MemoryAccessType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                MemoryAccessType::NonSeq => "N",
                MemoryAccessType::Seq => "S",
            }
        )
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum MemoryAccessWidth {
    MemoryAccess8,
    MemoryAccess16,
    MemoryAccess32,
}

const CYCLE_LUT_SIZE: usize = 0x10;

#[derive(Serialize, Deserialize, Clone)]
struct CycleLookupTables {
    n_cycles32: [usize; CYCLE_LUT_SIZE],
    s_cycles32: [usize; CYCLE_LUT_SIZE],
    n_cycles16: [usize; CYCLE_LUT_SIZE],
    s_cycles16: [usize; CYCLE_LUT_SIZE],
}

impl Default for CycleLookupTables {
    fn default() -> CycleLookupTables {
        CycleLookupTables {
            n_cycles32: [1; CYCLE_LUT_SIZE],
            s_cycles32: [1; CYCLE_LUT_SIZE],
            n_cycles16: [1; CYCLE_LUT_SIZE],
            s_cycles16: [1; CYCLE_LUT_SIZE],
        }
    }
}

impl CycleLookupTables {
    pub fn init(&mut self) {
        self.n_cycles32[PAGE_EWRAM] = 6;
        self.s_cycles32[PAGE_EWRAM] = 6;
        self.n_cycles16[PAGE_EWRAM] = 3;
        self.s_cycles16[PAGE_EWRAM] = 3;

        self.n_cycles32[PAGE_OAM] = 2;
        self.s_cycles32[PAGE_OAM] = 2;
        self.n_cycles16[PAGE_OAM] = 1;
        self.s_cycles16[PAGE_OAM] = 1;

        self.n_cycles32[PAGE_VRAM] = 2;
        self.s_cycles32[PAGE_VRAM] = 2;
        self.n_cycles16[PAGE_VRAM] = 1;
        self.s_cycles16[PAGE_VRAM] = 1;

        self.n_cycles32[PAGE_PALRAM] = 2;
        self.s_cycles32[PAGE_PALRAM] = 2;
        self.n_cycles16[PAGE_PALRAM] = 1;
        self.s_cycles16[PAGE_PALRAM] = 1;
    }

    pub fn update_gamepak_waitstates(&mut self, waitcnt: WaitControl) {
        static S_GAMEPAK_NSEQ_CYCLES: [usize; 4] = [4, 3, 2, 8];
        static S_GAMEPAK_WS0_SEQ_CYCLES: [usize; 2] = [2, 1];
        static S_GAMEPAK_WS1_SEQ_CYCLES: [usize; 2] = [4, 1];
        static S_GAMEPAK_WS2_SEQ_CYCLES: [usize; 2] = [8, 1];

        let ws0_first_access = waitcnt.ws0_first_access() as usize;
        let ws1_first_access = waitcnt.ws1_first_access() as usize;
        let ws2_first_access = waitcnt.ws2_first_access() as usize;
        let ws0_second_access = waitcnt.ws0_second_access() as usize;
        let ws1_second_access = waitcnt.ws1_second_access() as usize;
        let ws2_second_access = waitcnt.ws2_second_access() as usize;

        // update SRAM access
        let sram_wait_cycles = 1 + S_GAMEPAK_NSEQ_CYCLES[waitcnt.sram_wait_control() as usize];
        self.n_cycles32[PAGE_SRAM_LO] = sram_wait_cycles;
        self.n_cycles32[PAGE_SRAM_LO] = sram_wait_cycles;
        self.n_cycles16[PAGE_SRAM_HI] = sram_wait_cycles;
        self.n_cycles16[PAGE_SRAM_HI] = sram_wait_cycles;
        self.s_cycles32[PAGE_SRAM_LO] = sram_wait_cycles;
        self.s_cycles32[PAGE_SRAM_LO] = sram_wait_cycles;
        self.s_cycles16[PAGE_SRAM_HI] = sram_wait_cycles;
        self.s_cycles16[PAGE_SRAM_HI] = sram_wait_cycles;

        // update both pages of each waitstate
        for i in 0..2 {
            self.n_cycles16[PAGE_GAMEPAK_WS0 + i] = 1 + S_GAMEPAK_NSEQ_CYCLES[ws0_first_access];
            self.s_cycles16[PAGE_GAMEPAK_WS0 + i] = 1 + S_GAMEPAK_WS0_SEQ_CYCLES[ws0_second_access];

            self.n_cycles16[PAGE_GAMEPAK_WS1 + i] = 1 + S_GAMEPAK_NSEQ_CYCLES[ws1_first_access];
            self.s_cycles16[PAGE_GAMEPAK_WS1 + i] = 1 + S_GAMEPAK_WS1_SEQ_CYCLES[ws1_second_access];

            self.n_cycles16[PAGE_GAMEPAK_WS2 + i] = 1 + S_GAMEPAK_NSEQ_CYCLES[ws2_first_access];
            self.s_cycles16[PAGE_GAMEPAK_WS2 + i] = 1 + S_GAMEPAK_WS2_SEQ_CYCLES[ws2_second_access];

            // ROM 32bit accesses are split into two 16bit accesses 1N+1S
            self.n_cycles32[PAGE_GAMEPAK_WS0 + i] =
                self.n_cycles16[PAGE_GAMEPAK_WS0 + i] + self.s_cycles16[PAGE_GAMEPAK_WS0 + i];
            self.n_cycles32[PAGE_GAMEPAK_WS1 + i] =
                self.n_cycles16[PAGE_GAMEPAK_WS1 + i] + self.s_cycles16[PAGE_GAMEPAK_WS1 + i];
            self.n_cycles32[PAGE_GAMEPAK_WS2 + i] =
                self.n_cycles16[PAGE_GAMEPAK_WS2 + i] + self.s_cycles16[PAGE_GAMEPAK_WS2 + i];

            self.s_cycles32[PAGE_GAMEPAK_WS0 + i] = 2 * self.s_cycles16[PAGE_GAMEPAK_WS0 + i];
            self.s_cycles32[PAGE_GAMEPAK_WS1 + i] = 2 * self.s_cycles16[PAGE_GAMEPAK_WS1 + i];
            self.s_cycles32[PAGE_GAMEPAK_WS2 + i] = 2 * self.s_cycles16[PAGE_GAMEPAK_WS2 + i];
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SysBus {
    #[serde(skip)]
    #[serde(default = "WeakPointer::default")]
    /// Weak reference to the owning GameBoyAdvance, mut be set by calling SysBus::init(pointer) before the sysbus can be used
    gba: WeakPointer<GameBoyAdvance>,

    /// Contains the last read value from the BIOS
    bios_value: Cell<u32>,

    pub io: IoDevices,

    bios: BoxedMemory,
    onboard_work_ram: BoxedMemory,
    internal_work_ram: BoxedMemory,
    pub cartridge: Cartridge,

    cycle_luts: CycleLookupTables,

    pub trace_access: bool,
}

pub type SysBusPtr = WeakPointer<SysBus>;

impl SysBus {
    pub fn new(io: IoDevices, bios_rom: Box<[u8]>, cartridge: Cartridge) -> SysBus {
        let mut luts = CycleLookupTables::default();
        luts.init();
        luts.update_gamepak_waitstates(io.waitcnt);

        SysBus {
            io,
            gba: WeakPointer::default(),
            bios_value: Cell::new(0),
            bios: BoxedMemory::new(bios_rom),
            onboard_work_ram: BoxedMemory::new(vec![0; WORK_RAM_SIZE].into_boxed_slice()),
            internal_work_ram: BoxedMemory::new(vec![0; INTERNAL_RAM_SIZE].into_boxed_slice()),
            cartridge: cartridge,

            cycle_luts: luts,

            trace_access: false,
        }
    }

    /// must be called whenever this object is instanciated
    pub fn init(&mut self, gba: WeakPointer<GameBoyAdvance>) {
        self.gba = gba;
        let ptr = SysBusPtr::new(self as *mut SysBus);
        // HACK
        self.io.set_sysbus_ptr(ptr.clone());
    }

    pub fn on_waitcnt_written(&mut self, waitcnt: WaitControl) {
        self.cycle_luts.update_gamepak_waitstates(waitcnt);
    }

    #[inline(always)]
    pub fn get_cycles(
        &self,
        addr: Addr,
        access: MemoryAccessType,
        width: MemoryAccessWidth,
    ) -> usize {
        use MemoryAccessType::*;
        use MemoryAccessWidth::*;
        let page = (addr >> 24) as usize;

        // TODO optimize out by making the LUTs have 0x100 entries for each possible page ?
        if page > 0xF {
            // open bus
            return 1;
        }
        match width {
            MemoryAccess8 | MemoryAccess16 => match access {
                NonSeq => self.cycle_luts.n_cycles16[page],
                Seq => self.cycle_luts.s_cycles16[page],
            },
            MemoryAccess32 => match access {
                NonSeq => self.cycle_luts.n_cycles32[page],
                Seq => self.cycle_luts.s_cycles32[page],
            },
        }
    }
}

#[inline]
fn load_shifted(addr: u32, value: u32) -> u32 {
    value >> ((addr & 3) << 3)
}

/// Helper for "open-bus" accesses
/// http://problemkaputt.de/gbatek.htm#gbaunpredictablethings
/// FIXME: Currently I'm cheating since my bus emulation is not accurate
///     Instead of returning the last prefetched opcode, it will be more accurate
///     to cache the read value for each bus access and return this value instead.Addr
///     while 99% of the time this will be indeed the lsat prefetched opcode, it could also
///     be a leftover value from DMA.
///     However, doing it this way will have runtime overhead and the performance will suffer.
macro_rules! read_invalid {
    (open_bus_impl($sb:ident, $addr:expr)) => {{
        use super::arm7tdmi::CpuState;
        let value = match $sb.gba.cpu.cpsr.state() {
            CpuState::ARM => {
                $sb.gba.cpu.get_prefetched_opcode()
            }
            CpuState::THUMB => {
                // [$+2]
                let decoded = $sb.gba.cpu.get_decoded_opcode() & 0xffff;
                // [$+4]
                let prefetched = $sb.gba.cpu.get_prefetched_opcode() & 0xffff;
                let r15 = $sb.gba.cpu.pc;
                let page_r15 = (r15 >> 24) as usize;
                match page_r15 {
                    PAGE_EWRAM | PAGE_PALRAM | PAGE_VRAM | PAGE_GAMEPAK_WS0..=PAGE_GAMEPAK_WS2 => {
                        // LSW = [$+4], MSW = [$+4]
                        (prefetched << 16) | prefetched
                    }
                    PAGE_BIOS | PAGE_OAM => {
                        if r15 & 3 == 0 {
                            // LSW = [$+4], MSW = [$+6]   ;for opcodes at 4-byte aligned locations
                            warn!("[OPEN-BUS] aligned PC in BIOS or OAM (addr={:08x}, r15={:08x})", $addr, r15);
                            let r15_plus_6 = $sb.read_16(r15 + 6) as u32;
                            (r15_plus_6 << 16) | prefetched
                        } else {
                            // LSW = [$+2], MSW = [$+4]   ;for opcodes at non-4-byte aligned locations
                            (prefetched << 16) | decoded
                        }
                    }
                    PAGE_IWRAM => {
                        // OldLO=[$+2], OldHI=[$+2]
                        if r15 & 3 == 0{
                            // LSW = [$+4], MSW = OldHI   ;for opcodes at 4-byte aligned locations
                            (decoded << 16) | prefetched
                        } else {
                            // LSW = OldLO, MSW = [$+4]   ;for opcodes at non-4-byte aligned locations
                            (prefetched << 16) | decoded
                        }
                    }
                    _ => (prefetched << 16) | prefetched,
                }
            }
        };
        load_shifted($addr, value)
    }};
    ($sb:ident, word($addr:expr)) => {{
        read_invalid!(open_bus_impl($sb, $addr))
    }};
    ($sb:ident, half($addr:expr)) => {{
        read_invalid!(open_bus_impl($sb, $addr)) as u16
    }};
    ($sb:ident, byte($addr:expr)) => {{
        read_invalid!(open_bus_impl($sb, $addr)) as u8
    }};
}

impl Bus for SysBus {
    fn read_32(&self, addr: Addr) -> u32 {
        let aligned = addr & !3;
        match addr & 0xff000000 {
            BIOS_ADDR => {
                if aligned > 0x3ffc {
                    read_invalid!(self, word(addr))
                } else {
                    if self.gba.cpu.pc < 0x4000 {
                        let value = self.bios.read_32(aligned);
                        self.bios_value.set(value);
                        value
                    } else {
                        trace!(
                            "[BIOS-PROTECTION] Accessing BIOS region ({:08x}) {:x?}",
                            addr,
                            self.gba.cpu
                        );
                        self.bios_value.get()
                    }
                }
            }
            EWRAM_ADDR => self.onboard_work_ram.read_32(addr & 0x3_fffc),
            IWRAM_ADDR => self.internal_work_ram.read_32(addr & 0x7ffc),
            IOMEM_ADDR => {
                let addr = if addr & 0xfffc == 0x8000 {
                    0x800
                } else {
                    addr & 0x7fc
                };
                self.io.read_32(addr)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.read_32(aligned),
            GAMEPAK_WS0_LO | GAMEPAK_WS0_HI | GAMEPAK_WS1_LO | GAMEPAK_WS1_HI | GAMEPAK_WS2_LO => {
                self.cartridge.read_32(aligned)
            }
            GAMEPAK_WS2_HI => self.cartridge.read_32(aligned),
            SRAM_LO | SRAM_HI => self.cartridge.read_32(aligned),
            _ => read_invalid!(self, word(addr)),
        }
    }

    fn read_16(&self, addr: Addr) -> u16 {
        let aligned = addr & !1;
        match addr & 0xff000000 {
            BIOS_ADDR => {
                if aligned > 0x3ffe {
                    read_invalid!(self, half(addr))
                } else {
                    let value = if self.gba.cpu.pc < 0x4000 {
                        let value = self.bios.read_32(addr & !3);
                        self.bios_value.set(value);
                        value
                    } else {
                        trace!(
                            "[BIOS-PROTECTION] Accessing BIOS region ({:08x}) {:x?}",
                            addr,
                            self.gba.cpu
                        );
                        self.bios_value.get()
                    };
                    (value >> ((addr & 2) * 8)) as u16
                }
            }
            EWRAM_ADDR => self.onboard_work_ram.read_16(addr & 0x3_fffe),
            IWRAM_ADDR => self.internal_work_ram.read_16(addr & 0x7ffe),
            IOMEM_ADDR => {
                let addr = if addr & 0xfffe == 0x8000 {
                    0x800
                } else {
                    addr & 0x7fe
                };
                self.io.read_16(addr)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.read_16(aligned),
            GAMEPAK_WS0_LO | GAMEPAK_WS0_HI | GAMEPAK_WS1_LO | GAMEPAK_WS1_HI | GAMEPAK_WS2_LO => {
                self.cartridge.read_16(aligned)
            }
            GAMEPAK_WS2_HI => self.cartridge.read_16(aligned),
            SRAM_LO | SRAM_HI => self.cartridge.read_16(aligned),
            _ => read_invalid!(self, half(addr)),
        }
    }

    fn read_8(&self, addr: Addr) -> u8 {
        match addr & 0xff000000 {
            BIOS_ADDR => {
                if addr > 0x3fff {
                    read_invalid!(self, byte(addr))
                } else {
                    let value = if self.gba.cpu.pc < 0x4000 {
                        let value = self.bios.read_32(addr & !3);
                        self.bios_value.set(value);
                        value
                    } else {
                        trace!(
                            "[BIOS-PROTECTION] Accessing BIOS region ({:08x}) {:x?}",
                            addr,
                            self.gba.cpu
                        );
                        self.bios_value.get()
                    };
                    (value >> ((addr & 3) * 8)) as u8
                }
            }
            EWRAM_ADDR => self.onboard_work_ram.read_8(addr & 0x3_ffff),
            IWRAM_ADDR => self.internal_work_ram.read_8(addr & 0x7fff),
            IOMEM_ADDR => {
                let addr = if addr & 0xffff == 0x8000 {
                    0x800
                } else {
                    addr & 0x7ff
                };
                self.io.read_8(addr)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.read_8(addr),
            GAMEPAK_WS0_LO | GAMEPAK_WS0_HI | GAMEPAK_WS1_LO | GAMEPAK_WS1_HI | GAMEPAK_WS2_LO => {
                self.cartridge.read_8(addr)
            }
            GAMEPAK_WS2_HI => self.cartridge.read_8(addr),
            SRAM_LO | SRAM_HI => self.cartridge.read_8(addr),
            _ => read_invalid!(self, byte(addr)),
        }
    }

    fn write_32(&mut self, addr: Addr, value: u32) {
        match addr & 0xff000000 {
            BIOS_ADDR => {}
            EWRAM_ADDR => self.onboard_work_ram.write_32(addr & 0x3_fffc, value),
            IWRAM_ADDR => self.internal_work_ram.write_32(addr & 0x7ffc, value),
            IOMEM_ADDR => {
                let addr = if addr & 0xfffc == 0x8000 {
                    0x800
                } else {
                    addr & 0x7fc
                };
                self.io.write_32(addr, value)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.write_32(addr, value),
            GAMEPAK_WS0_LO => self.cartridge.write_32(addr, value),
            GAMEPAK_WS2_HI => self.cartridge.write_32(addr, value),
            SRAM_LO | SRAM_HI => self.cartridge.write_32(addr, value),
            _ => {}
        }
    }

    fn write_16(&mut self, addr: Addr, value: u16) {
        match addr & 0xff000000 {
            BIOS_ADDR => {}
            EWRAM_ADDR => self.onboard_work_ram.write_16(addr & 0x3_fffe, value),
            IWRAM_ADDR => self.internal_work_ram.write_16(addr & 0x7ffe, value),
            IOMEM_ADDR => {
                let addr = if addr & 0xfffe == 0x8000 {
                    0x800
                } else {
                    addr & 0x7fe
                };
                self.io.write_16(addr, value)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.write_16(addr, value),
            GAMEPAK_WS0_LO => self.cartridge.write_16(addr, value),
            GAMEPAK_WS2_HI => self.cartridge.write_16(addr, value),
            SRAM_LO | SRAM_HI => self.cartridge.write_16(addr, value),
            _ => {}
        }
    }

    fn write_8(&mut self, addr: Addr, value: u8) {
        match addr & 0xff000000 {
            BIOS_ADDR => {}
            EWRAM_ADDR => self.onboard_work_ram.write_8(addr & 0x3_ffff, value),
            IWRAM_ADDR => self.internal_work_ram.write_8(addr & 0x7fff, value),
            IOMEM_ADDR => {
                let addr = if addr & 0xffff == 0x8000 {
                    0x800
                } else {
                    addr & 0x7ff
                };
                self.io.write_8(addr, value)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.write_8(addr, value),
            GAMEPAK_WS0_LO => self.cartridge.write_8(addr, value),
            GAMEPAK_WS2_HI => self.cartridge.write_8(addr, value),
            SRAM_LO | SRAM_HI => self.cartridge.write_8(addr, value),
            _ => {}
        }
    }
}

impl DebugRead for SysBus {
    fn debug_read_8(&self, addr: Addr) -> u8 {
        match addr & 0xff000000 {
            BIOS_ADDR => self.bios.debug_read_8(addr),
            EWRAM_ADDR => self.onboard_work_ram.debug_read_8(addr & 0x3_ffff),
            IWRAM_ADDR => self.internal_work_ram.debug_read_8(addr & 0x7fff),
            IOMEM_ADDR => {
                let addr = if addr & 0xffff == 0x8000 {
                    0x800
                } else {
                    addr & 0x7ff
                };
                self.io.debug_read_8(addr)
            }
            PALRAM_ADDR | VRAM_ADDR | OAM_ADDR => self.io.gpu.debug_read_8(addr),
            GAMEPAK_WS0_LO | GAMEPAK_WS0_HI | GAMEPAK_WS1_LO | GAMEPAK_WS1_HI | GAMEPAK_WS2_LO => {
                self.cartridge.debug_read_8(addr)
            }
            GAMEPAK_WS2_HI => self.cartridge.debug_read_8(addr),
            SRAM_LO | SRAM_HI => self.cartridge.debug_read_8(addr),
            _ => {
                // No open bus for debug reads
                0
            }
        }
    }
}

impl DmaNotifer for SysBus {
    fn notify(&mut self, timing: u16) {
        self.io.dmac.notify_from_gpu(timing);
    }
}
