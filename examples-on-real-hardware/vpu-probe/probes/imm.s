	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v32mov HY(0,0),-1
	v32mov HY(1,0),63
	v32mov HY(2,0),32
	v16mov HX(3,0),-1
	v16mov HX(4,0),32
	v32add HY(5,0),HX(62,0),32
	v32add HY(6,0),HX(62,0),-1
	v16add HX(7,0),HX(62,0),32
	v32st HY(0++,0),(r0+=r3) REP64
	rts
