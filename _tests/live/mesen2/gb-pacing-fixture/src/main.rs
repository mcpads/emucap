use sm83::{
    AluOperation::*, Assembler, ByteOperand::*, Condition, IndirectAddress, Instruction::*,
    Register16,
};
fn set(c: &mut Assembler, port: u8, value: u8) {
    c.emit(Load8Immediate(A, value))
        .emit(LoadHighAddressFromA(port));
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: gb-pacing-fixture HEADER_SOURCE.gb OUTPUT.gb[c]".into());
    }
    let donor = std::fs::read(&args[1])?;
    if donor.len() < 0x150 {
        return Err("missing cartridge header".into());
    }
    let gbc = std::path::Path::new(&args[2])
        .extension()
        .is_some_and(|x| x == "gbc");
    let mut c = Assembler::new();
    c.emit(DisableInterrupts);
    c.label("init_blank")
        .emit(LoadAFromHighAddress(0x44))
        .emit(AluImmediate(Cp, 144));
    c.jump_relative_if(Condition::C, "init_blank");
    set(&mut c, 0x40, 0);
    c.emit(Load16Immediate(Register16::Hl, 0x8000));
    c.label("clear_vram")
        .emit(Load8Immediate(A, 0))
        .emit(LoadIndirectFromA(IndirectAddress::HlIncrement));
    c.emit(Load8(A, H))
        .emit(AluImmediate(Cp, 0xa0))
        .jump_relative_if(Condition::Nz, "clear_vram");
    c.emit(Load16Immediate(Register16::Bc, 0))
        .emit(Load16Immediate(Register16::De, 0));
    for addr in 0xc100..0xc104 {
        c.emit(Load8Immediate(A, 0)).emit(LoadAddressFromA(addr));
    }
    set(&mut c, 0x00, 0x10); // Select buttons, not directions.
    set(&mut c, 0x47, 0);
    if gbc {
        set(&mut c, 0x68, 0x80);
        set(&mut c, 0x69, 0xff);
        set(&mut c, 0x69, 0x7f);
    }
    set(&mut c, 0x40, 0x91);
    c.label("active")
        .emit(LoadAFromHighAddress(0x44))
        .emit(AluImmediate(Cp, 144));
    c.jump_relative_if(Condition::Nc, "active");
    c.label("blank")
        .emit(LoadAFromHighAddress(0x44))
        .emit(AluImmediate(Cp, 144));
    c.jump_relative_if(Condition::C, "blank");
    c.emit(Increment16(Register16::Bc));
    c.emit(Load8(A, C))
        .emit(LoadAddressFromA(0xc100))
        .emit(Load8(A, B))
        .emit(LoadAddressFromA(0xc101));
    c.emit(LoadAFromHighAddress(0x00))
        .emit(AluImmediate(And, 1))
        .jump_relative_if(Condition::Nz, "color");
    c.emit(Increment16(Register16::De));
    c.emit(Load8(A, E))
        .emit(LoadAddressFromA(0xc102))
        .emit(Load8(A, D))
        .emit(LoadAddressFromA(0xc103));
    c.label("color")
        .emit(Load8(A, C))
        .emit(AluImmediate(And, 2))
        .jump_relative_if(Condition::Z, "white");
    if gbc {
        set(&mut c, 0x68, 0x80);
        set(&mut c, 0x69, 0x1f);
        set(&mut c, 0x69, 0);
    } else {
        set(&mut c, 0x47, 3);
    }
    c.jump("active");
    c.label("white");
    if gbc {
        set(&mut c, 0x68, 0x80);
        set(&mut c, 0x69, 0xff);
        set(&mut c, 0x69, 0x7f);
    } else {
        set(&mut c, 0x47, 0);
    }
    c.jump("active");
    let program = c.assemble(0x150)?;
    let mut entry = Assembler::new();
    entry.emit(Jump(0x150));
    let entry = entry.assemble(0x100)?;
    let mut rom = vec![0; 32768];
    rom[0x104..0x134].copy_from_slice(&donor[0x104..0x134]);
    rom[0x100..0x100 + entry.bytes().len()].copy_from_slice(entry.bytes());
    rom[0x134..0x13f].copy_from_slice(b"EMUCAP PACE");
    rom[0x143] = if gbc { 0x80 } else { 0 };
    rom[0x14d] = rom[0x134..0x14d]
        .iter()
        .fold(0u8, |a, b| a.wrapping_sub(*b).wrapping_sub(1));
    rom[0x150..0x150 + program.bytes().len()].copy_from_slice(program.bytes());
    let sum = rom.iter().fold(0u16, |a, b| a.wrapping_add(*b as u16));
    rom[0x14e..0x150].copy_from_slice(&sum.to_be_bytes());
    std::fs::write(&args[2], rom)?;
    println!(
        "verified {} SM83 instructions",
        program.instruction_spans().len() + 1
    );
    Ok(())
}
