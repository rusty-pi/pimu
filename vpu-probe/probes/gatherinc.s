	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(20,0),(r4+64)
	v8ld H(21,0),(r4+64)
	v32mov HY(0++,0),-1 REP8
	v16mov -,HX(20,0) CLRA UACC
	v8lookupm H(0,0),H(20,0),H(21,0)	; a plain inert slot
	v16mov -,HX(20,0) CLRA UACC
	v8lookupm H(1,0),H(20++,0),H(21,0)	; and one carrying `++`
	v16mov -,HX(20,0) CLRA UACC
	v8lookupm H(2++,0),H(20,0),H(21,0) REP2	; the same over two repetitions
	v16mov -,HX(20,0) CLRA UACC
	v8lookupm H(4++,0),H(20++,0),H(21,0) REP2
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
