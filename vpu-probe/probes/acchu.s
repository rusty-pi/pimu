	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	; the accumulator's high half with SUB and no SIGN, both polarities
	v16mov -,HX(62,0) CLRA UACC
	v16mov HY(0,0),HX(63,0) UACC HIGH SUB	; unsigned high SUB
	vgetacc HY(1,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA SACC
	v16mov HY(2,0),HX(63,0) SACC HIGH SUB	; the signed one, for comparison
	vgetacc HY(3,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA UACC
	v16mov HY(4,0),HX(63,0) UACC HIGH	; and without SUB
	vgetacc HY(5,0),HX(56,0),HX(56,0)
	v16mov HY(6,0),HX(62,0)		; the operands, for the arithmetic
	v16mov HY(7,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
