	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4+64)
	v8ld H(21,0),(r4+64)
	v8ld H(22,0),(r1+32)
	v16mov -,HX(20,0) CLRA UACC
	v8indexwriteml H(22,0),(r4+256)
	v8ld H(0,0),(r4+256)
	v16mov -,HX(20,0) CLRA UACC
	v8indexwritem H(22,0),(r4+512)
	v8ld H(1,0),(r4+512)
	v16mov -,HX(20,0) CLRA UACCH
	v8indexwritem H(22,0),(r4+768)
	v8ld H(2,0),(r4+768)
	v16mov -,HX(20,0) CLRA UACC
	v8memread H(3,0),H(22,0),HX(20,0)
	v16mov -,HX(20,0) CLRA UACC
	v8memwrite H(4,0),H(22,0),H(21,0)
	v8ld H(5,0),(r1+32)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
