	.text
	.global _start
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP64
	v16ld VX(32,46),(r1)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
