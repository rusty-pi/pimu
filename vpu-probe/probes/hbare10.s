	.text
	.global _start
_start:
	mov r4,r1
	add r4,#4096
	mov r3,#64
	mov r2,#7
	st r2,(r0+4088)
	v8mem10 H(0,0),H(20,0),(r4)	; nothing vector has run since entry
	mov r2,#99
	st r2,(r0+4088)
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
