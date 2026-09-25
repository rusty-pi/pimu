	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)
	v16ld HX(21,0),(r4+64)
	v8ld H(22,0),(r4+96)
	v16ld HX(23,0),(r4+96)
	v8memread H(0,0),H(20,0),H(22,0)
	v8memread H(1,0),H(22,0),H(20,0)
	v16memread HX(2,0),HX(21,0),HX(23,0)
	v16mov -,HX(23,0) CLRA UACC
	v8memread H(3,0),H(20,0),H(22,0)
	v16mov -,HX(23,0) CLRA UACCH
	v8memread H(4,0),H(20,0),H(22,0)
	v8memwrite H(5,0),H(20,0),H(22,0)
	v8memwrite H(6,0),H(22,0),H(20,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
