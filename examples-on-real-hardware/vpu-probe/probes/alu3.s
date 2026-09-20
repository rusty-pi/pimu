	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16add -,HX(59,0),HX(59,0) SETF
	v16signshl HX(0,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op13 HX(1,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16signasl HX(2,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16signasls HX(3,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op22 HX(4,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op23 HX(5,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16clips HX(6,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16testmag HX(7,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16addc HX(8,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16addsc HX(9,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16subc HX(10,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16subsc HX(11,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16rsubc HX(12,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16rsubsc HX(13,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op44 HX(14,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op45 HX(15,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op46 HX(16,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16op47 HX(17,0),HX(62,0),HX(63,0)
	v32mov HY(59,0),-1
	v16add -,HX(59,0),HX(59,0) SETF
	v16addc HX(18,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16addsc HX(19,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16subc HX(20,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16subsc HX(21,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16rsubc HX(22,0),HX(62,0),HX(63,0)
	v16add -,HX(59,0),HX(59,0) SETF
	v16rsubsc HX(23,0),HX(62,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
