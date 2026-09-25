	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16mov -,HX(62,0) CLRA UACC
	v16mov -,HX(63,0) USUB
	vgetacc HY(0,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA USUB
	vgetacc HY(1,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA SACC
	v16mov -,HX(63,0) SSUB
	vgetacc HY(2,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA SACC
	v16mov HX(6,0),HX(63,0) SSUBH
	vgetacc HY(3,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA UACCH
	v16mov HX(7,0),HX(63,0) USUBH
	vgetacc HY(8,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA UACC
	v16mov -,HX(63,0) USUB
	v16mov HX(4,0),HX(63,0) USUB
	v16mov HX(9,0),HX(63,0) USUB WBA
	vgetacc HY(5,0),HX(56,0),HX(56,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
