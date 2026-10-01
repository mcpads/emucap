use rp2a03::{AddressingMode::*, Assembler, Instruction, Mnemonic, Mnemonic::*, Operand};
fn emit(c: &mut Assembler, op: Mnemonic, mode: rp2a03::AddressingMode, value: Operand) {
    c.emit(Instruction::new(op, mode, value).unwrap());
}
fn byte(c: &mut Assembler, op: Mnemonic, value: u8) {
    emit(c, op, Immediate, Operand::Byte(value));
}
fn addr(c: &mut Assembler, op: Mnemonic, value: u16) {
    emit(c, op, Absolute, Operand::Word(value));
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: nes-pacing-fixture OUTPUT.nes")?;
    let mut c = Assembler::new();
    emit(&mut c, Sei, Implied, Operand::None);
    emit(&mut c, Cld, Implied, Operand::None);
    byte(&mut c, Ldx, 0xff);
    emit(&mut c, Txs, Implied, Operand::None);
    byte(&mut c, Lda, 0);
    for port in [0x2000, 0x2001, 0x4010, 0x4015, 0x100, 0x101, 0x102, 0x103] {
        addr(&mut c, Sta, port);
    }
    byte(&mut c, Lda, 0x40);
    addr(&mut c, Sta, 0x4017);
    for label in ["warmup1", "warmup2"] {
        c.label(label);
        addr(&mut c, Bit, 0x2002);
        c.emit_label_ref(Bpl, Relative, label);
    }
    byte(&mut c, Lda, 0x80);
    addr(&mut c, Sta, 0x2000);
    c.label("idle");
    c.emit_label_ref(Jmp, Absolute, "idle");
    c.label("nmi");
    emit(&mut c, Pha, Implied, Operand::None);
    addr(&mut c, Inc, 0x100);
    c.emit_label_ref(Bne, Relative, "input");
    addr(&mut c, Inc, 0x101);
    c.label("input");
    byte(&mut c, Lda, 1);
    addr(&mut c, Sta, 0x4016);
    byte(&mut c, Lda, 0);
    addr(&mut c, Sta, 0x4016);
    addr(&mut c, Lda, 0x4016);
    byte(&mut c, And, 1);
    c.emit_label_ref(Beq, Relative, "color");
    addr(&mut c, Inc, 0x102);
    c.emit_label_ref(Bne, Relative, "color");
    addr(&mut c, Inc, 0x103);
    c.label("color");
    byte(&mut c, Lda, 0x3f);
    addr(&mut c, Sta, 0x2006);
    byte(&mut c, Lda, 0);
    addr(&mut c, Sta, 0x2006);
    addr(&mut c, Lda, 0x100);
    byte(&mut c, And, 2);
    c.emit_label_ref(Beq, Relative, "white");
    byte(&mut c, Lda, 0x16);
    c.emit_label_ref(Jmp, Absolute, "write");
    c.label("white");
    byte(&mut c, Lda, 0x30);
    c.label("write");
    addr(&mut c, Sta, 0x2007);
    byte(&mut c, Lda, 0);
    addr(&mut c, Sta, 0x2006);
    addr(&mut c, Sta, 0x2006);
    emit(&mut c, Pla, Implied, Operand::None);
    emit(&mut c, Rti, Implied, Operand::None);
    let program = c.assemble(0x8000)?;
    let mut rom = vec![0; 16 + 16384];
    rom[..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1; // NROM, one PRG bank, CHR RAM.
    rom[16..16 + program.bytes().len()].copy_from_slice(program.bytes());
    for offset in [0x3ffa, 0x3ffc, 0x3ffe] {
        rom[16 + offset..18 + offset].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    rom[16 + 0x3ffa..18 + 0x3ffa]
        .copy_from_slice(&program.label_location("nmi").unwrap().to_le_bytes());
    std::fs::write(path, rom)?;
    println!(
        "verified {} RP2A03 instructions",
        program.instruction_spans().len()
    );
    Ok(())
}
