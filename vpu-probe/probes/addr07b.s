	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	mov r5,r1
	add r5,#8192		; a readable address of our own
	v32mov HY(0++,0),0 REP64
	v32mov HY(22,0),r5
	mov r2,#0
	v8memread H(10,0),-,(r2)	; the lookup table before
	mov r2,#1
	v8memread H(11,0),-,(r2)
	mov r2,#2
	v8memread H(12,0),-,(r2)
	v32mem07 HY(0,0),HY(22,0),HY(23,0)	; A = the address, B = zeros
	mov r2,#0
	v8memread H(13,0),-,(r2)	; and after
	mov r2,#1
	v8memread H(14,0),-,(r2)
	mov r2,#2
	v8memread H(15,0),-,(r2)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
