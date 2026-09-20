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
	v16add -,HX(62,0),HX(63,0) SUMU r0
	v32mov HY(0,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) SUMS r0
	v32mov HY(1,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) max2 r0
	v32mov HY(2,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) IMIN r0
	v32mov HY(3,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) max4 r0
	v32mov HY(4,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) IMAX r0
	v32mov HY(5,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) max6 r0
	v32mov HY(6,0),r0
	mov r0,#0
	v16add -,HX(62,0),HX(63,0) MAX r0
	v32mov HY(7,0),r0
	mov r0,#0
	v16mov -,HX(62,0) SUMU r0
	v32mov HY(8,0),r0
	mov r0,#0
	v16mov -,HX(62,0) SUMS r0
	v32mov HY(9,0),r0
	mov r0,#0
	v16mov -,HX(62,0) max2 r0
	v32mov HY(10,0),r0
	mov r0,#0
	v16mov -,HX(62,0) IMIN r0
	v32mov HY(11,0),r0
	mov r0,#0
	v16mov -,HX(62,0) max4 r0
	v32mov HY(12,0),r0
	mov r0,#0
	v16mov -,HX(62,0) IMAX r0
	v32mov HY(13,0),r0
	mov r0,#0
	v16mov -,HX(62,0) max6 r0
	v32mov HY(14,0),r0
	mov r0,#0
	v16mov -,HX(62,0) MAX r0
	v32mov HY(15,0),r0
	mov r0,#0
	v32mov -,HX(62,0) SUMU r0
	v32mov HY(16,0),r0
	mov r0,#0
	v32mov -,HX(62,0) SUMS r0
	v32mov HY(17,0),r0
	mov r0,#0
	v32mov -,HX(62,0) IMIN r0
	v32mov HY(18,0),r0
	mov r0,#0
	v32mov -,HX(62,0) IMAX r0
	v32mov HY(19,0),r0
	mov r0,#0
	v32mov -,HX(62,0) MAX r0
	v32mov HY(20,0),r0
	mov r0,#0
	v16add HX(30,0),HX(62,0),HX(63,0) SUMU r0
	v32mov HY(21,0),r0
	mov r0,r6
	v32st HY(0++,0),(r0+=r3) REP64
	rts
