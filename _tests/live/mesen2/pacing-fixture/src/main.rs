//! Synthetic cartridge: one counter per VBlank, one counter per sampled button 2, fixed PSG tone.
use z80::{AluOperation::*, Assembler, ByteOperand::A, Condition, Instruction::*, Register16};

fn output(c: &mut Assembler, port: u8, value: u8) {
    c.emit(LdRImm(A, value)).emit(OutPortA(port));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: sms-pacing-fixture OUTPUT.sms")?;
    let gamegear = std::path::Path::new(&path).extension().is_some_and(|ext| ext == "gg");
    let mut c = Assembler::new();
    c.emit(Di).emit(LdRRImm(Register16::Sp, 0xdff0));
    // Disable display/IRQ; retain border output and the hardware VBlank latch.
    output(&mut c, 0xbf, 0);
    output(&mut c, 0xbf, 0x81);
    // SMS mode 4, display/IRQ disabled; VBlank status continues to latch.
    output(&mut c, 0xbf, 4);
    output(&mut c, 0xbf, 0x80);
    // Mode-4 backdrop colours: index 17 red and 31 white.
    output(&mut c, 0xbf, 0);
    output(&mut c, 0xbf, 0xc0);
    for index in 0..32 {
        if gamegear {
            // Game Gear CRAM uses little-endian 12-bit RGB entries.
            output(&mut c, 0xbe, if index == 17 { 0x0f } else { 0xff });
            output(&mut c, 0xbe, if index == 17 { 0 } else { 0x0f });
        } else {
            output(&mut c, 0xbe, if index == 17 { 0x03 } else { 0x3f });
        }
    }
    // SN76489 channel 0 period 0x180, attenuation 8; other channels muted.
    for value in [0x80, 0x18, 0x98, 0xbf, 0xdf, 0xff] {
        output(&mut c, 0x7f, value);
    }
    c.emit(LdRRImm(Register16::Hl, 0))
        .emit(LdAddrHL(0xc100))
        .emit(LdAddrHL(0xc102))
        .emit(LdRImm(A, 1))
        .emit(LdAddrA(0xc104))
        .label("field")
        .emit(InAPort(0xbf))
        .emit(AluImm(And, 0x80))
        .jr_cond(Condition::Z, "field")
        .emit(LdHLAddr(0xc100))
        .emit(IncRR(Register16::Hl))
        .emit(LdAddrHL(0xc100))
        // SMS controller port: button 2 is active-low bit 5.
        .emit(InAPort(0xdc))
        .emit(AluImm(And, 0x20))
        .jr_cond(Condition::Nz, "border")
        .emit(LdHLAddr(0xc102))
        .emit(IncRR(Register16::Hl))
        .emit(LdAddrHL(0xc102))
        .label("border");
    if gamegear {
        //Two fields per colour keep motion observable with native LCD frame blending.
        c.emit(LdAAddr(0xc100))
            .emit(AluImm(And, 1))
            .jr_cond(Condition::Nz, "field");
    }
    c.emit(LdAAddr(0xc104))
        .emit(AluImm(Xor, 0x0e))
        .emit(LdAddrA(0xc104))
        .emit(OutPortA(0xbf))
        .emit(LdRImm(A, 0x87))
        .emit(OutPortA(0xbf))
        .jp("field");
    // The assembler resolves labels and verifies encoding/decoding before returning bytes.
    let program = c.assemble(0)?;
    let mut cartridge = vec![0xff; 32 * 1024];
    cartridge[..program.bytes().len()].copy_from_slice(program.bytes());
    cartridge[0x7ff0..0x7ff8].copy_from_slice(b"TMR SEGA");
    cartridge[0x7ff8..].fill(0);
    cartridge[0x7fff] = if gamegear { 0x6c } else { 0x4c }; // Export, 32 KiB.
    let checksum = cartridge[..0x7ff0].iter().fold(0u16, |n, b| n.wrapping_add(*b as u16));
    cartridge[0x7ffa..0x7ffc].copy_from_slice(&checksum.to_le_bytes());
    std::fs::write(path, cartridge)?;
    println!(
        "verified {} instructions, {} code bytes",
        program.instructions().len(),
        program.bytes().len()
    );
    Ok(())
}
