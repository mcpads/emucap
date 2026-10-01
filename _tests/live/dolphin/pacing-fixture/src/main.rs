//! CPU-owned VI/XFB fixture. All executable bytes pass through the typed Gekko assembler.
use powerpc_gekko_750cl::{
    BranchOptions, ConditionRegisterBit, ConditionRegisterField, GekkoAssembler, GeneralRegister,
    Instruction, InstructionForm as F, Operand as O, TimeBaseRegister,
};
fn r(n: u8) -> O {
    O::GeneralRegister(GeneralRegister::new(n).unwrap())
}
fn emit(a: &mut GekkoAssembler, form: F, operands: Vec<O>) {
    a.emit(Instruction::new(form, operands).unwrap());
}
fn imm(a: &mut GekkoAssembler, reg: u8, value: u32) {
    emit(
        a,
        F::Addis,
        vec![r(reg), r(0), O::SignedImmediate((value >> 16) as i16)],
    );
    emit(
        a,
        F::Ori,
        vec![r(reg), r(reg), O::UnsignedImmediate(value as u16)],
    );
}
fn mem(a: &mut GekkoAssembler, form: F, reg: u8, base: u8, offset: i16) {
    emit(a, form, vec![r(reg), O::MemoryOffset(offset), r(base)]);
}
fn add(a: &mut GekkoAssembler, reg: u8, value: i16) {
    emit(a, F::Addi, vec![r(reg), r(reg), O::SignedImmediate(value)]);
}
fn branch(a: &mut GekkoAssembler, options: u8, bit: u8, label: &str) {
    a.conditional_branch_to_label(
        BranchOptions::new(options).unwrap(),
        ConditionRegisterBit::new(bit).unwrap(),
        false,
        label,
    );
}
fn store(a: &mut GekkoAssembler, offset: i16, value: u32, half: bool) {
    imm(a, 10, value);
    mem(a, if half { F::Sth } else { F::Stw }, 10, 3, offset);
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: dolphin-pacing-fixture OUTPUT.dol")?;
    let mut a = GekkoAssembler::new();
    imm(&mut a, 3, 0xcc002000); // VI MMIO
    imm(&mut a, 4, 0x80100000); // CPU-owned XFB, 320 x 240 YUYV
    imm(&mut a, 5, 0x80010000); // record: scans, held scans, TB lower, SI, beam, TB upper
    imm(&mut a, 12, 0xcc006400); // SI channel zero
    store(&mut a, 2, 0, true); // disable VI while configuring
    imm(&mut a, 14, 0x80100000);
    imm(&mut a, 13, 320 * 240 / 2);
    imm(&mut a, 10, 0x10801080);
    a.label("fill");
    mem(&mut a, F::Stw, 10, 14, 0);
    add(&mut a, 14, 4);
    add(&mut a, 13, -1);
    emit(
        &mut a,
        F::Cmpli,
        vec![
            O::ConditionRegisterField(ConditionRegisterField::new(0).unwrap()),
            r(13),
            O::UnsignedImmediate(0),
        ],
    );
    branch(&mut a, 4, 2, "fill");
    // Retain native NTSC horizontal timing. Set a 240-line non-interlaced field.
    // Boot region defaults may select 27 or 54 MHz. The fixture owns its clock.
    store(&mut a, 0x6c, 0, true);
    store(&mut a, 0, (240 << 4) | 6, true);
    store(&mut a, 0x0c, (5 << 16) | 22, false);
    store(&mut a, 0x10, (4 << 16) | 23, false);
    store(&mut a, 0x1c, 0x00100000, false);
    store(&mut a, 0x24, 0x00100000, false);
    store(&mut a, 0x48, 0x1414, true); // 20 sixteen-pixel words / line
    imm(&mut a, 6, 0);
    imm(&mut a, 7, 0);
    for offset in [0, 4, 8, 12, 16, 20] {
        mem(&mut a, F::Stw, 6, 5, offset);
    }
    store(&mut a, 2, 5, true);
    emit(
        &mut a,
        F::Mftb,
        vec![r(16), O::TimeBaseRegister(TimeBaseRegister::Lower)],
    );
    mem(&mut a, F::Lhz, 8, 3, 0x2c);
    a.label("poll");
    emit(
        &mut a,
        F::Mftb,
        vec![r(11), O::TimeBaseRegister(TimeBaseRegister::Lower)],
    );
    mem(&mut a, F::Lhz, 9, 3, 0x2c);
    emit(
        &mut a,
        F::Mftb,
        vec![r(17), O::TimeBaseRegister(TimeBaseRegister::Lower)],
    );
    emit(&mut a, F::Ori, vec![r(15), r(16), O::UnsignedImmediate(0)]);
    emit(&mut a, F::Ori, vec![r(16), r(11), O::UnsignedImmediate(0)]);
    emit(
        &mut a,
        F::Cmpl,
        vec![
            O::ConditionRegisterField(ConditionRegisterField::new(0).unwrap()),
            r(9),
            r(8),
        ],
    );
    emit(&mut a, F::Ori, vec![r(8), r(9), O::UnsignedImmediate(0)]);
    branch(&mut a, 4, 0, "poll"); // new beam >= previous: same field
    add(&mut a, 6, 1);
    mem(&mut a, F::Lwz, 10, 12, 4);
    mem(&mut a, F::Stw, 10, 5, 12);
    emit(
        &mut a,
        F::Andis_,
        vec![r(11), r(10), O::UnsignedImmediate(0x8000)],
    ); // transfer error
    branch(&mut a, 4, 2, "record");
    emit(
        &mut a,
        F::Andis_,
        vec![r(11), r(10), O::UnsignedImmediate(0x0100)],
    ); // A
    branch(&mut a, 12, 2, "record");
    add(&mut a, 7, 1);
    a.label("record");
    mem(&mut a, F::Stw, 6, 5, 0);
    mem(&mut a, F::Stw, 7, 5, 4);
    mem(&mut a, F::Stw, 15, 5, 8);
    mem(&mut a, F::Stw, 17, 5, 20);
    mem(&mut a, F::Stw, 9, 5, 16);
    mem(&mut a, F::Lwz, 10, 4, 0);
    emit(
        &mut a,
        F::Xori,
        vec![r(10), r(10), O::UnsignedImmediate(0x1010)],
    );
    mem(&mut a, F::Stw, 10, 4, 0);
    a.branch_to_label(false, "poll");
    let program = a.assemble(0x80003100)?;
    // The DOL header is data; pad the text segment to the loader's 32-byte alignment.
    let size = (program.bytes().len() + 31) & !31;
    let mut dol = vec![0_u8; 256 + size];
    for (offset, value) in [
        (0, 256_u32),
        (0x48, 0x80003100),
        (0x90, size as u32),
        (0xe0, 0x80003100),
    ] {
        dol[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    dol[256..256 + program.bytes().len()].copy_from_slice(program.bytes());
    let path = std::path::PathBuf::from(path);
    std::fs::write(&path, dol)?;
    std::fs::write(
        path.with_extension("json"),
        format!(
            "{{\"entry\":{},\"ready\":{},\"record\":{},\"record_bytes\":24,\"code_bytes\":{}}}\n",
            0x80003100_u32,
            program.label_location("poll").unwrap(),
            0x80010000_u32,
            program.bytes().len()
        ),
    )?;
    println!(
        "verified {} instructions, {} code bytes, poll={:#x}",
        program.instruction_spans().len(),
        program.bytes().len(),
        program.label_location("poll").unwrap()
    );
    Ok(())
}
