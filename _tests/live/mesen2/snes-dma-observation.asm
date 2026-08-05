.MEMORYMAP
  DEFAULTSLOT 0
  SLOTSIZE $8000
  SLOT 0 $8000
.ENDME

.ROMBANKMAP
  BANKSTOTAL 1
  BANKSIZE $8000
  BANKS 1
.ENDRO

.SNESHEADER
  ID "EMUC"
  NAME "EMUCAP DMA OBSERVE   "
  LOROM
  SLOWROM
  CARTRIDGETYPE $00
  ROMSIZE $05
  SRAMSIZE $00
  COUNTRY $01
  LICENSEECODE $00
  VERSION $00
.ENDSNES

.SNESNATIVEVECTOR
  COP EmptyHandler
  BRK EmptyHandler
  ABORT EmptyHandler
  NMI EmptyHandler
  UNUSED EmptyHandler
  IRQ EmptyHandler
.ENDNATIVEVECTOR

.SNESEMUVECTOR
  COP EmptyHandler
  UNUSED EmptyHandler
  ABORT EmptyHandler
  NMI EmptyHandler
  RESET Reset
  IRQBRK EmptyHandler
.ENDEMUVECTOR

.BANK 0 SLOT 0
.ORG 0

.SECTION "program" FORCE
Reset:
  sei
  clc
  xce
  rep #$30
  ldx #$1fff
  txs
  sep #$20

  lda #$80
  sta $2100

  ; Manual DMA channel 0: four ROM bytes to the OAM data port.
  stz $4300
  lda #$04
  sta $4301
  lda #<ManualData
  sta $4302
  lda #>ManualData
  sta $4303
  lda #$00
  sta $4304
  lda #$04
  sta $4305
  stz $4306
  lda #$01
  sta $420b

  ; HDMA channel 1: one table byte to the same port on the next scanline.
  stz $4310
  lda #$04
  sta $4311
  lda #<HdmaTable
  sta $4312
  lda #>HdmaTable
  sta $4313
  lda #$00
  sta $4314
  lda #$02
  sta $420c

  ; Produce enough selected events to exercise the Core sink's bounded backpressure before the
  ; next frame runs HDMA. This stays finite and well inside the recording limits.
  rep #$10
  ldx #$0800
EventBurst:
  dex
  bne EventBurst

  stz $2100
Loop:
  wai
  bra Loop

EmptyHandler:
  rti

ManualData:
  .DB $11, $22, $33, $44

HdmaTable:
  .DB $01, $55, $00
.ENDS
