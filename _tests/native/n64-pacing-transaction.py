#!/usr/bin/env python3
"""Exercise maintained N64 owner/ABI methods, concurrent writers and callback reentry."""
import argparse
import os
from pathlib import Path
import shlex
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, required=True, help='patched core src directory')
parser.add_argument('--sanitizer', choices=['address,undefined', 'thread'], default='address,undefined')
a = parser.parse_args()
main = (a.source/'main/main.c').read_text()
frontend = (a.source/'api/frontend.c').read_text()
header = (a.source/'api/m64p_emucap_pacing.h').read_text()
owner = main[main.index('/* Native owner:'):main.index('static int main_is_paused(')]
def body(text, signature):
    start = text.index(signature); i=text.index('{', start)+1; depth=1
    while depth:
        depth += (text[i]=='{')-(text[i]=='}'); i+=1
    return text[start:i]
setup = r'''
#define SDL_MAIN_HANDLED
#include <SDL.h>
#include <pthread.h>
#include <stdatomic.h>
#include <assert.h>
#include <stdio.h>
#define EXPORT
#define CALL
#define M64CORE_SPEED_FACTOR 4
#define M64MSG_STATUS 1
#define OSD_BOTTOM_LEFT 0
#define OSD_TOP_RIGHT 1
#define M64ERR_SUCCESS 0
#define M64ERR_INPUT_INVALID 1
#define M64ERR_NOT_INIT 2
#define M64ERR_INCOMPATIBLE 3
typedef int m64p_error;
static int l_CoreInit=1;
static void* l_msgFF;
static atomic_int reenter, callbacks;
static int audio_factor, audio_calls;
m64p_emucap_policy main_pacing_read(void);
static void main_speedset(int percent);
static void* observer(void* unused) { (void)main_pacing_read(); return NULL; }
static void StateChanged(int field, int factor) {
    atomic_fetch_add(&callbacks,1);
    int action=atomic_exchange(&reenter,0);
    if (action==1) {
        /* Another thread must acquire the owner while the callback waits. */
        pthread_t reader; assert(!pthread_create(&reader,NULL,observer,NULL));
        assert(!pthread_join(reader,NULL));
    }
    if (action==2) main_speedset(400);
}
static void main_message(int level,int corner,const char* format,...) {}
static void main_toggle_pause(void) { (void)main_pacing_read(); }
static void set_audio(int factor) { audio_factor=factor; ++audio_calls; (void)main_pacing_read(); }
static struct { void (*setSpeedFactor)(int); } audio={set_audio};
static void* osd_new_message(int corner,const char* message) {return (void*)1;}
static void osd_message_set_static(void* m) {}
static void osd_message_set_user_managed(void* m) {}
static void osd_delete_message(void* m) {}
'''
checks = r'''
static m64p_emucap_pacing_result apply(int percent) {
    m64p_emucap_pacing_result r={0};
    assert(CoreEmucapPacing(1,1,percent,&r,sizeof r)==0);
    assert(r.version==1);
    if (r.outcome) { assert(r.previous.netplay); assert(!memcmp(&r.previous,&r.applied,sizeof r.previous)); }
    else {
        assert(r.applied.limiter==(percent!=0));
        assert(!r.applied.fast_forward && !r.applied.netplay);
        if(percent) assert(r.applied.factor==percent);
        int changed=r.previous.factor!=r.applied.factor || r.previous.limiter!=r.applied.limiter || r.previous.fast_forward!=r.applied.fast_forward;
        assert(r.applied.revision==r.previous.revision+(uint64_t)changed);
    }
    return r;
}
static void* agent(void* unused) {
    for(int i=0;i<10000;++i) { apply(i%2 ? 50:1000); m64p_emucap_policy p=main_pacing_read(); assert(p.factor>=1 && p.factor<=1000); }
    return NULL;
}
static void* native_writer(void* unused) {
    for(int i=0;i<10000;++i) {
        main_speedset(100); main_speedup(20); main_speeddown(20);
        main_set_fastforward(1); main_set_fastforward(0); main_set_speedlimiter(0); main_set_speedlimiter(1);
        main_pacing_set_netplay(1); main_pacing_netplay_limiter(1); main_pacing_netplay_limiter(0); main_pacing_set_netplay(0);
    }
    return NULL;
}
int main(void) {
    assert(main_pacing_init());
    m64p_emucap_policy initial=main_pacing_read();
    m64p_emucap_pacing_result r;
    assert(CoreEmucapPacing(2,1,50,&r,sizeof r)==M64ERR_INCOMPATIBLE);
    assert(CoreEmucapPacing(1,1,50,&r,sizeof r-1)==M64ERR_INPUT_INVALID);
    assert(CoreEmucapPacing(1,1,50,NULL,sizeof r)==M64ERR_INPUT_INVALID);
    assert(CoreEmucapPacing(1,2,50,&r,sizeof r)==M64ERR_INPUT_INVALID);
    assert(CoreEmucapPacing(1,1,1001,&r,sizeof r)==M64ERR_INPUT_INVALID);
    assert(CoreEmucapPacing(1,1,-1,&r,sizeof r)==M64ERR_INPUT_INVALID);
    assert(main_pacing_read().revision==initial.revision);
    atomic_store(&reenter,1); r=apply(50); assert(r.previous.factor==100);
    assert(audio_calls==0); main_pacing_service(); assert(audio_calls==1 && audio_factor==50);
    assert(apply(50).previous.revision==r.applied.revision);
    atomic_store(&reenter,2); r=apply(100);
    assert(r.applied.factor==100 && main_pacing_read().factor==400);
    assert(r.applied.revision+1==main_pacing_read().revision);
    main_set_fastforward(1); r=apply(200); assert(r.previous.fast_forward && r.previous.factor==250);
    main_set_fastforward(0); assert(main_pacing_read().factor==200);
    main_set_fastforward(1); r=apply(0); assert(!r.applied.limiter && r.applied.factor==200);
    main_set_fastforward(0); assert(!main_pacing_read().limiter);
    apply(1); apply(1000);
    main_speedlimiter_toggle(); assert(!main_pacing_read().limiter);
    main_speedlimiter_toggle(); assert(main_pacing_read().limiter);
    main_pacing_set_netplay(1); r=apply(50); assert(r.outcome==1);
    main_set_fastforward(1); assert(!main_pacing_read().fast_forward);
    main_pacing_netplay_limiter(1); assert(main_pacing_read().netplay_lag && !main_pacing_read().limiter);
    main_pacing_netplay_limiter(0); assert(!main_pacing_read().netplay_lag && main_pacing_read().limiter);
    main_pacing_set_netplay(0);
    SDL_LockMutex(l_PacingMutex); l_Pacing.revision=UINT64_MAX; SDL_UnlockMutex(l_PacingMutex);
    r=apply(50); assert(r.applied.revision==0);
    pthread_t writer,requester;
    assert(!pthread_create(&writer,NULL,native_writer,NULL));
    assert(!pthread_create(&requester,NULL,agent,NULL));
    assert(!pthread_join(writer,NULL)); assert(!pthread_join(requester,NULL));
    assert(CoreEmucapPacing(1,0,0,&r,sizeof r)==0);
    assert(!memcmp(&r.previous,&r.applied,sizeof r.previous));
    /* Same owner survives a front-session replacement: query the actual native
       writer value rather than replaying an earlier agent request (50). */
    apply(50);
    main_speedset(375);
    assert(CoreEmucapPacing(1,0,0,&r,sizeof r)==0);
    assert(r.applied.factor==375 && r.previous.factor==375);
    assert(!memcmp(&r.previous,&r.applied,sizeof r.previous));
    /* A new core generation has launch defaults, not the prior generation's
       policy, fast-forward restoration value, or revision. */
    main_set_fastforward(1);
    main_pacing_set_netplay(1);
    main_pacing_shutdown();
    assert(main_pacing_init());
    assert(CoreEmucapPacing(1,0,0,&r,sizeof r)==0);
    assert(r.applied.factor==100 && r.applied.limiter==1);
    assert(!r.applied.fast_forward && !r.applied.netplay && !r.applied.netplay_lag);
    assert(r.applied.revision==initial.revision);
    main_set_fastforward(1); main_set_fastforward(0);
    assert(main_pacing_read().factor==100);
    main_pacing_shutdown();
    puts("PASS N64 native owner: coherent query/apply, all policy writers, netplay, FF release, reentry, deferred audio and concurrency");
}
'''
with tempfile.TemporaryDirectory(prefix='n64-pacing-owner-') as temp:
    source=Path(temp)/'check.c'; source.write_text(header+setup+owner+body(frontend,'EXPORT m64p_error CALL CoreEmucapPacing(')+checks)
    flags=shlex.split(subprocess.check_output(['pkg-config','--cflags','--libs','sdl2'],text=True))
    executable=Path(temp)/'check'
    subprocess.run(['cc','-std=c11','-g','-O1','-pthread',f'-fsanitize={a.sanitizer}','-fno-sanitize-recover=all',str(source),*flags,'-o',str(executable)],check=True)
    env = dict(os.environ)
    libdir = subprocess.check_output(['pkg-config', '--variable=libdir', 'sdl2'], text=True).strip()
    env['DYLD_LIBRARY_PATH'] = libdir + (':' + env['DYLD_LIBRARY_PATH'] if env.get('DYLD_LIBRARY_PATH') else '')
    subprocess.run([str(executable)],check=True,timeout=30,env=env)
