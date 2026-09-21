	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; something in the inert slot to be ignored
	v32mov HY(0++,0),-1 REP8
	v8ld H(0,0),-,H(21,0) REP2	; 80-bit, inert slot a bare dash
	v8ld H(2,0),H(20,0),H(21,0) REP2	; 80-bit, inert slot naming a register
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
