	.text
	.global _start
_start:
	mov r3,#0			; no step, so the address cannot wander
	v32mov HY(0++,0),0 REP64
	v16mov -,HX(23,0) CLRA UACC	; every accumulator cleared: index 0
	v8lookupm H(0,0),H(23,0),(r63)	; what does a gather off r63 read?
	v8lookupm H(1,0),H(23,0),(r63)+r3
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
