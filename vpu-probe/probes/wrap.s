	.text
	.global _start
; r0 = output page, r1 = marker page
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP64
	v32ld HY(0,0),(r1)
	v16st HY(0,0),(r0+7)
	rts
