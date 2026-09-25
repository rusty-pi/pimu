	.text
	.global _start
_start:
	mov r3,#0
	v32mov HY(0++,0),0 REP64
	v32mov HY(0,0),-1		; a witness: nothing should touch it
	v16mov -,HX(23,0) CLRA UACC
	v8lookupm -,H(23,0),(r63)	; a gather with nowhere to put it
	v8lookupm -,-,(r63)+r3
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
