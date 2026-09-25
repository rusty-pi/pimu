	.text
	.global _start
_start:
	mov r3,#0
	mov r4,r1
	add r4,#4096
	mov r5,r1
	add r5,#8192		; scratch of our own, well inside the allocation
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)
	v8ld H(10,0),(r5)	; the scratch before
	v32mov HY(22,0),r5
	v8st H(20,0),-,HY(22,0)	; a store whose B slot holds a vector
	v8ld H(11,0),(r5)	; and after
	v8ld H(12,0),(r5+16)
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
