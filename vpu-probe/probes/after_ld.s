	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)
	v32mov HY(0,0),-1
	v8mem11 H(0,0),H(23,0),H(24,0)
	v8ld H(12,0),(r4+64)		; a load after it, then the dump
	v32st HY(0++,0),(r0+=r3) REP64
	rts
