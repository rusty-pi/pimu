	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4+64)
	v8ld H(21,0),(r4+64)
	v16mov -,HX(20,0) CLRA UACC
	v8lookupm H(0,0),(r1)
	v16mov -,HX(20,0) CLRA UACC
	v8lookupml H(1,0),(r1)
	v16mov -,HX(20,0) CLRA UACC
	v16lookupml HX(2,0),(r1)
	v16mov -,HX(20,0) CLRA UACC
	v8memread H(3,0),H(21,0),HX(20,0)
	vgetacc HY(4,0),HX(56,0),HX(56,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
