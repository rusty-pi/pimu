	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32ld HY(20,0),(r4+64)		; sixteen known 32-bit values
	v32mov HY(0++,0),-1 REP8	; rows 0-7 preset, so a partial write shows
	v16or HX(0,0),HY(20,0),0	; A wider than the operation
	v16or HX(1,0),H(21,0),HY(20,0)	; B wider than the operation
	v16or HY(2,0),HX(20,0),0	; D wider than the operation
	v16mov HX(3,0),HY(20,0)		; the same, as a mov
	v32or HY(4,0),HX(20,0),0	; a narrower register than the operation
	v16add HX(5,0),HY(20,0),0	; the same again, on a different op
	v32or HY(6,0),HY(20,0),0	; the control: same width, `or 0`
	v16or HX(7,0),HX(20,0),0	; the control at 16 bits
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
