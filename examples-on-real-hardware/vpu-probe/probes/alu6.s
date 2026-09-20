	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32mov HY(0++,0),-1 REP32
	v8ld H(58,0),(r4+64)
	v8ld H(57,0),(r4+96)
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16add HX(0,0),-,HX(63,0)
	v16sub HX(1,0),-,HX(63,0)
	v16and HX(2,0),-,HX(63,0)
	v16min HX(3,0),-,HX(63,0)
	v16add HX(4,0),HX(62,0)*,HX(63,0)
	v16add HX(5,0)*,HX(62,0),HX(63,0)
	v16add HX(6,0),HX(62,0),HX(63,0)*
	v16mov HX(7,0),HX(62,0)*
	v16ld HX(8,0)*,(r4+64)
	v16ld HX(9,0),(r4+64)
	vmull.ss HX(10,0),HX(62,0),H(57,0)
	vmull.ss H(11,0),HX(62,0),HX(63,0)
	vmull.ss HX(12,0),H(58,0),HX(63,0)
	vmulhd.ss HX(13,0),HX(62,0),H(57,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
