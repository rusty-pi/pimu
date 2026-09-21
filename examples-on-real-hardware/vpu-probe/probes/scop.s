	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	ld r2,(r4)			; a scalar load, not a vector one
	v8mem11 H(0,0),H(23,0),H(24,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
