#!/usr/bin/env python3
"""Exercise the maintained QMP transaction and policy setter under their BQL precondition."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
NATIVE = ROOT / 'adapters/xemu/work/xemu/ui'

def function(source, signature):
    start = source.index(signature)
    brace = source.index('{', start)
    end, depth = brace + 1, 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]

pacing = (NATIVE / 'xemu-emucap-pacing.c').read_text()
qmp = (NATIVE / 'xemu-emucap.c').read_text()
code = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
typedef int Error;
typedef struct {uint32_t percent,previous_percent,transaction_version;
 uint64_t revision,previous_revision,frame_boundary,start_frame_boundary;int clock_profile;} XemuEmucapPacing;
static struct {uint32_t percent;uint64_t revision;void *wake;} pacing={.percent=100};
static bool supported=true,mttcg=false;
static int anchors=0,wakes=0,errors=0;
static pthread_mutex_t bql=PTHREAD_MUTEX_INITIALIZER;
static _Thread_local bool held;
static bool bql_locked(void){return held;}
static void lock(void){pthread_mutex_lock(&bql);held=true;}
static void unlock(void){held=false;pthread_mutex_unlock(&bql);}
static bool xemu_emucap_clock_supported(void){return supported;}
static bool qemu_tcg_mttcg_enabled(void){return mttcg;}
static uint64_t current_frame_boundary(void){return 42;}
static int clock_profile(void){return 3;}
static void init(void){assert(held);}
static void anchor(void){assert(held);++anchors;}
static void timer_del(void *t){assert(held);(void)t;}
static void wake(void *p){assert(held);(void)p;++wakes;}
#define g_new0(type,n) ((type*)calloc(n,sizeof(type)))
#define error_setg(...) (++errors)
''' + '\n'.join(function(pacing, signature) for signature in [
    'bool xemu_emucap_pacing_set(', 'uint32_t xemu_emucap_pacing_percent(',
    'uint64_t xemu_emucap_pacing_revision(']) + '\n' + function(qmp, 'XemuEmucapPacing *qmp_xemu_emucap_pacing(') + r'''
static XemuEmucapPacing *set(uint32_t percent){return qmp_xemu_emucap_pacing(percent!=0,percent,percent==0,percent==0,NULL);}
static void *writer(void *opaque){
 uint32_t percent=(uintptr_t)opaque;
 for(int i=0;i<200;i++){
  lock();uint32_t before=pacing.percent;uint64_t revision=pacing.revision;
  XemuEmucapPacing *r=set(percent);assert(r && r->transaction_version==1);
  assert(r->previous_percent==before && r->previous_revision==revision);
  assert(r->percent==percent && r->revision==revision+(percent!=before));
  assert(r->frame_boundary==42 && r->start_frame_boundary==42);
  free(r);unlock();
 }
 return NULL;
}
int main(void){
 lock();
 XemuEmucapPacing *probe=qmp_xemu_emucap_pacing(false,0,false,false,NULL);
 assert(probe && probe->percent==100 && probe->previous_percent==100 && anchors==0);
 const char *error=NULL;assert(xemu_emucap_pacing_set(false,250,&error));
 XemuEmucapPacing *r=set(50);assert(r->previous_percent==250 && r->percent==50);free(r);free(probe);
 uint32_t before=pacing.percent;uint64_t revision=pacing.revision;int old_anchors=anchors;
 assert(!set(1001));assert(!qmp_xemu_emucap_pacing(true,50,true,true,NULL));
 assert(!qmp_xemu_emucap_pacing(false,0,true,false,NULL));
 supported=false;assert(!set(50));supported=true;mttcg=true;assert(!set(50));mttcg=false;
 assert(pacing.percent==before && pacing.revision==revision && anchors==old_anchors && errors==5);
 for(unsigned i=0;i<4;i++){
  uint32_t rates[]={0,1,100,1000};r=set(rates[i]);assert(r && r->percent==rates[i]);free(r);
  r=set(rates[i]);assert(r && r->revision==r->previous_revision);free(r);
 }
 unlock();
 pthread_t a,b;assert(!pthread_create(&a,NULL,writer,(void*)(uintptr_t)50));
 assert(!pthread_create(&b,NULL,writer,(void*)(uintptr_t)400));
 pthread_join(a,NULL);pthread_join(b,NULL);
 puts("xemu native transaction: BQL writers, native previous, revisions, no-mutation rejection and limits passed");
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-pacing-transaction-') as directory:
    temp = Path(directory)
    (temp / 'test.c').write_text(code)
    subprocess.run(['clang', '-std=c11', '-pthread', '-fsanitize=address,undefined', '-g',
                    str(temp / 'test.c'), '-o', str(temp / 'test')], check=True)
    subprocess.run([str(temp / 'test')], check=True)
