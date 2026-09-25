	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v32mov HY(59,0),-1
	v16add -,HX(59,0),HX(59,0) SETF
	v16ld HX(0,0),(r4+64) SETF
	v32mov HY(1,0),-1 IFZ
	v32mov HY(2,0),-1 IFN
	v32mov HY(3,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16st HX(62,0),(r4+256) SETF
	v32mov HY(4,0),-1 IFZ
	v32mov HY(5,0),-1 IFN
	v32mov HY(6,0),-1 IFC
	v16add -,HX(56,0),HX(56,0) SETF
	v8ld H(7,0),(r4) SETF
	v32mov HY(8,0),-1 IFZ
	v32mov HY(9,0),-1 IFN
	v16mov -,HX(62,0) CLRA UACC
	vgetacc HX(10,0),HX(62,0),HX(56,0)
	vgetacc HX(11,0),HX(62,0),HX(63,0)
	v16mov -,HX(59,0) CLRA UACC
	v16mov -,HX(59,0) UACC
	vgetacc HX(12,0),HX(62,0),HX(56,0)
	vgetacc HX(13,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA SACC
	vgetacc HX(14,0),HX(62,0),HX(56,0)
	vgetacc HY(15,0),HX(62,0),HX(56,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
