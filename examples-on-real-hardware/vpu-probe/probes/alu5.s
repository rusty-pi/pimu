	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32mov HY(0++,0),-1 REP32
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16op13 HX(0,0),HX(62,0),HX(63,0)
	v32op13 HY(1,0),HX(62,0),HX(63,0)
	v16op22 HX(2,0),HX(62,0),HX(63,0)
	v32op22 HY(3,0),HX(62,0),HX(63,0)
	v16op23 HX(4,0),HX(62,0),HX(63,0)
	v32op23 HY(5,0),HX(62,0),HX(63,0)
	v16op44 HX(6,0),HX(62,0),HX(63,0)
	v32op44 HY(7,0),HX(62,0),HX(63,0)
	v16op45 HX(8,0),HX(62,0),HX(63,0)
	v32op45 HY(9,0),HX(62,0),HX(63,0)
	v16op46 HX(10,0),HX(62,0),HX(63,0)
	v32op46 HY(11,0),HX(62,0),HX(63,0)
	v16op47 HX(12,0),HX(62,0),HX(63,0)
	v32op47 HY(13,0),HX(62,0),HX(63,0)
	v16clips HX(14,0),HX(62,0),HX(63,0)
	v32clips HY(15,0),HX(62,0),HX(63,0)
	v16testmag HX(16,0),HX(62,0),HX(63,0)
	v32testmag HY(17,0),HX(62,0),HX(63,0)
	v16count HX(18,0),HX(62,0),HX(63,0)
	v32count HY(19,0),HX(62,0),HX(63,0)
	v16signshl HX(20,0),HX(62,0),HX(63,0)
	v32signshl HY(21,0),HX(62,0),HX(63,0)
	v16msb HX(22,0),HX(62,0),HX(63,0)
	v32msb HY(23,0),HX(62,0),HX(63,0)
	v16bitrev HX(24,0),HX(62,0),HX(63,0)
	v32bitrev HY(25,0),HX(62,0),HX(63,0)
	v16dist HX(26,0),HX(62,0),HX(63,0)
	v32dist HY(27,0),HX(62,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
