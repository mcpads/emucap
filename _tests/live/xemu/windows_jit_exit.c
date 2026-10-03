#include <windows.h>
#include <stdint.h>
#include <setjmp.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
void qemu_win64_longjmp(jmp_buf, int) __attribute__((noreturn));
int test_jump_restore(void *code);
void *saved_setjmp;
int test_jump_value;
_Static_assert(offsetof(_JUMP_BUFFER, Rsp)==16,"jmp_buf Rsp layout");
_Static_assert(offsetof(_JUMP_BUFFER, Rip)==80,"jmp_buf Rip layout");
_Static_assert(offsetof(_JUMP_BUFFER, Xmm15)==240,"jmp_buf vector layout");
int main(int argc, char **argv) {
 (void)argv;
 SetErrorMode(SEM_NOGPFAULTERRORBOX|SEM_FAILCRITICALERRORS);
 HMODULE crt=LoadLibraryA("msvcrt.dll");
 saved_setjmp=(void *)GetProcAddress(crt,"_setjmp");
 void *exit_jump=argc>1?(void *)GetProcAddress(crt,"longjmp"):(void *)&qemu_win64_longjmp;
 if(!saved_setjmp||!exit_jump)return 4;
 printf("CRT=msvcrt.dll mode=%s\n",argc>1?"baseline":"nonunwinding");fflush(stdout);
 unsigned char code[512];size_t n=0;
 #define B(x) code[n++]=(x)
 B(0x48);B(0x83);B(0xec);B(0x28);
 const int regs[]={3,5,6,7,12,13,14,15};
 for(size_t i=0;i<8;i++){B(regs[i]>=8?0x49:0x48);B(0xb8+(regs[i]&7));memset(code+n,0,8);n+=8;}
 for(int x=6;x<16;x++){B(0x66);if(x>=8)B(0x45);B(0x0f);B(0x57);B(0xc0|((x&7)<<3)|(x&7));}

 B(0x48);B(0xb8);uintptr_t fn=(uintptr_t)exit_jump;memcpy(code+n,&fn,8);n+=8;B(0xff);B(0xd0);B(0xcc);
 void *p=VirtualAlloc(NULL,4096,MEM_COMMIT|MEM_RESERVE,PAGE_READWRITE);if(!p)return 2;
 memcpy(p,code,n);DWORD old;if(!VirtualProtect(p,4096,PAGE_EXECUTE_READ,&old))return 3;FlushInstructionCache(GetCurrentProcess(),p,n);
 int result=0;const int values[]={0,1,2,-2,-3};for(size_t v=0;v<5;v++){test_jump_value=values[v];for(int i=0;i<1000;i++)if(test_jump_restore(p)){result=1;break;}}
 VirtualFree(p,0,MEM_RELEASE);printf("5000 JIT exits with 0, 1, 2, -2, -3; all 8 nonvolatile GPRs and XMM6-XMM15 restored: %s\n",result?"FAIL":"PASS");return result;
}
