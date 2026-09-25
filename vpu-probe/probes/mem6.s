	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4+64)
	v8ld H(21,0),(r4+64)
	v16mov -,HX(20,0) CLRA UACCH
	v8lookupm H(0,0),(r1)
	v16mov -,HX(20,0) CLRA UACCH
	v16lookupm HX(1,0),(r1)
	v16mov -,HX(20,0) CLRA UACCH
	v32lookupm HY(2,0),(r1)
	v16mov -,HX(20,0) CLRA UACC
	v32lookupml HY(3,0),(r1)
	v16mov -,HX(20,0) CLRA UACC
	v8lookupml H(4,0),(r1+7)
	v16mov -,HX(20,0) CLRA UACC
	v8memread H(5,0),H(21,0),HX(20,0)
	v16mov -,HX(20,0) CLRA UACC
	v8memwrite H(6,0),H(21,0),H(21,0)
	v16mov -,HX(20,0) CLRA UACC
	v32indexwriteml HY(7,0),(r1)
	vgetacc HY(8,0),HX(56,0),HX(56,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
