	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32mov HY(0++,0),-1 REP16
	v32ld HY(62,0),(r4+64)
	v32ld HY(63,0),(r4+128)
	vmul32.ss HY(0,0),HY(62,0),HY(63,0)
	vmul32.su HY(1,0),HY(62,0),HY(63,0)
	vmul32.us HY(2,0),HY(62,0),HY(63,0)
	vmul32.uu HY(3,0),HY(62,0),HY(63,0)
	vmul32.uu HX(4,0),HY(62,0),HY(63,0)
	vmul32.ss HY(5,0),HY(62,0),HY(63,0) CLRA SACC
	vgetacc HY(6,0),HY(56,0),HY(56,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
