	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v32mov HY(0,0),-1		; a destination preset, so a zero shows
	v32mov HY(1,0),-1		; a destination preset, so a zero shows
	v32mov HY(2,0),-1		; a destination preset, so a zero shows
	v32mov HY(3,0),-1		; a destination preset, so a zero shows
	v32mov HY(4,0),-1		; a destination preset, so a zero shows
	v32mov HY(5,0),-1		; a destination preset, so a zero shows
	mov r2,#0
	st r2,(r0+4088)
	add r4,#64
	mov r2,#1
	st r2,(r0+4088)
	v8ld H(20,0),(r4)
	mov r2,#10
	st r2,(r0+4088)
	v8mem10 H(0,0),H(20,0),(r4)
	mov r2,#16
	st r2,(r0+4088)
	v8mem16 H(1,0),H(20,0),(r4)
	mov r2,#19
	st r2,(r0+4088)
	v8mem19 H(2,0),H(20,0),(r4)
	mov r2,#28
	st r2,(r0+4088)
	v8mem28 H(3,0),H(20,0),(r4)
	mov r2,#30
	st r2,(r0+4088)
	v8mem30 H(4,0),H(20,0),(r4)
	mov r2,#99
	st r2,(r0+4088)
	v32st HY(0++,0),(r0+=r3) REP64
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
