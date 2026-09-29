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
	; redirect table 71 -> ph71, 70 -> ph70
	mov r16,#0xfec01f1c
	ld r6,(r16)
	st r6,(r22+240)
	lea r10,ph71
	or r10,#1
	st r10,(r16)
	mov r16,#0xfec01f18
	ld r6,(r16)
	st r6,(r22+236)
	lea r10,ph70
	or r10,#1
	st r10,(r16)
	; save prio0, enable src71 (field7) and src70 (field6)
	ld r6,(r13+0x10)
	st r6,(r22+244)
	mov r7,#0x11000000
	or r6,r7
	st r6,(r13+0x10)
	mov r15,#0xffffffff
	st r15,(r13+0x50)

	; fill tstack (r0+0x6000 top) and sstack (r0+0x4000 top)
	mov r11,r0
	add r11,#0x2000
	mov r14,#0x1000
	mov r15,#0x5afec0de
fillst:
	st r15,(r11)
	add r11,#4
	sub r14,#1
	cmp r14,#0
	bne fillst

	mov r20,r0
	add r20,#0x6000		; tstack top
	mov r19,r0
	add r19,#0x4000		; sstack top (supervisor test stack)

	mov r9,#0
	mov r8,#0
	mov r21,#2		; nested mode
	mov r6,sp
	st r6,(r0+0xf10)
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)	; force src71
	ei
	mov r14,#80000
nwait:
	cmp r9,#0
	bne ndone
	sub r14,#1
	cmp r14,#0
	bne nwait
ndone:
	di
	ld r6,(r0+0xf10)
	mov sp,r6
	st r9,(r22+0)		; outer deliv
	st r8,(r22+4)		; nested deliv

	; restore
	ld r6,(r22+244)
	st r6,(r13+0x10)
	ld r6,(r22+240)
	mov r16,#0xfec01f1c
	st r6,(r16)
	ld r6,(r22+236)
	mov r16,#0xfec01f18
	st r6,(r16)
	mov r15,#0xffffffff
	st r15,(r13+0x50)
	st r15,(r13+0x54)
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

	; outer handler (src71): record, then force nested src70 from supervisor
ph71:
	mov r17,#0x7e002050
	mov r18,#0x80
	st r18,(r17)
	add r9,#1
	; record outer handler state
	mov r18,sp
	st r18,(r22+16)		; outer sp (banked supervisor stack)
	mov r18,r28
	st r18,(r22+20)		; outer r28 (== thread sp)
	mov r18,sr
	st r18,(r22+24)		; outer sr (supervisor)
	; save outer frame ptr, switch to my supervisor test stack
	mov r23,sp
	mov sp,r19
	; force src70 and enable delivery (still supervisor)
	mov r17,#0x7e002048
	mov r18,#0x40
	st r18,(r17)
	ei
	mov r14,#80000
n2wait:
	cmp r8,#0
	bne n2done
	sub r14,#1
	cmp r14,#0
	bne n2wait
n2done:
	di
	; restore outer frame ptr for our own rti
	mov sp,r23
	rti

	; nested handler (src70): entered from SUPERVISOR mode
ph70:
	mov r15,#0x7e002050
	mov r16,#0x40
	st r16,(r15)
	mov r15,sp
	st r15,(r22+32)		; NESTED handler sp
	mov r15,r28
	st r15,(r22+36)		; nested r28
	mov r15,sr
	st r15,(r22+40)		; nested sr
	ld r15,(sp)
	st r15,(r22+44)		; pushed sr (outer context)
	ld r15,(sp+4)
	st r15,(r22+48)		; pushed pc
	add r8,#1
	rti
