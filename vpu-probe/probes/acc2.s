	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(60,0),(r4)
	v16ld HX(61,0),(r4+32)
	v16add HX(0,0),HX(60,0),HX(61,0) CLRA UACC
	v16add HX(1,0),HX(60,0),HX(61,0) CLRA UACCH
	v16add HX(2,0),HX(60,0),HX(61,0) CLRA SACC
	v16add HX(3,0),HX(60,0),HX(61,0) CLRA SACCH
	v16add HX(4,0),HX(60,0),HX(61,0) CLRA UDEC
	v16add HX(5,0),HX(60,0),HX(61,0) CLRA SDEC
	v16add -,HX(60,0),HX(61,0) CLRA UACC
	v16add HX(6,0),HX(60,0),HX(61,0) UACC
	v16add -,HX(60,0),HX(61,0) CLRA UACC
	vgetacc HX(7,0),HX(60,0),HX(61,0)
	vgetaccs16 HX(8,0),HX(60,0),HX(61,0)
	vgetaccs32 HX(9,0),HX(60,0),HX(61,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
