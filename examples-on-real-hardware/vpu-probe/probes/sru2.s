	.text
	.global _start
_start:
	mov r3,#64
	mov r6,r0
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	mov r0,#0
	v16mov -,HX(62,0) max2 r0
	v32mov HY(0,0),r0
	mov r0,#0
	v16mov -,HX(62,0) max4 r0
	v32mov HY(1,0),r0
	mov r0,#0
	v16mov -,HX(62,0) max6 r0
	v32mov HY(2,0),r0
	mov r0,#0
	v16mov -,HX(62,0) MAX r0
	v32mov HY(3,0),r0
	mov r0,#0
	v16mov -,HX(62,0) IMIN r0
	v32mov HY(4,0),r0
	mov r0,#0
	v16mov -,HX(62,0) IMAX r0
	v32mov HY(5,0),r0
	mov r0,#0
	v16mov -,HX(62,0) SUMU r0
	v32mov HY(6,0),r0
	v16mov HX(61,0),HX(63,0)
	mov r0,#0
	v16mov -,HX(61,0) IMIN r0
	v32mov HY(7,0),r0
	mov r0,#0
	v16mov -,HX(61,0) IMAX r0
	v32mov HY(8,0),r0
	mov r0,r6
	v32st HY(0++,0),(r0+=r3) REP64
	rts
