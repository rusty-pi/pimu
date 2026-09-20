	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)
	v8ld H(21,0),(r4+96)
	v16ld HX(22,0),(r4+64)
	v16ld HX(23,0),(r4+96)
	v8memwrite -,H(20,0),H(21,0)
	v8memread H(0,0),H(20,0),H(21,0)
	v8memread H(1,0),H(21,0),H(20,0)
	v16memwrite -,HX(22,0),HX(23,0)
	v16memread HX(2,0),HX(22,0),HX(23,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
