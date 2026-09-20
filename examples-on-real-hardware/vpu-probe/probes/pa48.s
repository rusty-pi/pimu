	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r2
	v32mov HY(0++,0),0 REP16
	v16mov H(0,0)+r4,0x15
	v32st HY(0++,0),(r0+=r3) REP16
	rts
