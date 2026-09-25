	.text
	.global _start
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP64
	v32ld HY(0,0),(r1+13)
	v32st HY(0++,0),(r0+=r3) REP4
	rts
