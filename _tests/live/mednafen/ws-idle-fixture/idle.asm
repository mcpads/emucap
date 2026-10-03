; Synthetic 128 KiB WonderSwan cartridge. NASM, 16-bit V30 instruction subset.
; The final ROM bank maps to physical F0000. No external code or assets.
bits 16
org 0
times 0x10000 db 0
entry:
    cli
    xor ax, ax
    mov ds, ax
    out 0xb2, al                  ; Mask all native interrupt sources.
    mov word [0x0100], 0x1234
idle:
    hlt
    mov word [0x0100], 0x5678     ; Must remain unreachable without an interrupt.
    jmp $
times 0x1fff0 - ($ - $$) db 0
    jmp 0xf000:0
    nop
    db 0, 0, 0, 0, 0, 0, 0, 0 ; Manufacturer/system/title/version/size/save/flags/RTC.
    dw 0                        ; Build script supplies the cartridge checksum.
