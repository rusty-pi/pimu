	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),-1 REP32
	v32mov HY(32++,0),0 REP32
	v8ld H(60,0),(r4)
	v8ld H(60,16),(r4+16)
	v8ld H(61,0),(r4+32)
	v8ld H(61,16),(r4+48)
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v32count H(0,0),H(60,0),H(61,0)
	v16count H(1,0),H(60,0),H(61,0)
	v16bitrev H(2,0),H(60,0),H(61,0)
	v32bitrev H(3,0),H(60,0),H(61,0)
	v16ror H(4,0),H(60,0),H(61,0)
	v16msb H(5,0),H(60,0),H(61,0)
	v16clip H(6,0),H(60,0),H(61,0)
	v16clips H(7,0),H(60,0),H(61,0)
	v16sign H(8,0),H(60,0),H(61,0)
	v16testmag H(9,0),H(60,0),H(61,0)
	v16interl H(10,0),H(60,0),H(61,0)
	v16interh H(11,0),H(60,0),H(61,0)
	v16odd H(12,0),H(60,0),H(61,0)
	v16signshl H(13,0),H(60,0),H(61,0)
	v16shls H(14,0),H(60,0),H(61,0)
	v16dists H(15,0),H(60,0),H(61,0)
	v16addc H(16,0),H(60,0),H(61,0)
	v32even H(17,0),H(60,0),H(61,0)
	v32bitrev HX(18,0),HX(62,0),HX(63,0)
	v16bitrev HX(19,0),H(60,0),H(61,0)
	v16msb HX(20,0),H(60,0),H(61,0)
	v16count HX(21,0),H(60,0),H(61,0)
	v32msb HY(22,0),HX(62,0),HX(63,0)
	v32clip HY(23,0),HX(62,0),HX(63,0)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
