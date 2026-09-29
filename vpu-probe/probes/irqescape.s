	.text
	.global _start
_start:
	mov r6,sr
	st r6,(r0+0xf00)
	di
	stm r6-r23,(--sp)
	mov r6,lr
	st r6,(r0+0xf0c)
	mov r6,sp
	st r6,(r0+0xf04)

	mov r13,#0x7e002000
	mov r22,r0
	add r22,#0x100
	mov r16,#0xfec01f1c
	ld r6,(r16)
	st r6,(r22+240)
	lea r10,ph71
	or r10,#1
	st r10,(r16)
	ld r6,(r13+0x10)
	st r6,(r22+244)
	mov r7,#0x10000000
	or r6,r7
	st r6,(r13+0x10)
	mov r15,#0xffffffff
	st r15,(r13+0x50)

	mov r20,r0
	add r20,#0x6000

	; ---- run A: normal delivery, PENDING after ----
	mov r21,#0
	bl deliver
	st r9,(r22+0)
	ld r6,(r13+0x04)
	st r6,(r22+4)

	; ---- run B: handler leaves by BRANCH (no rti) ----
	mov r21,#6
	mov r9,#0
	mov r6,sp
	st r6,(r0+0xf18)
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)
	ei
	mov r14,#40000
bwait:
	cmp r9,#0
	bne besc
	sub r14,#1
	cmp r14,#0
	bne bwait
besc:
	; if the handler escaped by branch, it lands at b_land; if it rti'd, we reach here
	di
	ld r6,(r0+0xf18)
	mov sp,r6
	b b_done
b_land:
	di
	ld r6,(r0+0xf18)
	mov sp,r6
b_done:
	st r9,(r22+8)
	ld r6,(r13+0x04)
	st r6,(r22+12)
	; after escape, a normal delivery
	mov r21,#0
	bl deliver
	st r9,(r22+16)
	ld r6,(r13+0x04)
	st r6,(r22+20)

	; ---- run C: unmatched rti from thread context ----
	mov r19,r20
	sub r19,#8
	mov r6,sr
	st r6,(r19)
	lea r6,c_after
	st r6,(r19+4)
	mov r6,sp
	st r6,(r0+0xf10)
	mov sp,r19
	rti
c_after:
	di
	ld r6,(r0+0xf10)
	mov sp,r6
	mov r6,#1
	st r6,(r22+24)
	mov r21,#0
	bl deliver
	st r9,(r22+28)
	ld r6,(r13+0x04)
	st r6,(r22+32)

	ld r6,(r22+244)
	st r6,(r13+0x10)
	ld r6,(r22+240)
	mov r16,#0xfec01f1c
	st r6,(r16)
	mov r15,#0xffffffff
	st r15,(r13+0x50)
	ld r6,(r0+0xf00)
	mov sr,r6
	ld r6,(r0+0xf04)
	mov sp,r6
	ld r6,(r0+0xf0c)
	mov lr,r6
	ldm r6-r23,(sp++)
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts

deliver:
	mov r9,#0
	mov r6,sp
	st r6,(r0+0xf18)
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)
	ei
	mov r14,#40000
dwait:
	cmp r9,#0
	bne ddone
	sub r14,#1
	cmp r14,#0
	bne dwait
ddone:
	di
	ld r6,(r0+0xf18)
	mov sp,r6
	rts

ph71:
	mov r17,#0x7e002050
	mov r18,#0x80
	st r18,(r17)
	add r9,#1
	cmp r21,#6
	beq ph_escape
	rti
ph_escape:
	mov sp,r20
	b b_land
