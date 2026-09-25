	.text
	.global _start
_start:
	mov r3,#64
	mov r6,r0
	mov r4,r1
	add r4,#4096
	mov r5,#0x1234
	v32mov HY(0++,0),0 REP64
	v32mov HY(0++,0),-1 REP4
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16bitplanes HX(0,0),r5
	v32bitplanes HY(1,0),r5
	v16bitplanes HX(2,0),0x25
	v16bitplanes HX(3,0),HX(62,0)
	v16bitplanes HX(4,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16bitplanes -,r5 SETF
	v32mov HY(5,0),-1 IFZ
	v32mov HY(6,0),-1 IFN
	v32mov HY(7,0),-1 IFC
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) SUMU r0
	v32mov HY(8,0),r0
	v16bitplanes -,r5 SETF
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) IFZ SUMU r0
	v32mov HY(9,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) IFNZ SUMU r0
	v32mov HY(10,0),r0
	mov r0,#0
	v16mov -,HX(62,0) IFZ IMIN r0
	v32mov HY(11,0),r0
	mov r0,r6
	v32st HY(0++,0),(r0+=r3) REP64
	rts
