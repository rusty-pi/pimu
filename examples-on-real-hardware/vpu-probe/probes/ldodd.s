	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4160
	mov r2,r1
	add r2,#4352
	mov r5,#3
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4)
	v16ld HX(0,0),(r4)
	v16ld HX(1,0),-+r5,(r4)
	v8ld H(2,0),-+r5,(r4)
	v16ld HX(3,0),HX(20,0),(r4)
	v16st HX(20,0),-+r5,(r2)
	v16ld HX(4,0),(r2)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
