	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	vmulhdt.ss HX(0,0),HX(62,0),HX(63,0)
	vmulhdt.su HX(1,0),HX(62,0),HX(63,0)
	vmulhd.ss HX(2,0),HX(62,0),HX(63,0)
	vmulhn.ss HX(3,0),HX(62,0),HX(63,0)
	v16mov -,HX(62,0) CLRA UACC
	v16add HX(4,0),HX(62,0),HX(63,0) UADD
	v16mov -,HX(62,0) CLRA UACC
	v16add HX(5,0),HX(62,0),HX(63,0) SADD
	v16mov -,HX(62,0) CLRA UACC
	vgetacc HY(6,0),HX(58,0),HX(58,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
