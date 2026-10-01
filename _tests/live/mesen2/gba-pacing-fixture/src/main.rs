use arm7tdmi::*;
fn r(n: u8) -> Register {
    Register::new(n).unwrap()
}
fn imm(v: u32) -> Operand2 {
    for rot in (0..32).step_by(2) {
        let n = v.rotate_left(rot);
        if n <= 255 {
            return Operand2::Immediate(RotatedImmediate::new(n as u8, rot as u8).unwrap());
        }
    }
    panic!("unencodable immediate {v:x}")
}
fn op(c: &mut ArmAssembler, operation: DataOperation, dst: u8, src: u8, v: u32, flags: bool) {
    c.emit(ArmInstruction::DataProcessing {
        condition: Condition::Always,
        operation,
        set_flags: flags,
        destination: r(dst),
        first: r(src),
        second: imm(v),
    });
}
fn word(c: &mut ArmAssembler, load: bool, reg: u8, base: u8, offset: u16) {
    c.emit(ArmInstruction::SingleTransfer {
        condition: Condition::Always,
        load,
        width: TransferWidth::Word,
        register: r(reg),
        address: AddressingMode2 {
            base: r(base),
            offset: AddressOffset::Immediate(offset),
            add: true,
            index: IndexMode::Offset,
        },
    });
}
fn half(c: &mut ArmAssembler, load: bool, reg: u8, base: u8, offset: u8) {
    c.emit(ArmInstruction::HalfwordTransfer {
        condition: Condition::Always,
        load,
        kind: HalfwordTransferKind::UnsignedHalfword,
        register: r(reg),
        address: AddressingMode3 {
            base: r(base),
            offset: HalfwordOffset::Immediate(offset),
            add: true,
            index: IndexMode::Offset,
        },
    });
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: gba-pacing-fixture HEADER_SOURCE.gba OUTPUT.gba".into());
    }
    let donor = std::fs::read(&args[1])?;
    if donor.len() < 0xc0 {
        return Err("source does not contain a GBA header".into());
    }
    let mut c = ArmAssembler::new();
    use DataOperation::*;
    for (reg, value) in [
        (0, 0x04000000),
        (1, 0x05000000),
        (2, 0x02000000),
        (3, 0),
        (4, 31),
        (7, 0),
        (8, 0),
    ] {
        op(&mut c, Move, reg, 0, value, false);
    }
    word(&mut c, false, 7, 0, 0x208); // IME off; execution never enters BIOS Halt.
    half(&mut c, false, 7, 0, 0); // Mode 0, no layers: palette entry 0 is the backdrop.
    half(&mut c, false, 4, 1, 0);
    word(&mut c, false, 3, 2, 0);
    word(&mut c, false, 8, 2, 4);
    c.label("active");
    half(&mut c, true, 6, 0, 6);
    op(&mut c, Compare, 0, 6, 160, true);
    c.branch_to_label(Condition::CarrySet, "active");
    c.label("blank");
    half(&mut c, true, 6, 0, 6);
    op(&mut c, Compare, 0, 6, 160, true);
    c.branch_to_label(Condition::CarryClear, "blank");
    op(&mut c, Add, 3, 3, 1, false);
    word(&mut c, false, 3, 2, 0);
    word(&mut c, true, 6, 0, 0x130);
    op(&mut c, Test, 0, 6, 1, true);
    c.branch_to_label(Condition::NotEqual, "colour");
    op(&mut c, Add, 8, 8, 1, false);
    word(&mut c, false, 8, 2, 4);
    c.label("colour");
    op(&mut c, Test, 0, 3, 1, true);
    c.branch_to_label(Condition::NotEqual, "active");
    op(&mut c, ExclusiveOr, 4, 4, 0x7f00, false);
    op(&mut c, ExclusiveOr, 4, 4, 0xe0, false);
    half(&mut c, false, 4, 1, 0);
    c.branch_to_label(Condition::Always, "active");
    let program = c.assemble(0x080000c0)?;
    let mut entry = ArmAssembler::new();
    entry.emit(ArmInstruction::Branch {
        condition: Condition::Always,
        link: false,
        displacement: 0xb8,
    });
    let entry = entry.assemble(0x08000000)?;
    let mut rom = vec![0xff; 32768];
    rom[..0xc0].copy_from_slice(&donor[..0xc0]);
    rom[..4].copy_from_slice(entry.bytes());
    rom[0xa0..0xac].copy_from_slice(b"EMUCAP PACE ");
    rom[0xac..0xb0].copy_from_slice(b"EMCP");
    rom[0xbd] = rom[0xa0..0xbd]
        .iter()
        .fold(0u8, |a, b| a.wrapping_sub(*b))
        .wrapping_sub(0x19);
    rom[0xc0..0xc0 + program.bytes().len()].copy_from_slice(program.bytes());
    std::fs::write(&args[2], rom)?;
    println!(
        "verified {} ARM instructions; header supplied separately",
        program.instruction_spans().len() + 1
    );
    Ok(())
}
