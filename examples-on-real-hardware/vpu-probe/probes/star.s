	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	mov r5,#2
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)
	v16ld HX(63,0),(r4+96)
	v16add HX(0++,0),HX(62,0),HX(63,0) REP4
	v16add HX(4++,0)*,HX(62,0),HX(63,0) REP4
	v16add HX(8++,0),HX(62,0)*,HX(63,0) REP4
	v16mov HX(12,0),HX(62,0)+r5
	v16mov HX(13,0),HX(62,0)+r5*
	v16ld HX(14++,0),(r4+64) REP2
	v16ld HX(16++,0)*,(r4+64) REP2
	v16st HX(62,0)*,(r4+256)
	v16ld HX(18,0),(r4+256)
	v32st HY(0++,0),(r0+=r3) REP64
	rts
