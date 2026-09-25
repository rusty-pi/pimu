	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16sub -,HX(62,0),HX(63,0) SETF
	v32mov HY(0,0),-1 IFZ
	v32mov HY(1,0),-1 IFNZ
	v32mov HY(2,0),-1 IFN
	v32mov HY(3,0),-1 IFNN
	v32mov HY(4,0),-1 IFC
	v32mov HY(5,0),-1 IFNC
	v32mov HY(6,0),-1 NONE
	v16sub HX(7,0),HX(62,0),HX(63,0)
	v16add -,HX(62,0),HX(63,0) SETF
	v32mov HY(8,0),-1 IFZ
	v32mov HY(9,0),-1 IFN
	v32mov HY(10,0),-1 IFC
	v16add HX(11,0),HX(62,0),HX(63,0)
	v16mov -,HX(62,0) SETF
	v32mov HY(12,0),-1 IFZ
	v32mov HY(13,0),-1 IFN
	v32mov HY(14,0),-1 IFC
	v16ld HX(15,0),(r4+64) SETF
	v32mov HY(16,0),-1 IFZ
	v32mov HY(17,0),-1 IFN
	v32mov HY(18,0),-1 IFC
	v32st HY(0++,0),(r0+=r3) REP64
	rts
