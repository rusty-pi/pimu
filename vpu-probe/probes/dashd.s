	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	mov r5,#7
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4+64)
	v16ld HX(21,0),(r4+96)
	; the bare dash, as the assembler spells it
	.byte 0x01,0xfd,0x21,0xe0,0x15,0x4a,0xc0,0xf3,0x3c,0x00
	v32mov HY(10,0),-1 IFZ		; where the zero flag landed
	v32mov HY(11,0),-1 IFN		; and the negative one
	; the same with an addend nibble of r0 on the dash
	v32mov HY(1,0),0
	.byte 0x01,0xfd,0x21,0xe0,0x15,0x4a,0xc0,0x03,0x3c,0x00
	v32mov HY(12,0),-1 IFZ
	v32mov HY(13,0),-1 IFN
	; and with an addend nibble of r3
	.byte 0x01,0xfd,0x21,0xe0,0x15,0x4a,0xc0,0x33,0x3c,0x00
	v32mov HY(14,0),-1 IFZ
	v32mov HY(15,0),-1 IFN
	v32st HY(0++,0),(r0+=r3) REP64
	rts
