	.text
	.global _start
_start:
	mov r3,#64
	mov r2,#16
	v32mov HY(0++,0),0 REP64
	v8ld V(0,0++),(r1+=r2) REP4
	v32st HY(0++,0),(r0+=r3) REP64
	rts
