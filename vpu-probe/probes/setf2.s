	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(58,0),(r4)
	v8ld H(58,16),(r4+16)
	v8ld H(57,0),(r4+32)
	v8ld H(57,16),(r4+48)
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16adds -,HX(62,0),HX(63,0) SETF
	v32mov HY(0,0),-1 IFZ
	v32mov HY(1,0),-1 IFN
	v32mov HY(2,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16shl -,HX(62,0),HX(63,0) SETF
	v32mov HY(3,0),-1 IFZ
	v32mov HY(4,0),-1 IFN
	v32mov HY(5,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16and -,HX(62,0),HX(63,0) SETF
	v32mov HY(6,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16min -,HX(62,0),HX(63,0) SETF
	v32mov HY(7,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	vmull.ss -,HX(62,0),HX(63,0) SETF
	v32mov HY(8,0),-1 IFZ
	v32mov HY(9,0),-1 IFN
	v32mov HY(10,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16dist -,HX(62,0),HX(63,0) SETF
	v32mov HY(11,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16asr -,HX(62,0),HX(63,0) SETF
	v32mov HY(12,0),-1 IFC
	v16add -,HX(59,0),HX(59,0) SETF
	v16rsub -,HX(62,0),HX(63,0) SETF
	v32mov HY(13,0),-1 IFZ
	v32mov HY(14,0),-1 IFN
	v32mov HY(15,0),-1 IFC
	v16add H(16,0),H(58,0),H(57,0) SETF
	v32mov HY(17,0),-1 IFZ
	v32mov HY(18,0),-1 IFN
	v32mov HY(19,0),-1 IFC
	v32add -,HX(62,0),HX(63,0) SETF
	v32mov HY(20,0),-1 IFZ
	v32mov HY(21,0),-1 IFN
	v32mov HY(22,0),-1 IFC
	v16add HX(23,0),HX(62,0),HX(63,0) CLRA UACC SETF
	v32mov HY(24,0),-1 IFZ
	v32mov HY(25,0),-1 IFN
	v32mov HY(26,0),-1 IFC
	v32st HY(0++,0),(r0+=r3) REP64
	rts
