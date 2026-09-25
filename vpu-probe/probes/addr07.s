	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	mov r5,r1
	add r5,#8192		; scratch, well inside our own allocation
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)
	v8ld H(21,0),(r4+96)
	v32mov HY(22,0),r5	; a candidate address, the same in every lane
	v8ld H(10,0),(r5)	; the scratch before
	v8ld H(11,0),(r5+16)
	v32mov HY(0,0),-1
	v32mem07 HY(0,0),HY(22,0),HY(21,0)	; A = the address
	v8ld H(12,0),(r5)	; after
	v8ld H(13,0),(r5+16)
	v32mov HY(1,0),-1
	v32mem07 HY(1,0),HY(20,0),HY(22,0)	; B = the address
	v8ld H(14,0),(r5)
	v8ld H(15,0),(r5+16)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
