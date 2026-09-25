	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; one load, then three fences, no branch
	.rept 3
	v8mem11 H(0,0),H(23,0),H(24,0)
	.endr
	v32st HY(0++,0),(r0+=r3) REP64
	rts
