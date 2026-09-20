	.text
	.global _start
_start:
	mov r3,#0
	mov r5,#0
	v32mov HY(0++,0),0 REP64
	v16mov -,HX(23,0) CLRA UACC	; indices all zero
	v32ld HY(10,0),(r5)		; what address 0 holds
	v8lookupm H(0,0),H(23,0),H(21,0)	; B a vector, not an address
	v8lookupm H(1,0),H(23,0),(r63)		; the address-less form, for comparison
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
