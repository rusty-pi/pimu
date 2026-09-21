	.text
	.global _start
_start:
	mov r3,#0
	mov r4,r1
	add r4,#4096
	mov r5,#0
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; the data a store would move
	v32ld HY(10,0),(r5)		; address 0..63 before
	v32ld HY(11,0),(r5+64)		; address 64..127 before
	v8st H(20,0),-,HY(21,0)		; the store whose B slot holds a vector
	v32ld HY(12,0),(r5)		; and after
	v32ld HY(13,0),(r5+64)
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
