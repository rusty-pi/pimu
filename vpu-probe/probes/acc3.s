	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(60,0),(r4)
	v16ld HX(61,0),(r4+32)
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16add -,HX(60,0),HX(61,0) CLRA UACC
	v16add HX(0,0),HX(60,0),HX(61,0) UACC
	v16add HX(1,0),HX(60,0),HX(61,0) UACCH
	v16add -,HX(60,0),HX(61,0) CLRA SACC
	v16add HX(2,0),HX(60,0),HX(61,0) SACC
	v16add HX(3,0),HX(60,0),HX(61,0) SACCH
	v16add -,HX(60,0),HX(61,0) CLRA UACC
	v16add -,HX(60,0),HX(61,0) UACC
	v16add -,HX(60,0),HX(61,0) UACC
	v16add -,HX(60,0),HX(61,0) UACC
	vgetacc HX(4,0),HX(60,0),HX(61,0)
	vgetacc HX(5,0),HX(60,0),HX(62,0)
	vgetacc HX(6,0),HX(60,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
