	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(60,0),(r4)
	v16ld HX(61,0),(r4+32)
	v16add HX(0,0),HX(60,0),HX(61,0)
	v16adds HX(1,0),HX(60,0),HX(61,0)
	v16addc HX(2,0),HX(60,0),HX(61,0)
	v16addsc HX(3,0),HX(60,0),HX(61,0)
	v16sub HX(4,0),HX(60,0),HX(61,0)
	v16subs HX(5,0),HX(60,0),HX(61,0)
	v16rsub HX(6,0),HX(60,0),HX(61,0)
	v16rsubs HX(7,0),HX(60,0),HX(61,0)
	v16min HX(8,0),HX(60,0),HX(61,0)
	v16max HX(9,0),HX(60,0),HX(61,0)
	v16asr HX(10,0),HX(60,0),HX(61,0)
	v16lsr HX(11,0),HX(60,0),HX(61,0)
	v16shl HX(12,0),HX(60,0),HX(61,0)
	v16shls HX(13,0),HX(60,0),HX(61,0)
	v16dist HX(14,0),HX(60,0),HX(61,0)
	v16dists HX(15,0),HX(60,0),HX(61,0)
	v16clip HX(16,0),HX(60,0),HX(61,0)
	v16clips HX(17,0),HX(60,0),HX(61,0)
	v16sign HX(18,0),HX(60,0),HX(61,0)
	v16count HX(19,0),HX(60,0),HX(61,0)
	v16bitrev HX(20,0),HX(60,0),HX(61,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
