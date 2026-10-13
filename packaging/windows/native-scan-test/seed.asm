EXTERN ?init_scan_orders@@YAXXZ:PROC
PUBLIC seed_init
.code
seed_init PROC FRAME
  sub rsp,28h
  .allocstack 28h
  .endprolog
  ; After CALL, callee entry RSP is eight bytes lower. Its entry+10h is our RSP+8.
  mov dword ptr [rsp+8],ecx
  call ?init_scan_orders@@YAXXZ
  add rsp,28h
  ret
seed_init ENDP
END

