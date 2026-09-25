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
	v16mov H(0,0),H(60,0)
	v16add H(1,0),H(60,0),H(61,0)
	v16adds H(2,0),H(60,0),H(61,0)
	v16sub H(3,0),H(60,0),H(61,0)
	v16subs H(4,0),H(60,0),H(61,0)
	v16shl H(5,0),H(60,0),H(61,0)
	v16lsr H(6,0),H(60,0),H(61,0)
	v16asr H(7,0),H(60,0),H(61,0)
	v16and H(8,0),H(60,0),H(61,0)
	v16min H(9,0),H(60,0),H(61,0)
	v16dist H(10,0),H(60,0),H(61,0)
	v16even H(11,0),H(60,0),H(61,0)
	v16count H(12,0),H(60,0),H(61,0)
	v16max H(13,0),H(60,0),H(61,0)
	v16add HX(14,0),H(60,0),H(61,0)
	v16adds HX(15,0),H(60,0),H(61,0)
	v16mov HX(16,0),H(60,0)
	v32add H(17,0),H(60,0),H(61,0)
	v32adds H(18,0),H(60,0),H(61,0)
	v32mov H(19,0),H(60,0)
	v32add HX(20,0),HX(62,0),HX(63,0)
	v32adds HX(21,0),HX(62,0),HX(63,0)
	v32mov HX(22,0),HX(62,0)
	v32add HY(23,0),HX(62,0),HX(63,0)
	v16add H(24,0),H(60,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
