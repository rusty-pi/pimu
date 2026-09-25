	.text
	.global _start
_start:
	mov r4,r1
	add r4,#4096		; a scratch page, to be filled with pointers
	mov r5,r1		; each pointer one byte further into the marker page,
	mov r2,#16		; whose byte n holds n + 1
1:	st r5,(r4)
	add r4,#4
	add r5,#1
	sub r2,#1
	cmp r2,#0
	bne 1b
	mov r4,r1
	add r4,#4096		; back to the table
	mov r3,#64
	v32mov HY(0++,0),0 REP64
	v32mov HY(0,0),-1
	v8ld H(20,0),(r4)	; the table itself, as a plain load
	mov r2,#7
	st r2,(r0+4088)
	v8mem10 H(0,0),H(20,0),(r4)
	mov r2,#99
	st r2,(r0+4088)
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
