	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32ld HY(20,0),(r4+64)
	v32mov HY(0++,0),-1 REP8
	v16or HY(0,0),HY(20,0),0	; A wider *and* D wider: does the op truncate?
	v16adds HX(1,0),HY(20,0),1	; a saturating op over a wide A
	v16adds HX(2,0),HX(20,0),1	; and its control
	v16shl HX(3,0),HY(20,0),4	; a shift over a wide A
	v16shl HX(4,0),HX(20,0),4	; and its control
	v16subs HX(5,0),HY(20,0),1
	v16subs HX(6,0),HX(20,0),1
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
