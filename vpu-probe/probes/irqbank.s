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
	mov r6,r28
	st r6,(r0+0xf08)

	mov r13,#0x7e002000
	mov r22,r0
	add r22,#0x100

	; save + redirect stock vector entry 71 -> ph71|1
	mov r16,#0xfec01f1c		; table base 0xfec01e00 + 284
	ld r6,(r16)
	st r6,(r22+64)			; saved entry71
	lea r10,ph71
	or r10,#1
	st r10,(r16)
	ld r6,(r16)
	st r6,(r22+68)			; readback (writability test)

	; save prio word0, enable src71 (field7=prio1) keeping others
	ld r6,(r13+0x10)
	st r6,(r22+72)			; saved prio0
	mov r7,#0x10000000
	or r6,r7
	st r6,(r13+0x10)
	mov r15,#0xffffffff
	st r15,(r13+0x50)		; clear any pending

	; fill tstack
	mov r11,r0
	add r11,#0x5000
	mov r14,#0x800
	mov r15,#0x5afec0de
4:	st r15,(r11)
	add r11,#4
	sub r14,#1
	cmp r14,#0
	bne 4b

	mov r9,#0
	mov r20,r0
	add r20,#0x6000
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)		; force src71
	ei
	mov r14,#40000
5:	cmp r9,#0
	bne 6f
	sub r14,#1
	cmp r14,#0
	bne 5b
6:	di
	ld r6,(r0+0xf04)
	mov sp,r6
	st r9,(r22+40)
	; scan tstack touched
	mov r11,r0
	add r11,#0x5000
	mov r14,#0x800
	mov r12,#0
7:	ld r15,(r11)
	mov r6,#0x5afec0de
	cmp r15,r6
	beq 8f
	add r12,#1
8:	add r11,#4
	sub r14,#1
	cmp r14,#0
	bne 7b
	st r12,(r22+52)

	; restore prio0, table71, pending clear
	ld r6,(r22+72)
	st r6,(r13+0x10)
	ld r6,(r22+64)
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
	mov r18,sp
	st r18,(r22)
	mov r18,r28
	st r18,(r22+4)
	mov r18,sr
	st r18,(r22+8)
	ld r18,(sp)
	st r18,(r22+12)
	ld r18,(sp+4)
	st r18,(r22+16)
	ld r18,(sp+8)
	st r18,(r22+24)
	mov r17,#0x7e002004
	ld r18,(r17)
	st r18,(r22+20)
	mov r17,#0x7e002050
	mov r18,#0x80
	st r18,(r17)
	add r9,#1
	rti
