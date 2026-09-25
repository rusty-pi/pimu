	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; the value a normal `writelut` writes
	v32mov HY(0++,0),-1 REP8
	v8memwrite H(0,0),H(20,0),H(21,0)	; index 0 in every lane's bank
	v8memread H(1,0),-,H(21,0)		; read it back
	v8memwrite H(2,0),-,H(21,0)		; and again with a dash A
	v8memread H(3,0),-,H(21,0)		; what is in the table now
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
