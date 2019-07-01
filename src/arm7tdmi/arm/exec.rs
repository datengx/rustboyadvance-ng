use crate::bit::BitIndex;

use crate::arm7tdmi::bus::{Bus, MemoryAccess, MemoryAccessType::*, MemoryAccessWidth::*};
use crate::arm7tdmi::cpu::{Core, CpuExecResult, CpuPipelineAction};
use crate::arm7tdmi::exception::Exception;
use crate::arm7tdmi::{Addr, CpuError, CpuInstruction, CpuResult, CpuState, REG_PC};

use crate::sysbus::SysBus;

use super::{
    ArmCond, ArmInstruction, ArmFormat, ArmOpCode, ArmRegisterShift, ArmShiftType,
    ArmShiftedValue,
};

impl Core {
    pub fn exec_arm(&mut self, sysbus: &mut SysBus, insn: ArmInstruction) -> CpuExecResult {
        if !self.check_arm_cond(insn.cond) {
            self.add_cycles(
                insn.pc + (self.word_size() as u32),
                sysbus,
                Seq + MemoryAccess32,
            );
            self.add_cycle();
            return Ok(CpuPipelineAction::IncPC);
        }
        match insn.fmt {
            ArmFormat::BX => self.exec_bx(sysbus, insn),
            ArmFormat::B_BL => self.exec_b_bl(sysbus, insn),
            ArmFormat::DP => self.exec_data_processing(sysbus, insn),
            ArmFormat::SWI => self.exec_swi(sysbus, insn),
            ArmFormat::LDR_STR => self.exec_ldr_str(sysbus, insn),
            _ => Err(CpuError::UnimplementedCpuInstruction(CpuInstruction::Arm(
                insn,
            ))),
        }
    }

    /// Cycles 2S+1N
    fn exec_b_bl(
        &mut self,
        sysbus: &mut SysBus,
        insn: ArmInstruction,
    ) -> CpuResult<CpuPipelineAction> {
        if insn.link_flag() {
            self.set_reg(14, self.pc & !0b1);
        }

        // +1N
        self.add_cycles(self.pc, sysbus, NonSeq + MemoryAccess32);

        self.pc = (self.pc as i32).wrapping_add(insn.branch_offset()) as u32;

        // +2S
        self.add_cycles(self.pc, sysbus, Seq + MemoryAccess32);
        self.add_cycles(
            self.pc + (self.word_size() as u32),
            sysbus,
            Seq + MemoryAccess32,
        );

        Ok(CpuPipelineAction::Flush)
    }

    /// Cycles 2S+1N
    fn exec_bx(
        &mut self,
        sysbus: &mut SysBus,
        insn: ArmInstruction,
    ) -> CpuResult<CpuPipelineAction> {
        let rn = self.get_reg(insn.rn());
        if rn.bit(0) {
            self.cpsr.set_state(CpuState::THUMB);
        } else {
            self.cpsr.set_state(CpuState::ARM);
        }

        // +1N
        self.add_cycles(self.pc, sysbus, NonSeq + MemoryAccess32);

        self.pc = rn & !1;

        // +2S
        self.add_cycles(self.pc, sysbus, Seq + MemoryAccess32);
        self.add_cycles(
            self.pc + (self.word_size() as u32),
            sysbus,
            Seq + MemoryAccess32,
        );

        Ok(CpuPipelineAction::Flush)
    }

    fn exec_swi(
        &mut self,
        _sysbus: &mut SysBus,
        _insn: ArmInstruction,
    ) -> CpuResult<CpuPipelineAction> {
        self.exception(Exception::SoftwareInterrupt);
        Ok(CpuPipelineAction::Flush)
    }

    fn barrel_shift(val: i32, amount: u32, shift: ArmShiftType) -> i32 {
        match shift {
            ArmShiftType::LSL => val.wrapping_shl(amount),
            ArmShiftType::LSR => (val as u32).wrapping_shr(amount) as i32,
            ArmShiftType::ASR => val.wrapping_shr(amount),
            ArmShiftType::ROR => val.rotate_right(amount),
        }
    }

    fn register_shift(&mut self, reg: usize, shift: ArmRegisterShift) -> CpuResult<i32> {
        let val = self.get_reg(reg) as i32;
        match shift {
            ArmRegisterShift::ShiftAmount(amount, shift) => {
                Ok(Core::barrel_shift(val, amount, shift))
            }
            ArmRegisterShift::ShiftRegister(reg, shift) => {
                if reg != REG_PC {
                    Ok(Core::barrel_shift(val, self.get_reg(reg) & 0xff, shift))
                } else {
                    Err(CpuError::IllegalInstruction)
                }
            }
        }
    }

    fn alu_sub_update_carry(a: i32, b: i32, carry: &mut bool) -> i32 {
        let res = a.wrapping_sub(b);
        *carry = res > a;
        res
    }

    fn alu_add_update_carry(a: i32, b: i32, carry: &mut bool) -> i32 {
        let res = a.wrapping_sub(b);
        *carry = res < a;
        res
    }

    fn alu(&mut self, opcode: ArmOpCode, op1: i32, op2: i32, set_cond_flags: bool) -> Option<i32> {
        let C = self.cpsr.C() as i32;

        let mut carry = self.cpsr.C();
        let mut overflow = self.cpsr.V();

        let result = match opcode {
            ArmOpCode::AND | ArmOpCode::TST => op1 & op2,
            ArmOpCode::EOR | ArmOpCode::TEQ => op1 ^ op2,
            ArmOpCode::SUB | ArmOpCode::CMP => Self::alu_sub_update_carry(op1, op2, &mut carry),
            ArmOpCode::RSB => Self::alu_sub_update_carry(op2, op1, &mut carry),
            ArmOpCode::ADD | ArmOpCode::CMN => Self::alu_add_update_carry(op1, op2, &mut carry),
            ArmOpCode::ADC => Self::alu_add_update_carry(op1, op2.wrapping_add(C), &mut carry),
            ArmOpCode::SBC => Self::alu_add_update_carry(op1, op2.wrapping_sub(1 - C), &mut carry),
            ArmOpCode::RSC => Self::alu_add_update_carry(op2, op1.wrapping_sub(1 - C), &mut carry),
            ArmOpCode::ORR => op1 | op2,
            ArmOpCode::MOV => op2,
            ArmOpCode::BIC => op1 & (!op2),
            ArmOpCode::MVN => !op2,
        };

        if set_cond_flags {
            self.cpsr.set_N(result < 0);
            self.cpsr.set_Z(result == 0);
            self.cpsr.set_C(carry);
            self.cpsr.set_V(overflow);
        }

        match opcode {
            ArmOpCode::TST | ArmOpCode::TEQ | ArmOpCode::CMP | ArmOpCode::CMN => None,
            _ => Some(result),
        }
    }

    /// Logical/Arithmetic ALU operations
    ///
    /// Cycles: 1S+x+y (from GBATEK)
    ///         Add x=1I cycles if Op2 shifted-by-register. Add y=1S+1N cycles if Rd=R15.
    fn exec_data_processing(
        &mut self,
        sysbus: &mut SysBus,
        insn: ArmInstruction,
    ) -> CpuResult<CpuPipelineAction> {
        // TODO handle carry flag

        let mut pipeline_action = CpuPipelineAction::IncPC;

        let op1 = self.get_reg(insn.rn()) as i32;
        let op2 = insn.operand2()?;

        let rd = insn.rd();
        if rd == REG_PC {
            // +1N
            self.add_cycles(self.pc, sysbus, NonSeq + MemoryAccess32);
        }

        let op2: i32 = match op2 {
            ArmShiftedValue::RotatedImmediate(immediate, rotate) => {
                Ok(immediate.rotate_right(rotate) as i32)
            }
            ArmShiftedValue::ShiftedRegister {
                reg,
                shift,
                added: _,
            } => {
                // +1I
                self.add_cycle();
                self.register_shift(reg, shift)
            }
            _ => unreachable!(),
        }?;

        let opcode = insn.opcode().unwrap();
        let set_flags = opcode.is_setting_flags() || insn.set_cond_flag();
        if let Some(result) = self.alu(opcode, op1, op2, set_flags) {
            self.set_reg(rd, result as u32);
            if (rd == REG_PC) {
                pipeline_action = CpuPipelineAction::Flush;
                // +1S
                self.add_cycles(self.pc, sysbus, Seq + MemoryAccess32);
            }
        }

        // +1S
        self.add_cycles(
            self.pc + (self.word_size() as u32),
            sysbus,
            Seq + MemoryAccess32,
        );
        Ok(pipeline_action)
    }

    fn get_rn_offset(&mut self, insn: &ArmInstruction) -> i32 {
        // TODO decide if error handling or panic here
        match insn.ldr_str_offset().unwrap() {
            ArmShiftedValue::ImmediateValue(offset) => offset,
            ArmShiftedValue::ShiftedRegister {
                reg,
                shift,
                added: Some(added),
            } => {
                let abs = self.register_shift(reg, shift).unwrap();
                if added {
                    abs
                } else {
                    -abs
                }
            }
            _ => panic!("bad barrel shift"),
        }
    }

    /// Memory Load/Store
    /// Instruction                     |  Cycles       | Flags | Expl.
    /// ------------------------------------------------------------------------------
    /// LDR{cond}{B}{T} Rd,<Address>    | 1S+1N+1I+y    | ----  |  Rd=[Rn+/-<offset>]
    /// STR{cond}{B}{T} Rd,<Address>    | 2N            | ----  |  [Rn+/-<offset>]=Rd
    /// ------------------------------------------------------------------------------
    /// For LDR, add y=1S+1N if Rd=R15.
    fn exec_ldr_str(
        &mut self,
        sysbus: &mut SysBus,
        insn: ArmInstruction,
    ) -> CpuResult<CpuPipelineAction> {
        if insn.write_back_flag() && insn.rd() == insn.rn() {
            return Err(CpuError::IllegalInstruction);
        }

        let mut pipeline_action = CpuPipelineAction::IncPC;

        let mut addr = self.get_reg(insn.rn());
        if insn.rn() == REG_PC {
            addr += 8; // prefetching
        }
        let dest = self.get_reg(insn.rd());

        let offset = self.get_rn_offset(&insn);

        let effective_addr = (addr as i32).wrapping_add(offset) as Addr;
        addr = if insn.pre_index_flag() {
            effective_addr
        } else {
            addr
        };

        if insn.load_flag() {
            let data = if insn.transfer_size() == 1 {
                // +1N
                self.add_cycles(dest, sysbus, NonSeq + MemoryAccess8);
                sysbus.read_8(addr) as u32
            } else {
                // +1N
                self.add_cycles(dest, sysbus, NonSeq + MemoryAccess32);
                sysbus.read_32(addr)
            };
            // +1S
            self.add_cycles(
                self.pc + (self.word_size() as u32),
                sysbus,
                Seq + MemoryAccess32,
            );

            self.set_reg(insn.rd(), data);

            // +1I
            self.add_cycle();
            // +y
            if insn.rd() == REG_PC {
                // +1S
                self.add_cycles(self.pc, sysbus, Seq + MemoryAccess32);
                // +1N
                self.add_cycles(
                    self.pc + (self.word_size() as u32),
                    sysbus,
                    NonSeq + MemoryAccess32,
                );
                pipeline_action = CpuPipelineAction::Flush;
            }
        } else {
            self.add_cycles(addr, sysbus, NonSeq + MemoryAccess32);
            let value = self.get_reg(insn.rn());
            if insn.transfer_size() == 1 {
                // +1N
                self.add_cycles(dest, sysbus, NonSeq + MemoryAccess8);
                sysbus.write_8(addr, value as u8).expect("bus error");
            } else {
                // +1N
                self.add_cycles(dest, sysbus, NonSeq + MemoryAccess32);
                sysbus.write_32(addr, value).expect("bus error");
            };
        }

        if insn.write_back_flag() {
            self.set_reg(insn.rn(), effective_addr as u32)
        }

        Ok(pipeline_action)
    }
}
