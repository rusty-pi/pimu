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

	; tstack top r0+0x6000, ostack top r0+0x5000
	mov r20,r0
	add r20,#0x6000
	mov r19,r0
	add r19,#0x5000
	; plant a frame on ostack: [otop-8]=sr, [otop-4]=resume pc
	sub r19,#8
	mov r6,sr
	st r6,(r19)
	lea r6,hsw_other
	st r6,(r19+4)
	st r19,(r22+40)		; oframe addr

	mov r9,#0
	mov r21,#5		; handler mode: switch via r19 frame
	; save stock sp, switch to tstack
	mov r6,sp
	st r6,(r0+0xf10)
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)
	ei
	.rept 64
	nop
	.endr
	di
	; if we get here the switch did NOT happen
	mov r6,#0xdead
	st r6,(r22+44)
	b hsw_done

hsw_other:
	; arrived through the planted frame
	di
	mov r6,sp
	st r6,(r22+48)		; sp on arrival (should be oframe+8 = r0+0x5000)
	mov r6,sr
	st r6,(r22+52)		; sr on arrival
	st r9,(r22+56)		; delivery count so far (1)
	; confirm delivery still works after the switch: 3 more
	mov r21,#0
	mov r5,#0
	mov r18,r22
	add r18,#60
hsw_loop:
	mov r9,#0
	mov r6,sp
	st r6,(r0+0xf14)
	mov sp,r20
	mov r15,#0x80
	st r15,(r13+0x48)
	ei
	.rept 64
	nop
	.endr
	di
	ld r6,(r0+0xf14)
	mov sp,r6
	st r9,(r18)
	add r18,#4
	add r5,#1
	cmp r5,#3
	blt hsw_loop

hsw_done:
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

ph71:
	mov r17,#0x7e002050
	mov r18,#0x80
	st r18,(r17)
	add r9,#1
	cmp r21,#5
	beq phsw_switch
	rti
phsw_switch:
	mov sp,r19
	rti
