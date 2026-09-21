	.text
	.global _start
_start:
	mov r3,#0
	mov r4,r1
	add r4,#4096
	mov r5,#0
	v32mov HY(0++,0),0 REP64
	v8ld H(19,0),(r4+64)		; data worth recognising
	v8ld H(21,0),(r4+96)
	v16mov HX(24,0),16
	v16mov -,HX(24,0) CLRA SACCH	; an index of 16, as the scatter takes it
	v32ld HY(10,0),(r5)		; address 0 before
	v8indexwritem H(0,0),H(19,0),H(21,0)	; three vector slots, no address
	v16indexwritem H(1,0),HX(19,0),H(21,0)
	v32ld HY(11,0),(r5)		; address 0 after
	mov r3,#64
	v32st HY(0++,0),(r0+=r3) REP64
	rts
