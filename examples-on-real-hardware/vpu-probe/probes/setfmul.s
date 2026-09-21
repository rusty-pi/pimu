	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v32mov HY(59,0),-1		; ones, to set the carry
	v16add -,HX(59,0),HX(59,0) SETF	; carry in = 1
	vmulhdt.ss -,HX(62,0),HX(63,0) SETF
	v32mov HY(0,0),-1 IFC
	v32mov HY(1,0),-1 IFZ
	v32mov HY(2,0),-1 IFN
	v16add -,HX(58,0),HX(58,0) SETF	; carry in = 0
	vmulhdt.ss -,HX(62,0),HX(63,0) SETF
	v32mov HY(3,0),-1 IFC
	vmulhdt.ss HY(4,0),HX(62,0),HX(63,0)	; and the result itself
	v16add -,HX(59,0),HX(59,0) SETF	; carry in = 1
	vmulhdt.su -,HX(62,0),HX(63,0) SETF
	v32mov HY(5,0),-1 IFC
	v32mov HY(6,0),-1 IFZ
	v32mov HY(7,0),-1 IFN
	v16add -,HX(58,0),HX(58,0) SETF	; carry in = 0
	vmulhdt.su -,HX(62,0),HX(63,0) SETF
	v32mov HY(8,0),-1 IFC
	vmulhdt.su HY(9,0),HX(62,0),HX(63,0)	; and the result itself
	v16add -,HX(59,0),HX(59,0) SETF	; carry in = 1
	vmul32.ss -,HX(62,0),HX(63,0) SETF
	v32mov HY(10,0),-1 IFC
	v32mov HY(11,0),-1 IFZ
	v32mov HY(12,0),-1 IFN
	v16add -,HX(58,0),HX(58,0) SETF	; carry in = 0
	vmul32.ss -,HX(62,0),HX(63,0) SETF
	v32mov HY(13,0),-1 IFC
	vmul32.ss HY(14,0),HX(62,0),HX(63,0)	; and the result itself
	v16add -,HX(59,0),HX(59,0) SETF	; carry in = 1
	vmul32.uu -,HX(62,0),HX(63,0) SETF
	v32mov HY(15,0),-1 IFC
	v32mov HY(16,0),-1 IFZ
	v32mov HY(17,0),-1 IFN
	v16add -,HX(58,0),HX(58,0) SETF	; carry in = 0
	vmul32.uu -,HX(62,0),HX(63,0) SETF
	v32mov HY(18,0),-1 IFC
	vmul32.uu HY(19,0),HX(62,0),HX(63,0)	; and the result itself
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
