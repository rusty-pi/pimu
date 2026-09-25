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
	v16signshl HX(0,0),HX(62,0),HX(63,0)
	v16signasl HX(1,0),HX(62,0),HX(63,0)
	v16signasls HX(2,0),HX(62,0),HX(63,0)
	v16op13 HX(3,0),HX(62,0),HX(63,0)
	v16op22 HX(4,0),HX(62,0),HX(63,0)
	v16op23 HX(5,0),HX(62,0),HX(63,0)
	v16op44 HX(6,0),HX(62,0),HX(63,0)
	v16op45 HX(7,0),HX(62,0),HX(63,0)
	v16op46 HX(8,0),HX(62,0),HX(63,0)
	v16op47 HX(9,0),HX(62,0),HX(63,0)
	v16clips HX(10,0),HX(62,0),HX(63,0)
	v32clips HY(11,0),HX(62,0),HX(63,0)
	v16clip HX(12,0),HX(62,0),HX(63,0)
	v16testmag HX(13,0),HX(62,0),HX(63,0)
	v16shl HX(14,0),HX(62,0),HX(63,0)
	v16asr HX(15,0),HX(62,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
