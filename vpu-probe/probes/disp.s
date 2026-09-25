	.text
	.global _start
; r0 = output page, r1 = page whose byte n holds n+1
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP16
	v32ld HY(0,0),(r1+32)
	v8ld H(1,0),(r1+32)
	v16ld HX(2,0),(r1+32)
	v32ld HY(3,0),(r1+4)
	v32st HY(0++,0),(r0+=r3) REP16
	rts
