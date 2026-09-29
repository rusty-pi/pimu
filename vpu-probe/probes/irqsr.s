	.text
	.global _start
_start:
	mov r1,sr
	st r1,(r0)			; entry sr
	; set flags via cmp, read sr (does sr expose NZCV?)
	mov r2,#0
	cmp r2,#0			; Z=1,N=0
	mov r3,sr
	st r3,(r0+4)			; sr after eq
	mov r2,#0
	cmp r2,#1			; N=1,Z=0 (0-1<0)
	mov r3,sr
	st r3,(r0+8)			; sr after neg
	; try mov sr to set Z bit (bit3) then test with beq
	mov r3,r1
	and r3,#-16
	or r3,#8			; low nibble = 8 (Z per model)
	mov r2,#5
	cmp r2,#0			; make NZ: 5-0 -> Z=0,N=0
	mov sr,r3			; write nibble 8
	mov r4,#0
	beq 1f
	mov r4,#0
	b 2f
1:	mov r4,#1
2:	st r4,(r0+12)			; 1 if mov sr forced Z (beq taken)
	mov r3,sr
	st r3,(r0+16)			; sr readback after mov sr nibble8
	; try to SET supervisor bit 29 via mov sr
	mov r3,r1
	or r3,#0x20000000
	mov sr,r3
	mov r3,sr
	st r3,(r0+20)			; sr after or bit29
	; restore original sr fully
	mov sr,r1
	mov r3,sr
	st r3,(r0+24)			; sr after restore
	; try clear IE via mov sr (bit30) then read
	mov r3,r1
	mov r5,#0x40000000
	bic r3,r5
	mov sr,r3
	mov r3,sr
	st r3,(r0+28)			; sr after clearing IE via mov sr
	mov sr,r1			; restore
	mov r3,sr
	st r3,(r0+32)
	mov r2,#0x5a5aa5a5
	st r2,(r0+4092)
	rts
