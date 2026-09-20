	.text
	.global _start
_start:
	mov r3,#64
	mov r5,#0
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v8ld H(20,0),(r4+64)		; data A
	v8ld H(21,0),(r4+96)		; data B
	v16mov HX(22,0),3		; an index vector of 3s
					; row 23 stays zero: an index vector of 0s
	mov r2,#0
	v8memwrite -,H(20,0),(r2)	; scalar index 0
	v8memread H(0,0),-,(r2)		; scalar read at 0
	v8memread H(1,0),H(20,0),H(23,0); vector index 0
	mov r2,#3
	v8memread H(2,0),-,(r2)		; scalar read at 3, before writing it
	v8memwrite -,H(21,0),(r2)	; scalar write at 3
	v8memread H(3,0),-,(r2)		; scalar read at 3
	v8memread H(4,0),H(20,0),H(22,0); vector index 3
	v16memwrite -,HX(20,0),(r2)	; a halfword write at scalar 3
	v16memread HX(5,0),-,(r2)	; and back
	v8memread H(6,0),-,(r2)		; what the halfword write left at byte 3
	v8memwrite -,H(20,0),0x5	; an immediate index
	v8memread H(7,0),-,0x5
	v32st HY(0++,0),(r0+=r3) REP64
	rts
