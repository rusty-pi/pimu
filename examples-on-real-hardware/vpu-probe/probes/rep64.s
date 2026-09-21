	.text
	.global _start
_start:
	mov r3,#64
	v32mov HY(0++,0),0 REP64
	v32mov HY(1,0),-1
	v8mem11 H(0++,0),H(23,0),H(24,0) REP64	; sixty-four of them in one go
	v32mov HY(2,0),-1			; a marker the dump will show
	v32st HY(0++,0),(r0+=r3) REP64
	rts
