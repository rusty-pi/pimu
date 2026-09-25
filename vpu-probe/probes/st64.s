	.text
	.global _start
_start:
	mov r3,#0
	mov r4,r1
	add r4,#4096
	mov r5,#0
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; something recognisable to store
	v8ld H(19,0),(r4+96)
	v32ld HY(10,0),(r5)		; address 0 before
	v8st H(20,0),H(19,0),HY(21,0)	; a store with a vector in every slot
	v8indexwritem H(0,0),-,0xa	; a scatter with nothing to write
	v8indexwritem H(1,0),-,H(21,0)
	v32ld HY(11,0),(r5)		; address 0 after
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
