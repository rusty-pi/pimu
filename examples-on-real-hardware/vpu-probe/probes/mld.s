	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32mov HY(22,0),4		; a small per-lane value
	v32ld HY(0,0),-+r1,HY(21,0)	; B all zeros, A a dash naming r1
	v32ld HY(1,0),-,HY(21,0)	; the same without the addend
	v32ld HY(2,0),-+r1,HY(22,0)	; B = 4 in every lane
	v32ld HY(3,0),HY(21,0),HY(22,0)	; a vector in the A position too
	v32st HY(0++,0),(r0+=r3) REP64
	rts
