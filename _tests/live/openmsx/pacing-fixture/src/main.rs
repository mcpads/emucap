//! Synthetic cartridge: one counter per VBlank, one counter per sampled A key, fixed PSG tone.
use z80::{AluOperation::*, Assembler, ByteOperand::A, Condition, Instruction::*, Register16};

fn output(c: &mut Assembler, port: u8, value: u8) {
    c.emit(LdRImm(A, value)).emit(OutPortA(port));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: openmsx-pacing-fixture OUTPUT.rom")?;
    let mut c = Assembler::new();
    c.emit(Di).emit(LdRRImm(Register16::Sp, 0xf300));
    // Disable display/IRQ; retain border output and the hardware VBlank latch.
    output(&mut c, 0x99, 0);
    output(&mut c, 0x99, 0x81);
    // PSG channel A: fixed tone, other channels/noise off, fixed volume.
    for (register, value) in [(0, 0x80), (1, 1), (7, 0xbe), (8, 8), (9, 0), (10, 0)] {
        output(&mut c, 0xa0, register);
        output(&mut c, 0xa1, value);
    }
    c.emit(LdRRImm(Register16::Hl, 0))
        .emit(LdAddrHL(0xc100))
        .emit(LdAddrHL(0xc102))
        .emit(LdRImm(A, 1))
        .emit(LdAddrA(0xc104))
        .label("field")
        .emit(InAPort(0x99))
        .emit(AluImm(And, 0x80))
        .jr_cond(Condition::Z, "field")
        .emit(LdHLAddr(0xc100))
        .emit(IncRR(Register16::Hl))
        .emit(LdAddrHL(0xc100))
        // PPI row 2, bit 6 is A, active low. Preserve the other port C control bits.
        .emit(InAPort(0xaa))
        .emit(AluImm(And, 0xf0))
        .emit(AluImm(Or, 2))
        .emit(OutPortA(0xaa))
        .emit(InAPort(0xa9))
        .emit(AluImm(And, 0x40))
        .jr_cond(Condition::Nz, "border")
        .emit(LdHLAddr(0xc102))
        .emit(IncRR(Register16::Hl))
        .emit(LdAddrHL(0xc102))
        .label("border")
        .emit(LdAAddr(0xc104))
        .emit(AluImm(Xor, 0x0e))
        .emit(LdAddrA(0xc104))
        .emit(OutPortA(0x99))
        .emit(LdRImm(A, 0x87))
        .emit(OutPortA(0x99))
        .jp("field");
    // The assembler resolves labels and verifies encoding/decoding before returning bytes.
    let program = c.assemble(0x4010)?;
    let mut cartridge = vec![0xff; 16 * 1024];
    cartridge[..16].fill(0);
    cartridge[..2].copy_from_slice(b"AB");
    cartridge[2..4].copy_from_slice(&0x4010_u16.to_le_bytes());
    cartridge[16..16 + program.bytes().len()].copy_from_slice(program.bytes());
    std::fs::write(path, cartridge)?;
    println!(
        "verified {} instructions, {} code bytes",
        program.instructions().len(),
        program.bytes().len()
    );
    Ok(())
}
