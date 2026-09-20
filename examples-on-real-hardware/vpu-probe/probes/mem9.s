	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4+64)
	v8ld H(21,0),(r1+32)
	v16mov -,HX(20,0) CLRA UACC
	v8lookupml H(0,0),(r1)
	v16lookupml HX(1,0),(r1)
	v32lookupml HY(2,0),(r1)
	v8lookupml H(3,0),(r1+7)
	v16mov -,HX(20,0) CLRA UACCH
	v8lookupm H(4,0),(r1)
	v16lookupm HX(5,0),(r1)
	v32lookupm HY(6,0),(r1)
	v16mov -,HX(20,0) CLRA UACC
	v8indexwriteml H(21,0),(r4+256)
	v8ld H(7,0),(r4+256)
	v16indexwriteml HX(20,0),(r4+512)
	v16ld HX(8,0),(r4+512)
	v16mov -,HX(20,0) CLRA UACCH
	v8indexwritem H(21,0),(r4+768)
	v8ld H(9,0),(r4+768)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
