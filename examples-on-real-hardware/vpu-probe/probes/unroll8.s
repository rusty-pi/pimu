	.text
	.global _start
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP64
	.rept 8
	v8mem11 H(0,0),H(23,0),H(24,0)	; eight of them, straight through
	.endr
	v32mov HY(2,0),-1		; the marker the harness watches for
	v32st HY(0++,0),(r0+=r3) REP64
	rts
