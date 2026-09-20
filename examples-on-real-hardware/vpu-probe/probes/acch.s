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
	vgetacc HY(0,0),HX(56,0),HX(56,0)
	v16mov -,HX(63,0) UACCH
	vgetacc HY(1,0),HX(56,0),HX(56,0)
	v16mov HX(2,0),HX(63,0) UACCH
	v16mov -,HX(62,0) CLRA SACC
	vgetacc HY(3,0),HX(56,0),HX(56,0)
	v16mov -,HX(63,0) SACCH
	vgetacc HY(4,0),HX(56,0),HX(56,0)
	v16mov HX(5,0),HX(63,0) SACCH
	v16mov -,HX(62,0) CLRA UACCH
	vgetacc HY(6,0),HX(56,0),HX(56,0)
	v16mov HX(7,0),HX(62,0) CLRA UACCH
	v16mov -,HX(62,0) CLRA SACCH
	vgetacc HY(8,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA UACC
	v16mov -,HX(63,0) UACC
	vgetacc HY(9,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA USUB
	vgetacc HY(10,0),HX(56,0),HX(56,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
