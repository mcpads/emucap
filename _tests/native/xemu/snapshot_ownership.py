#!/usr/bin/env python3
"""Run native NV2A payload ownership around success and decoder/encoder failures."""
import argparse
from pathlib import Path
import subprocess
import tempfile
ROOT=Path(__file__).resolve().parents[3]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,default=ROOT/'adapters/xemu/work/xemu/hw/xbox/nv2a/nv2a.c')
a=p.parse_args();source=a.source.read_text()
def function(name):
    start=source.index('static int '+name+'(')
    return source[start:source.index('\n}',start)+2]
callbacks='\n'.join(function(n) for n in ('nv2a_pre_save','nv2a_post_save','nv2a_pre_load','nv2a_payload_get','nv2a_payload_put'))
preamble=r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <errno.h>
typedef struct {int value;} NV2AState;
typedef struct {int error;} QEMUFile;
typedef struct {int code;} Error;
typedef int VMStateField;
typedef int JSONWriter;
static int vmstate_nv2a_payload;
static bool locked,emit_error;
static unsigned releases;
static int outcome;
static Error failure;
static void nv2a_lock_fifo(NV2AState *d) {(void)d;assert(!locked);locked=true;}
static void nv2a_unlock_fifo(NV2AState *d) {(void)d;assert(locked);locked=false;releases++;}
static int vmstate_load_state(QEMUFile *f,const int *desc,void *d,int version,Error **err) {
    (void)f;(void)d;assert(locked&&desc==&vmstate_nv2a_payload&&version==4);
    if(emit_error)*err=&failure;return outcome;
}
static int vmstate_save_state(QEMUFile *f,const int *desc,void *d,JSONWriter *j,Error **err) {
    (void)j;return vmstate_load_state(f,desc,d,4,err);
}
static void qemu_file_set_error_obj(QEMUFile *f,int code,Error *err) {assert(!locked&&err==&failure);f->error=code;}
'''
cases=r'''
int main(void) {
    NV2AState d={0};int results[]={0,-EINVAL,-EIO,-ENOMEM};
    for(unsigned i=0;i<4;i++)for(unsigned error=0;error<2;error++) {
        outcome=results[i];emit_error=error;
        for(unsigned repeat=0;repeat<3;repeat++) {
            QEMUFile f={0};releases=0;
            assert(nv2a_payload_get(&f,&d,sizeof(d),NULL)==outcome);
            assert(!locked&&releases==1);
            if(error)assert(f.error==(outcome<0?outcome:-EINVAL));
            releases=0;f.error=0;
            assert(nv2a_payload_put(&f,&d,sizeof(d),NULL,NULL)==outcome);
            assert(!locked&&releases==1);
            if(error)assert(f.error==(outcome<0?outcome:-EINVAL));
        }
    }
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='xemu-snapshot-ownership-') as directory:
    d=Path(directory);(d/'test.c').write_text(preamble+callbacks+cases)
    subprocess.run(['cc','-std=c11','-O2','-Wall','-Wextra','-Werror','-Wno-unused-parameter','-Wno-misleading-indentation',str(d/'test.c'),'-o',str(d/'test')],check=True)
    subprocess.run([str(d/'test')],check=True)
print('NV2A payload ownership releases exactly once on success, decoding/encoding failure and retry')
