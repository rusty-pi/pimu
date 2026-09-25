	.text
	.global _start
_start:
	mov r3,#64
	mov r4,r1
	add r4,#4096
	v32mov HY(0++,0),0 REP64
	v16ld HX(62,0),(r4+64)		; the vector under test
	v16mov -,HX(62,0) CLRA UACC	; accumulators = that vector
	vgetacc HY(0,0),HX(58,0),HX(58,0)	; what each lane holds
	vgetacc -,-,0 SUMS r5
	v32mov HY(1,0),r5
	vgetacc -,-,0 SUMU r5
	v32mov HY(2,0),r5
	vgetacc -,-,0 MAX r5
	v32mov HY(3,0),r5
	vgetacc -,-,0 IMIN r5
	v32mov HY(4,0),r5
	vgetacc -,-,0 IMAX r5
	v32mov HY(5,0),r5
	vgetacc -,-,1 SUMS r5		; the shift, with a discarded destination
	v32mov HY(6,0),r5
	vgetacc HY(7,0),-,0 SUMS r5	; a live destination beside the sum
	v32mov HY(8,0),r5
	v32st HY(0++,0),(r0+=r3) REP64
	rts
