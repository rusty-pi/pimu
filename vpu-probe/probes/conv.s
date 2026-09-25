	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r0
	add r4,#1024
	mov r5,r4
	add r5,#64
	v32mov HY(0++,0),0 REP16
	v32ld HY(0,0),(r1)
	v8ld H(1,0),(r1)
	v8st HY(0,0),(r4)
	v32st H(1,0),(r5)
	v32st HY(0++,0),(r0+=r3) REP16
	rts
