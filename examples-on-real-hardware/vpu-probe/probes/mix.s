	.text
	.global _start
_start:
	mov r3,#64
	mov r2,#16
	mov r4,#3
	v32mov HY(0++,0),0 REP64
	v8ld H(0,0)+r4,(r1)
	v16ld HX(1,0),(r1+7)
	v32ld HY(2,0),(r1)
	v8ld HY(3,0),(r1)
	v32ld H(4,0),(r1)
	v8ld V(16,0++),(r1+=r2) REP4
	v32st HY(0++,0),(r0+=r3) REP64
	rts
