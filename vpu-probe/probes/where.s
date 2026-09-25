	.text
	.global _start
_start:
	mov r3,#0
	v32mov HY(0++,0),0 REP64
	v32ld HY(0,0),-,HY(21,0)	; the address-less load
	mov r5,#0
	v32ld HY(1,0),(r5)		; address 0
	mov r5,#0x80000000
	v32ld HY(2,0),(r5)		; the alias at 0x80000000
	mov r5,#0xc0000000
	v32ld HY(3,0),(r5)		; and the one at 0xc0000000
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
