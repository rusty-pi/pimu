	.text
	.global _start
_start:
	mov r3,#0
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; junk in the A slot
	v16mov -,HX(23,0) CLRA UACC	; accumulators cleared
	v8lookupm H(0,0),H(23,0),(r63)	; A = zeros
	v8lookupm H(1,0),H(20,0),(r63)	; A = junk
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
