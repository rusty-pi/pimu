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
	; redirect table 71 -> ph71, swi entry 0x20 -> pswi
	mov r16,#0xfec01f1c
	ld r6,(r16)
	st r6,(r22+240)
	lea r10,ph71
	or r10,#1
	st r10,(r16)
	mov r16,#0xfec01e80
	ld r6,(r16)
	st r6,(r22+236)
	lea r10,pswi
	st r10,(r16)
	ld r6,(r13+0x10)
	st r6,(r22+244)
	mov r7,#0x10000000
	or r6,r7
	st r6,(r13+0x10)
	mov r15,#0xffffffff
	st r15,(r13+0x50)

	; fill tstack (r0+0x6000 top) and ustack (r0+0x4000 top)
	mov r11,r0
	add r11,#0x2000
	mov r14,#0x1000
	mov r15,#0x5afec0de
fillu:
	st r15,(r11)
	add r11,#4
	sub r14,#1
	cmp r14,#0
	bne fillu

	mov r20,r0
	add r20,#0x6000		; tstack top
	mov r19,r0
	add r19,#0x4000		; ustack top

	mov r9,#0
	mov r6,sp
	st r6,(r0+0xf10)
	; enter user mode via rti, with src71 forced so it delivers in user mode
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)	; force src71
	sub sp,sp,#8
	lea r6,user_code
	st r6,(sp+4)
	mov r6,#0xc0000000	; user (bit31) + IE (bit30)
	st r6,(sp)
	rti

user_code:
	mov r15,sr
	st r15,(r22+16)		; user sr
	mov r15,sp
	st r15,(r22+20)		; user sp
	mov r15,r28
	st r15,(r22+24)		; user r28
	st r9,(r22+28)		; deliveries seen in user mode
	; try to regain supervisor + disable IE via mov sr
	mov r15,#0x20000000
	mov sr,r15
	mov r15,sr
	st r15,(r22+32)		; sr after attempted mov sr from user
	swi 0
	b .

sup_land:
	di
	ld r6,(r0+0xf00)
	mov sr,r6		; back to entry (non-sup) mode -> sp = stock SP_nonsup
	ld r6,(r0+0xf04)
	mov sp,r6
	mov r6,#1
	st r6,(r22+60)		; returned to supervisor thread ok

	ld r6,(r22+244)
	st r6,(r13+0x10)
	ld r6,(r22+240)
	mov r16,#0xfec01f1c
	st r6,(r16)
	ld r6,(r22+236)
	mov r16,#0xfec01e80
	st r6,(r16)
	mov r15,#0xffffffff
	st r15,(r13+0x50)
	ld r6,(r0+0xf0c)
	mov lr,r6
	ldm r6-r23,(sp++)
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts

	; interrupt taken (possibly from user mode)
ph71:
	mov r17,#0x7e002050
	mov r18,#0x80
	st r18,(r17)
	mov r18,sp
	st r18,(r22+40)		; handler sp (banked?)
	mov r18,r28
	st r18,(r22+44)		; r28
	mov r18,sr
	st r18,(r22+48)		; handler sr
	ld r18,(sp)
	st r18,(r22+52)		; pushed sr (user context)
	add r9,#1
	rti

	; swi 0 handler: exit user mode back to supervisor thread
pswi:
	mov r16,sr
	st r16,(r22+56)		; swi handler sr
	ld r16,(sp)
	st r16,(r22+64)		; pushed sr at swi (user context)
	ld r16,(sp+4)
	st r16,(r22+68)		; pushed pc at swi
	lea r16,sup_land
	st r16,(sp+4)
	mov r16,#0x20000000
	st r16,(sp)
	rti
