	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(60,0),(r4)
	v8ld H(60,16),(r4+16)
	v8ld H(61,0),(r4+32)
	v8ld H(61,16),(r4+48)
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16add -,H(60,0),H(61,0) CLRA UACC
	v16add H(0,0),H(60,0),H(61,0) UACC
	v16add HX(1,0),H(60,0),H(61,0) UACC
	v16mov -,H(60,0) CLRA SACC
	v16sub HX(2,0),H(60,0),H(61,0) SACC
	v16add HX(3,0),H(60,0),H(61,0) SACC WBA
	v32add -,HX(62,0),HX(63,0) CLRA SACC
	v32add HY(5,0),HX(62,0),HX(63,0) SACC
	v32add HX(6,0),HX(62,0),HX(63,0) SACC WBA
	v32st HY(0++,0),(r0+=r3) REP64
	rts
