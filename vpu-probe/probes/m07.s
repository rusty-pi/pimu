	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)
	v8ld H(21,0),(r4+96)
	v32mov HY(0,0),-1		; a destination preset to all-ones
	v8mem07 H(0,0),H(20,0),H(21,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
