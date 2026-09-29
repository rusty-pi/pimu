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

	lea r23,ptab
	mov r5,#0
runloop:
	ldb r19,(r23)
	ldb r21,(r23+1)
	add r23,#2
	mov r9,#0
	cmp r21,#0
	beq skipforce
	mov r15,#0x80
	st r15,(r13+0x48)
skipforce:
	mov r6,sp
	st r6,(r0+0xf10)
	mov sp,r20
	mov r2,#0
	cmp r2,r19
	ei
	.rept 64
	nop
	.endr
	di
	mov r10,#0
	mov r11,#0
	mov r12,#0
	mov r14,#0
	bne 2f
	mov r10,#1
2:	bpl 3f
	mov r11,#2
3:	bcc 4f
	mov r12,#4
4:	bvc 5f
	mov r14,#8
5:	or r10,r11
	or r10,r12
	or r10,r14
	ld r6,(r0+0xf10)
	mov sp,r6
	mov r6,r5
	shl r6,#4
	add r6,r6,r22
	st r10,(r6)
	st r9,(r6+4)
	ld r7,(r22+248)
	st r7,(r6+8)
	add r5,#1
	cmp r5,#6
	blt runloop

	ld r6,(r22+244)
	st r6,(r13+0x10)
	ld r6,(r22+240)
	mov r16,#0xfec01f1c
	st r6,(r16)
	mov r15,#0xffffffff
	st r15,(r13+0x50)
	ld r6,(r0+0xf00)
	mov sr,r6
	ld r6,(r0+0xf0c)
	mov lr,r6
	ldm r6-r23,(sp++)
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts

ph71:
	mov r17,#0x7e002050
	mov r18,#0x80
	st r18,(r17)
	ld r18,(sp)
	st r18,(r22+248)
	add r9,#1
	cmp r21,#4
	beq phflip
	cmp r21,#3
	beq phclob
	rti
phflip:
	ld r18,(sp)
	eor r18,#15
	st r18,(sp)
phclob:
	mov r18,#1
	cmp r18,#0
	rti

	.balign 4
ptab:
	.byte 0,0
	.byte 1,0
	.byte 0,3
	.byte 1,3
	.byte 0,4
	.byte 1,4
