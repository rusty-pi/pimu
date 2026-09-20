	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	mov r5,#0x1234
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16mov -,HX(62,0) CLRA UACC
	v16add HX(0,0),HX(62,0),HX(63,0) CLRA
	vgetacc HY(1,0),HX(56,0),HX(56,0)
	v16mov -,HX(62,0) CLRA UACC
	v16add HX(2,0),HX(62,0),HX(63,0) WBA
	vgetacc HY(3,0),HX(56,0),HX(56,0)
	v16bitplanes -,r5 SETF
	v16add -,HX(62,0),HX(63,0) IFZ SETF
	v32mov HY(4,0),-1 IFZ
	v32mov HY(5,0),-1 IFN
	v16bitplanes -,r5 SETF
	v16add -,HX(62,0),HX(63,0) IFNZ SETF
	v32mov HY(6,0),-1 IFZ
	v32mov HY(7,0),-1 IFN
	v32st HY(0++,0),(r0+=r3) REP64
	rts
