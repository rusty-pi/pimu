	.text
	.global _start
_start:
	mov r3,#0
	mov r5,#0
	v32mov HY(0++,0),0 REP64
	v16mov HX(24,0),16
	v16mov -,HX(24,0) CLRA SACCH	; 16 in the accumulator's high half
	vgetacc HY(2,0),HX(23,0),HX(23,0)
	v32ld HY(10,0),(r5)		; address 0
	v8lookupm H(0,0),H(23,0),H(21,0)	; B a vector
	v8lookupm H(1,0),H(23,0),(r63)		; B an address-less dash
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
