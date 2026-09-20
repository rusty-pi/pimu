	.text
	.global _start
; r0 = output page, r1 = marker page (byte n = n+1), r4 = r1 + 4096 (test vectors)
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(60,0),(r4)
	v16ld HX(61,0),(r4+32)
	; multiplies
	vmulm.ss HX(0,0),HX(60,0),HX(61,0)
	vmulhn.ss HX(1,0),HX(60,0),HX(61,0)
	vmull.ss HX(2,0),HX(60,0),HX(61,0)
	; accumulate: three adds, writing the accumulator out each time
	v16add HX(3,0),HX(60,0),HX(61,0) CLRA UACC
	v16add HX(4,0),HX(60,0),HX(61,0) UACC
	v16add HX(5,0),HX(60,0),HX(61,0) UACC
	; signed, and taking back off
	v16add HX(6,0),HX(60,0),HX(61,0) CLRA SACC
	v16add HX(7,0),HX(60,0),HX(61,0) SDEC
	; multiply-accumulate, the shape the codec code uses
	vmulm.ss -,HX(60,0),HX(61,0) CLRA SACC
	vmulm.ss HX(8,0),HX(60,0),HX(61,0) SACC
	; a REP broadcast and a bitwise op over it
	v16mov HX(9++,0),0x15 REP4
	v16eor HX(13,0),HX(60,0),HX(61,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
