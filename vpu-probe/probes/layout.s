	.text
	.global _start
; r0 = output page, r1 = a page of memory whose byte n holds n+1
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP16
	v8ld H(0,0),(r1)
	v16ld H(1,0),(r1)
	v32ld H(2,0),(r1)
	v8ld HX(3,0),(r1)
	v16ld HX(4,0),(r1)
	v32ld HX(5,0),(r1)
	v8ld HY(6,0),(r1)
	v16ld HY(7,0),(r1)
	v32ld HY(8,0),(r1)
	v8ld H(9,16),(r1)
	v16ld HX(10,32),(r1)
	v8ld H(11,48),(r1)
	v32st HY(0++,0),(r0+=r3) REP16
	rts
