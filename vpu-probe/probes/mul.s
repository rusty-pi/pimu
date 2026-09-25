	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(60,0),(r4)
	v16ld HX(61,0),(r4+32)
	vmull.ss HX(0,0),HX(60,0),HX(61,0)
	vmulls.ss HX(1,0),HX(60,0),HX(61,0)
	vmulm.ss HX(2,0),HX(60,0),HX(61,0)
	vmulms.ss HX(3,0),HX(60,0),HX(61,0)
	vmulhd.ss HX(4,0),HX(60,0),HX(61,0)
	vmulhd.su HX(5,0),HX(60,0),HX(61,0)
	vmulhd.us HX(6,0),HX(60,0),HX(61,0)
	vmulhd.uu HX(7,0),HX(60,0),HX(61,0)
	vmulhn.ss HX(8,0),HX(60,0),HX(61,0)
	vmulhn.su HX(9,0),HX(60,0),HX(61,0)
	vmulhn.us HX(10,0),HX(60,0),HX(61,0)
	vmulhn.uu HX(11,0),HX(60,0),HX(61,0)
	vmulhdt.ss HX(12,0),HX(60,0),HX(61,0)
	vmulhdt.su HX(13,0),HX(60,0),HX(61,0)
	vmul32.ss HX(14,0),HX(60,0),HX(61,0)
	vmul32.su HX(15,0),HX(60,0),HX(61,0)
	vmul32.us HX(16,0),HX(60,0),HX(61,0)
	vmul32.uu HX(17,0),HX(60,0),HX(61,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
