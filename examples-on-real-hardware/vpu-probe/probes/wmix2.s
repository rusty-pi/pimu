	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(60,0),(r4)
	v8ld H(60,16),(r4+16)
	v8ld H(61,0),(r4+32)
	v8ld H(61,16),(r4+48)
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v32mov HY(0,0),H(60,0)
	v32add HY(1,0),H(60,0),H(61,0)
	v32mov HY(2,0),HX(62,0)
	v32sub HY(3,0),HX(62,0),HX(63,0)
	v16mov HX(4,0),H(60,0)
	v32adds HX(5,0),HX(62,0),HX(63,0)
	v32subs HX(6,0),HX(62,0),HX(63,0)
	v32adds H(7,0),H(60,0),H(61,0)
	v32subs H(8,0),H(60,0),H(61,0)
	v32adds HY(9,0),HX(62,0),HX(63,0)
	v32sub HX(10,0),HX(62,0),HX(63,0)
	v16adds HX(11,0),HX(62,0),HX(63,0)
	v16mov H(12,0),HX(62,0)
	v32mov H(13,0),HX(62,0)
	v32mov HX(14,0),H(60,0)
	v16dist HX(15,0),H(60,0),H(61,0)
	v16sub HX(16,0),H(60,0),H(61,0)
	v16subs HX(17,0),H(60,0),H(61,0)
	v16asr HX(18,0),HX(62,0),HX(63,0)
	v32asr HX(19,0),HX(62,0),HX(63,0)
	v32lsr HY(20,0),HX(62,0),HX(63,0)
	v32min HY(21,0),HX(62,0),HX(63,0)
	v32count HY(22,0),H(60,0),H(61,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
